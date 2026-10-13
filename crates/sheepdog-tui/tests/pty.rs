#![cfg(target_os = "linux")]
use sheepdog_client::Client;
use sheepdog_core::{Geometry, TabId, TerminalState};
use sheepdog_proto::{Request, Response};
use sheepdog_server::{Daemon, bootstrap};
use std::{
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{fs::PermissionsExt, process::CommandExt},
    },
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::Arc,
    time::Duration,
};
use tokio::sync::Notify;

struct Harness {
    directory: PathBuf,
    stop: Arc<Notify>,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
    client: Arc<Client>,
}
impl Harness {
    async fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "sheepdog-tui-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let daemon = Daemon::bind(
            "127.0.0.1:0".parse().unwrap(),
            directory.clone(),
            "/bin/sh".into(),
        )
        .unwrap();
        let stop = Arc::new(Notify::new());
        let task = tokio::spawn(daemon.serve(stop.clone()));
        let client = Client::connect(
            "127.0.0.1".into(),
            bootstrap(directory.clone()).await.unwrap(),
        )
        .await
        .unwrap();
        // Mock only the SSH command. The actual private bootstrap socket,
        // credential issuance, certificate pins, WT, PTYs and TUI are real.
        let script = directory.join("bootstrap.cjs");
        std::fs::write(&script,"const c=require('node:net').createConnection(process.argv[2]);c.on('connect',()=>c.write('bootstrap\\n'));c.on('data',b=>process.stdout.write(b));c.on('error',()=>process.exit(1));").unwrap();
        let ssh = directory.join("mock-ssh");
        std::fs::write(&ssh,format!("#!/bin/sh\nif [ \"$1\" = -G ]; then printf 'hostname 127.0.0.1\\n'; else exec node '{}' '{}'; fi\n",script.display(),directory.join("server.sock").display())).unwrap();
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            directory,
            stop,
            task,
            client,
        }
    }
    async fn tabs(&self) -> Vec<sheepdog_core::Tab> {
        let Response::Snapshot { tabs, .. } = self.client.request(Request::Snapshot).await.unwrap()
        else {
            panic!("snapshot");
        };
        tabs
    }
    async fn seed(&self) -> (TabId, TabId) {
        let Response::Space(space) = self
            .client
            .request(Request::CreateSpace {
                name: "Work".into(),
                directory: std::env::temp_dir(),
            })
            .await
            .unwrap()
        else {
            panic!("space");
        };
        let mut ids = vec![];
        for label in ["ONE", "TWO"] {
            let Response::Tab(tab) = self
                .client
                .request(Request::CreateTab {
                    space: space.id,
                    label: label.into(),
                    geometry: Geometry::new(80, 21).unwrap(),
                })
                .await
                .unwrap()
            else {
                panic!("tab");
            };
            ids.push(tab.id);
        }
        (ids[0], ids[1])
    }
    fn gui(&self) -> Gui {
        Gui::new(&self.directory)
    }
    async fn close(self) {
        self.stop.notify_one();
        self.task.await.unwrap().unwrap();
        std::fs::remove_dir_all(self.directory).unwrap();
    }
}
struct Gui {
    child: Child,
    master: std::fs::File,
    slave: std::fs::File,
    emulator: quosh_terminal::Emulator,
    initial: libc::termios,
}
impl Gui {
    fn new(directory: &std::path::Path) -> Self {
        let pty = quosh_pty::open_cloexec(80, 24).unwrap();
        let mut initial = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::tcgetattr(pty.slave.as_raw_fd(), &mut initial) },
            0
        );
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_sheepdog"));
        cmd.args([
            "--ssh",
            directory.join("mock-ssh").to_str().unwrap(),
            "fixture",
        ]);
        cmd.stdin(Stdio::from(pty.slave.try_clone().unwrap()))
            .stdout(Stdio::from(pty.slave.try_clone().unwrap()))
            .stderr(Stdio::from(pty.slave.try_clone().unwrap()))
            .env("TERM", "xterm-256color");
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn().unwrap();
        let flags = unsafe { libc::fcntl(pty.master.as_raw_fd(), libc::F_GETFL) };
        assert_eq!(
            unsafe {
                libc::fcntl(
                    pty.master.as_raw_fd(),
                    libc::F_SETFL,
                    flags | libc::O_NONBLOCK,
                )
            },
            0
        );
        Self {
            child,
            master: pty.master,
            slave: pty.slave,
            emulator: quosh_terminal::Emulator::new(24, 80, 0),
            initial,
        }
    }
    fn drain(&mut self) {
        let mut buf = [0; 16384];
        loop {
            match self.master.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    self.emulator.process(&buf[..n]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.raw_os_error() == Some(libc::EIO) => break,
                Err(e) => panic!("PTY read {e}"),
            }
        }
    }
    fn text(&mut self) -> String {
        self.drain();
        let frame = self.emulator.snapshot();
        let mut out = String::new();
        for row in 0..frame.rows() {
            for col in 0..frame.cols() {
                out.push_str(frame.cell_content(row, col));
            }
            out.push('\n');
        }
        out
    }
    async fn wait(&mut self, needle: &str) {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if self.text().contains(needle) {
                    break;
                }
                assert!(
                    self.child.try_wait().unwrap().is_none(),
                    "GUI exited: {:?}",
                    self.text()
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("missing {needle:?}: {:?}", self.text()));
    }
    async fn keys(&mut self, keys: &[u8]) {
        for &byte in keys {
            self.master.write_all(&[byte]).unwrap();
            tokio::time::sleep(Duration::from_millis(3)).await;
        }
    }
    async fn click(&mut self, x: u16, y: u16) {
        self.master
            .write_all(
                format!("\x1b[<0;{};{}M\x1b[<0;{};{}m", x + 1, y + 1, x + 1, y + 1).as_bytes(),
            )
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    async fn paste(&mut self, text: &[u8]) {
        // A terminal sends each CSI delimiter atomically. Delaying between
        // ESC and '[' models standalone Escape keypresses, not a paste.
        let mut bytes = b"\x1b[200~".to_vec();
        bytes.extend_from_slice(text);
        bytes.extend_from_slice(b"\x1b[201~");
        self.master.write_all(&bytes).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    async fn prefix(&mut self, key: u8) {
        self.keys(&[2, key]).await;
    }
    async fn quit(&mut self) {
        assert!(
            self.child.try_wait().unwrap().is_none(),
            "GUI exited before explicit quit"
        );
        self.prefix(b'q').await;
        tokio::time::timeout(Duration::from_secs(3), async {
            while self.child.try_wait().unwrap().is_none() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("quit blocked by TTY/network backpressure");
        assert!(
            self.child.try_wait().unwrap().unwrap().success(),
            "GUI quit failed"
        );
        let mut after = unsafe { std::mem::zeroed::<libc::termios>() };
        assert_eq!(
            unsafe { libc::tcgetattr(self.slave.as_raw_fd(), &mut after) },
            0
        );
        assert_eq!(
            after.c_lflag & (libc::ICANON | libc::ECHO),
            self.initial.c_lflag & (libc::ICANON | libc::ECHO)
        );
    }
}
impl Drop for Gui {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            unsafe {
                libc::kill(-(self.child.id() as i32), libc::SIGKILL);
            };
            let _ = self.child.wait();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_tui_navigates_without_destroying_work_and_quit_restores_local_tty() {
    let harness = Harness::new().await;
    let (one, two) = harness.seed().await;
    let mut gui = harness.gui();
    gui.wait("1:ONE").await;
    gui.wait("Connected").await;
    gui.keys(b"printf '\\124\\125\\111\\137\\117\\116\\105\\n'\r")
        .await;
    gui.wait("TUI_ONE").await;
    assert!(
        gui.text().contains("Spaces") && gui.text().contains("Terminals / Agents"),
        "multi-row terminal output must not overwrite the sidebar"
    );
    gui.click(35, 0).await;
    gui.wait("[2:TWO]").await;
    gui.wait("Connected").await;
    gui.keys(b"printf '\\124\\125\\111\\137\\124\\127\\117\\n'\r")
        .await;
    gui.wait("TUI_TWO").await;
    gui.prefix(b'p').await;
    gui.wait("TUI_ONE").await;
    gui.wait("Connected").await;
    assert!(
        !gui.text().contains("TUI_TWO"),
        "only selected terminal is visible"
    );
    gui.prefix(b'n').await;
    gui.wait("TUI_TWO").await;
    gui.wait("Connected").await;
    gui.prefix(b'p').await;
    gui.wait("TUI_ONE").await;
    gui.wait("Connected").await;
    gui.prefix(b'c').await;
    gui.wait("[3:Shell]").await;
    gui.wait("Connected").await;
    assert_eq!(harness.tabs().await.len(), 3);
    gui.prefix(b'g').await;
    gui.wait("Goto").await;
    gui.keys(b"/ONE\r").await;
    gui.wait("TUI_ONE").await;
    gui.quit().await;
    let tabs = harness.tabs().await;
    assert!(
        tabs.iter()
            .all(|t| matches!(t.terminal, TerminalState::Running(_)))
    );
    assert!(tabs.iter().any(|t| t.id == one));
    assert!(tabs.iter().any(|t| t.id == two));
    harness.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restored_tabs_explain_restart_and_require_explicit_new_shell() {
    let mut harness = Harness::new().await;
    let (one, _) = harness.seed().await;
    harness.stop.notify_one();
    harness.task.await.unwrap().unwrap();
    let daemon = Daemon::bind(
        "127.0.0.1:0".parse().unwrap(),
        harness.directory.clone(),
        "/bin/sh".into(),
    )
    .unwrap();
    harness.stop = Arc::new(Notify::new());
    harness.task = tokio::spawn(daemon.serve(harness.stop.clone()));
    harness.client = Client::connect(
        "127.0.0.1".into(),
        bootstrap(harness.directory.clone()).await.unwrap(),
    )
    .await
    .unwrap();
    let mut gui = harness.gui();
    gui.wait("Server restarted;").await;
    assert!(
        harness
            .tabs()
            .await
            .iter()
            .all(|t| matches!(t.terminal, TerminalState::Ended(_)))
    );
    gui.keys(b"\r").await;
    assert!(
        harness
            .tabs()
            .await
            .iter()
            .all(|t| matches!(t.terminal, TerminalState::Ended(_))),
        "ordinary Enter cannot launch work"
    );
    gui.prefix(b'\r').await;
    gui.wait("Connected").await;
    let tabs = harness.tabs().await;
    assert!(matches!(
        tabs.iter().find(|t| t.id == one).unwrap().terminal,
        TerminalState::Running(_)
    ));
    assert!(matches!(
        tabs.iter().find(|t| t.id != one).unwrap().terminal,
        TerminalState::Ended(_)
    ));
    gui.quit().await;
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fresh_server_starts_a_shell_and_mouse_creates_and_renames_work_without_a_wizard() {
    let harness = Harness::new().await;
    let mut gui = harness.gui();
    gui.wait("[1:Shell]").await;
    gui.wait("Connected").await;
    assert_eq!(
        harness.tabs().await.len(),
        1,
        "fresh startup opens a shell without any keys or wizard"
    );
    gui.keys(b"printf '\\106\\111\\122\\123\\124\\137\\123\\110\\105\\114\\114\\n'\r")
        .await;
    gui.wait("FIRST_SHELL").await;
    // Sidebar + New Space is row 11 at 80x24. Top-bar + is at column 76.
    gui.click(2, 11).await;
    gui.wait("Connected").await;
    tokio::time::timeout(Duration::from_secs(3), async {
        while harness.tabs().await.len() != 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while gui.text().matches("/ Shell").count() < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("new Space must be painted before acting on its Tab bar");
    gui.wait("[1:Shell]").await;
    gui.wait("Connected").await;
    gui.click(77, 0).await;
    gui.wait("[2:Shell]").await;
    gui.wait("Connected").await;
    assert_eq!(harness.tabs().await.len(), 3);
    gui.prefix(b'r').await;
    gui.wait("Rename Space").await;
    gui.keys(b"\x01\x0b").await;
    gui.keys("草原\r".as_bytes()).await;
    gui.wait("草原").await;
    gui.prefix(b',').await;
    gui.wait("Rename Tab").await;
    gui.keys(b"\x01\x0bTail\r").await;
    gui.wait("[2:Tail]").await;
    // Mouse-only Space switching returns the independently retained first shell.
    gui.click(2, 1).await;
    gui.wait("FIRST_SHELL").await;
    gui.wait("Connected").await;
    gui.paste(b"echo NOT_INGESTED\n").await;
    gui.wait("File Ingest not built").await;
    assert!(!gui.text().contains("NOT_INGESTED"));
    gui.prefix(b'P').await;
    gui.paste(b"printf '\\114\\111\\124\\105\\122\\101\\114\\137\\104\\117\\116\\105\\n'\n")
        .await;
    gui.wait("LITERAL_DONE").await;
    gui.paste(b"echo STILL_GATED\n").await;
    gui.wait("File Ingest not built").await;
    assert!(!gui.text().contains("STILL_GATED"));
    gui.quit().await;
    assert!(
        harness
            .tabs()
            .await
            .iter()
            .all(|t| matches!(t.terminal, TerminalState::Running(_)))
    );
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_requires_confirmation_and_cancellation_preserves_the_terminal() {
    let harness = Harness::new().await;
    let (one, two) = harness.seed().await;
    let mut gui = harness.gui();
    gui.wait("Connected").await;
    gui.prefix(b'n').await;
    gui.wait("[2:TWO]").await;
    gui.wait("Connected").await;
    gui.prefix(b'p').await;
    gui.wait("[1:ONE]").await;
    gui.wait("Connected").await;
    gui.prefix(b'X').await;
    gui.wait("Close Tab ONE?").await;
    gui.keys(b"n").await;
    assert_eq!(harness.tabs().await.len(), 2);
    gui.prefix(b'X').await;
    gui.wait("Close Tab ONE?").await;
    gui.keys(b"y").await;
    gui.wait("[1:TWO]").await;
    let tabs = harness.tabs().await;
    assert_eq!(tabs.len(), 1);
    assert_eq!(tabs[0].id, two);
    assert!(!tabs.iter().any(|t| t.id == one));
    gui.quit().await;
    assert!(matches!(
        harness.tabs().await[0].terminal,
        TerminalState::Running(_)
    ));
    harness.close().await;
}
struct Job(i32);
impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            libc::kill(self.0, libc::SIGKILL);
        }
    }
}
fn running(pid: i32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .is_some_and(|stat| {
            stat.rsplit_once(')')
                .is_some_and(|(_, fields)| fields.split_whitespace().next() != Some("Z"))
        })
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn confirmed_close_terminates_a_foreground_job_in_an_independent_process_group() {
    let harness = Harness::new().await;
    harness.seed().await;
    let mut gui = harness.gui();
    gui.wait("Connected").await;
    let file = harness.directory.join("foreground.pid");
    gui.keys(
        format!(
            "sh -c 'trap \"\" HUP TERM; echo $$ >{}; exec sleep 30'\r",
            file.display()
        )
        .as_bytes(),
    )
    .await;
    let job = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Ok(pid) = std::fs::read_to_string(&file)
                && let Ok(pid) = pid.trim().parse::<i32>()
            {
                break Job(pid);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_ne!(
        unsafe { libc::getpgid(job.0) },
        unsafe { libc::getsid(job.0) },
        "fixture must have real interactive shell job control"
    );
    gui.prefix(b'X').await;
    gui.wait("Close Tab ONE?").await;
    gui.keys(b"y").await;
    gui.wait("[1:TWO]").await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while running(job.0) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("foreground job did not stop");
    assert!(
        !running(job.0),
        "closing only the shell's process group leaves the foreground agent alive"
    );
    gui.quit().await;
    harness.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quit_is_prompt_with_an_unread_local_display_and_running_remote_output() {
    let harness = Harness::new().await;
    harness.seed().await;
    let mut gui = harness.gui();
    gui.wait("Connected").await;
    gui.keys(b"while :; do printf 'SPAM_OUTPUT\\n'; done\r")
        .await;
    gui.wait("SPAM_OUTPUT").await;
    // Do not drain the display. Repaints must hit local PTY backpressure.
    tokio::time::sleep(Duration::from_secs(8)).await;
    gui.quit().await;
    assert!(
        harness
            .tabs()
            .await
            .iter()
            .all(|t| matches!(t.terminal, TerminalState::Running(_)))
    );
    harness.close().await;
}
