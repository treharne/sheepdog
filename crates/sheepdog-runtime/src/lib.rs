//! Linux terminal owner: one PTY/emulator, many independently sequenced views.
//! No network listener or authentication is provided by this crate.

mod ledger;
pub use ledger::{AttachmentId, Checkpoint, Connection, InputError, InputResult, MAX_INPUT};

use anyhow::{Context, Result, bail};
use blit_remote::FrameState;
use ledger::Ledger;
use quosh_terminal::Emulator;
use sheepdog_core::Geometry;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::io::unix::AsyncFd;
use tokio::process::{Child, Command};
use tokio::sync::{Notify, mpsc, oneshot, watch};

const COMMAND_CAP: usize = 32;

#[derive(Clone, Debug)]
pub struct Program {
    pub executable: PathBuf,
    pub args: Vec<OsString>,
    pub directory: PathBuf,
    pub geometry: Geometry,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exit {
    pub code: Option<i32>,
    pub signal: Option<i32>,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub version: u64,
    pub frame: Arc<FrameState>,
    pub geometry: Geometry,
    pub checkpoints: BTreeMap<AttachmentId, Checkpoint>,
    pub exit: Option<Exit>,
}

impl Snapshot {
    /// This encoding preserves causal per-client echo acknowledgments. It does
    /// NOT encode prediction invalidation metadata; network wiring must carry
    /// that before enabling prediction with these screens.
    pub fn screen(&self, connection: Connection) -> Option<quosh_proto::Screen> {
        let checkpoint = self.checkpoints.get(&connection.id)?;
        if !checkpoint.connected || checkpoint.connection_epoch != connection.epoch {
            return None;
        }
        Some(quosh_proto::Screen {
            version: self.version,
            echo_ack: checkpoint.echo_ack,
            frame: (*self.frame).clone(),
        })
    }
}

struct Stop {
    requested: AtomicBool,
    wake: Notify,
}

#[derive(Clone)]
pub struct Terminal {
    commands: mpsc::Sender<OwnerCommand>,
    snapshots: watch::Receiver<Arc<Snapshot>>,
    stop: Arc<Stop>,
}

type Reply<T> = oneshot::Sender<std::result::Result<T, InputError>>;

enum OwnerCommand {
    Attach(Reply<Connection>),
    Resume(Connection, Reply<Connection>),
    Detach(Connection, Reply<()>),
    Retire(Connection, Reply<()>),
    Focus(Connection, Reply<()>),
    Input(Connection, u64, Vec<u8>, Reply<InputResult>),
    Resize(Connection, Geometry, Reply<()>),
}

impl Terminal {
    /// Run as the intended Unix user. There is no implicit privilege change.
    pub fn spawn(program: Program) -> Result<Self> {
        let runtime = tokio::runtime::Handle::try_current()
            .context("terminal spawn requires a Tokio runtime")?;
        Geometry::new(program.geometry.cols, program.geometry.rows)
            .map_err(|_| anyhow::anyhow!("invalid terminal geometry"))?;
        let pty = quosh_pty::open_cloexec(program.geometry.cols, program.geometry.rows)?;
        let mut command = Command::new(&program.executable);
        command
            .args(&program.args)
            .current_dir(&program.directory)
            .env("TERM", "xterm-256color")
            .stdin(Stdio::from(quosh_pty::dup_cloexec(pty.slave.as_raw_fd())?))
            .stdout(Stdio::from(quosh_pty::dup_cloexec(pty.slave.as_raw_fd())?))
            .stderr(Stdio::from(quosh_pty::dup_cloexec(pty.slave.as_raw_fd())?))
            .kill_on_drop(true);
        // Only async-signal-safe syscalls run between fork and exec.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        set_nonblocking(&pty.master)?;
        let master = AsyncFd::new(pty.master).context("PTY async readiness")?;
        let child = command.spawn().context("spawn terminal process")?;
        drop(pty.slave);
        let mut emulator = Emulator::new(program.geometry.rows, program.geometry.cols, 2000);
        let initial = Arc::new(Snapshot {
            version: 1,
            frame: Arc::new(emulator.snapshot()),
            geometry: program.geometry,
            checkpoints: BTreeMap::new(),
            exit: None,
        });
        let (publish, snapshots) = watch::channel(initial);
        let (commands, receive) = mpsc::channel(COMMAND_CAP);
        let stop = Arc::new(Stop {
            requested: AtomicBool::new(false),
            wake: Notify::new(),
        });
        let session = PtySession(child.id().expect("newly spawned child"));
        let owner = Owner {
            master,
            child,
            session,
            emulator,
            ledger: Ledger::default(),
            receive,
            publish,
            geometry: program.geometry,
            view_geometry: BTreeMap::new(),
            version: 1,
            stop: stop.clone(),
        };
        runtime.spawn(owner.run());
        Ok(Self {
            commands,
            snapshots,
            stop,
        })
    }

    pub fn subscribe(&self) -> watch::Receiver<Arc<Snapshot>> {
        self.snapshots.clone()
    }
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.snapshots.borrow().clone()
    }

    async fn request<T>(&self, make: impl FnOnce(Reply<T>) -> OwnerCommand) -> Result<T> {
        let (send, receive) = oneshot::channel();
        self.commands
            .send(make(send))
            .await
            .map_err(|_| anyhow::anyhow!("terminal ended"))?;
        Ok(receive.await.context("terminal owner stopped")??)
    }

    pub async fn attach(&self) -> Result<Connection> {
        self.request(OwnerCommand::Attach).await
    }
    pub async fn resume(&self, connection: Connection) -> Result<Connection> {
        self.request(|reply| OwnerCommand::Resume(connection, reply))
            .await
    }
    pub async fn detach(&self, connection: Connection) -> Result<()> {
        self.request(|reply| OwnerCommand::Detach(connection, reply))
            .await
    }
    /// Explicitly forget a disconnected attachment after all accepted input has
    /// settled. The connection cannot subsequently resume.
    pub async fn retire(&self, connection: Connection) -> Result<()> {
        self.request(|reply| OwnerCommand::Retire(connection, reply))
            .await
    }
    pub async fn focus(&self, connection: Connection) -> Result<()> {
        self.request(|reply| OwnerCommand::Focus(connection, reply))
            .await
    }
    pub async fn input(
        &self,
        connection: Connection,
        seq: u64,
        bytes: Vec<u8>,
    ) -> Result<InputResult> {
        if bytes.is_empty() || bytes.len() > MAX_INPUT {
            bail!(InputError::InvalidInput);
        }
        self.request(|reply| OwnerCommand::Input(connection, seq, bytes, reply))
            .await
    }
    pub async fn resize(&self, connection: Connection, geometry: Geometry) -> Result<()> {
        Geometry::new(geometry.cols, geometry.rows)
            .map_err(|_| anyhow::anyhow!("invalid geometry"))?;
        self.request(|reply| OwnerCommand::Resize(connection, geometry, reply))
            .await
    }

    /// Out-of-band stop cannot wait behind a full input queue.
    pub fn request_close(&self) {
        self.stop.requested.store(true, Ordering::SeqCst);
        self.stop.wake.notify_one();
    }

    pub async fn close(&self) -> Result<Exit> {
        self.request_close();
        let mut snapshots = self.subscribe();
        loop {
            if let Some(exit) = snapshots.borrow().exit.clone() {
                return Ok(exit);
            }
            snapshots
                .changed()
                .await
                .context("owner ended without exit status")?;
        }
    }
}

struct PtySession(u32);

impl Drop for PtySession {
    fn drop(&mut self) {
        signal_session(Some(self.0), libc::SIGKILL);
    }
}

struct Owner {
    master: AsyncFd<File>,
    child: Child,
    session: PtySession,
    emulator: Emulator,
    ledger: Ledger,
    receive: mpsc::Receiver<OwnerCommand>,
    publish: watch::Sender<Arc<Snapshot>>,
    geometry: Geometry,
    view_geometry: BTreeMap<AttachmentId, Geometry>,
    version: u64,
    stop: Arc<Stop>,
}

impl Owner {
    fn publish(&mut self, exit: Option<Exit>) {
        self.version = self
            .version
            .checked_add(1)
            .expect("screen version exhausted");
        let frame = Arc::new(self.emulator.snapshot());
        self.publish.send_replace(Arc::new(Snapshot {
            version: self.version,
            frame,
            geometry: self.geometry,
            checkpoints: self.ledger.checkpoints(),
            exit,
        }));
    }

    fn complete_command<T>(&mut self, result: std::result::Result<T, InputError>, reply: Reply<T>) {
        // Publish before waking a requester on another runtime worker.
        self.publish(None);
        let _ = reply.send(result);
    }

    fn command(&mut self, command: OwnerCommand) -> Result<()> {
        match command {
            OwnerCommand::Attach(reply) => {
                let result = self.ledger.attach();
                if let Ok(c) = result {
                    self.view_geometry.insert(c.id, self.geometry);
                }
                self.complete_command(result, reply);
            }
            OwnerCommand::Resume(c, reply) => {
                let result = self.ledger.resume(c);
                self.complete_command(result, reply);
            }
            OwnerCommand::Detach(c, reply) => {
                let result = self.ledger.detach(c);
                self.complete_command(result, reply);
            }
            OwnerCommand::Retire(c, reply) => {
                let result = self.ledger.retire(c);
                if result.is_ok() {
                    self.view_geometry.remove(&c.id);
                }
                self.complete_command(result, reply);
            }
            OwnerCommand::Focus(c, reply) => {
                let result = self.ledger.validate(c);
                if result.is_ok() {
                    self.apply_geometry(self.view_geometry[&c.id])?;
                }
                self.complete_command(result, reply);
            }
            OwnerCommand::Input(c, seq, bytes, reply) => {
                let result = self.ledger.input(c, seq, bytes);
                if result == Ok(InputResult::Accepted) {
                    self.apply_geometry(self.view_geometry[&c.id])?;
                }
                self.complete_command(result, reply);
            }
            OwnerCommand::Resize(c, geometry, reply) => {
                let result = self.ledger.validate(c);
                if result.is_ok() {
                    self.apply_geometry(geometry)?;
                    self.view_geometry.insert(c.id, geometry);
                }
                self.complete_command(result, reply);
            }
        }
        Ok(())
    }

    fn apply_geometry(&mut self, geometry: Geometry) -> Result<()> {
        if geometry == self.geometry {
            return Ok(());
        }
        let size = libc::winsize {
            ws_row: geometry.rows,
            ws_col: geometry.cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        if unsafe { libc::ioctl(self.master.get_ref().as_raw_fd(), libc::TIOCSWINSZ, &size) } < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        self.emulator.resize(geometry.rows, geometry.cols);
        self.geometry = geometry;
        self.ledger.geometry_changed();
        Ok(())
    }

    fn consume_output(&mut self, bytes: &[u8]) -> Result<()> {
        self.emulator.process(bytes);
        self.ledger.generated(self.emulator.take_pty_write())?;
        self.publish(None);
        Ok(())
    }

    async fn run(mut self) {
        let mut bytes = [0u8; 8192];
        let pid = Some(self.session.0);
        let mut status = None;
        let result: Result<()> = async {
            loop {
                if self.stop.requested.load(Ordering::SeqCst) { break; }
                let want_write = self.ledger.write_buf().is_some();
                let echo_at = self.ledger.deadline();
                let sync_at = self.emulator.sync_deadline();
                tokio::select! {
                    _ = self.stop.wake.notified() => break,
                    exited = self.child.wait() => { status = Some(exited?); break; }
                    ready = self.master.readable() => {
                        let read = {
                            let mut guard = ready?;
                            guard.try_io(|inner| inner.get_ref().read(&mut bytes))
                        };
                        match read {
                            Ok(Ok(0)) => break,
                            Ok(Ok(n)) => self.consume_output(&bytes[..n])?,
                            Ok(Err(e)) if e.raw_os_error() == Some(libc::EIO) => break,
                            Ok(Err(e)) => return Err(e.into()),
                            Err(_) => {},
                        }
                    }
                    ready = self.master.writable(), if want_write => {
                        let mut guard = ready?;
                        match guard.try_io(|inner| inner.get_ref().write(self.ledger.write_buf().unwrap())) {
                            Ok(Ok(0)) => bail!("PTY write returned zero"),
                            Ok(Ok(n)) => self.ledger.written(n, Instant::now()),
                            Ok(Err(e)) => return Err(e.into()),
                            Err(_) => {},
                        }
                    }
                    command = self.receive.recv() => {
                        match command { Some(c) => self.command(c)?, None => break }
                    }
                    _ = wait_until(echo_at) => {
                        if self.ledger.settle(Instant::now()) { self.publish(None); }
                    }
                    _ = wait_until(sync_at) => {
                        self.emulator.flush_sync();
                        self.ledger.generated(self.emulator.take_pty_write())?;
                        self.publish(None);
                    }
                }
            }
            Ok(())
        }.await;
        if status.is_none() {
            signal_session(pid, libc::SIGTERM);
            status = match tokio::time::timeout(Duration::from_secs(2), self.child.wait()).await {
                Ok(Ok(s)) => Some(s),
                _ => {
                    signal_session(pid, libc::SIGKILL);
                    let _ = self.child.start_kill();
                    tokio::time::timeout(Duration::from_millis(400), self.child.wait())
                        .await
                        .ok()
                        .and_then(|result| result.ok())
                }
            };
        }
        // Job-control groups in the owning session must not outlive its leader.
        signal_session(pid, libc::SIGKILL);
        // Preserve the final available output even when wait() won the race.
        for _ in 0..256 {
            match self.master.get_ref().read(&mut bytes) {
                Ok(0) => break,
                Ok(n) => {
                    self.emulator.process(&bytes[..n]);
                }
                Err(_) => break,
            }
        }
        self.emulator.flush_sync();
        self.publish(Some(Exit {
            code: status.as_ref().and_then(|s| s.code()),
            signal: status.as_ref().and_then(|s| s.signal()),
            error: result.err().map(|e| format!("{e:#}")).or_else(|| {
                status
                    .is_none()
                    .then(|| "child could not be reaped within deadline".into())
            }),
        }));
    }
}

async fn wait_until(deadline: Option<Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at.into()).await,
        None => std::future::pending::<()>().await,
    }
}

fn set_nonblocking(file: &File) -> Result<()> {
    let fd = file.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

fn process_group_session(stat: &[u8]) -> Option<(i32, i32)> {
    // comm can contain spaces, parentheses, or non-UTF-8 bytes. Numeric fields
    // start after the LAST closing parenthesis, not after a whitespace split.
    let end = stat.iter().rposition(|b| *b == b')')?;
    let fields = std::str::from_utf8(&stat[end + 1..]).ok()?;
    let mut fields = fields.split_whitespace();
    fields.next()?; // state
    fields.next()?; // ppid
    Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
}

fn signal_session(pid: Option<u32>, signal: i32) {
    let Some(session) = pid.map(|pid| pid as i32) else {
        return;
    };
    // Interactive shells place foreground/background jobs in different PGIDs.
    // All remain in the PTY's new session unless deliberately detached with
    // setsid(). Never signal another user's/session's unrelated process tree.
    let mut groups = std::collections::BTreeSet::from([session]);
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let Some(member) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<i32>().ok())
            else {
                continue;
            };
            let Ok(stat) = std::fs::read(entry.path().join("stat")) else {
                continue;
            };
            let Some((group, sid)) = process_group_session(&stat) else {
                continue;
            };
            if group <= 0 || sid != session || groups.contains(&group) {
                continue;
            }
            // Recheck membership immediately before signaling. A proc entry may
            // have exited or changed session while its stat record was read.
            if unsafe { libc::getsid(member) } == session
                && unsafe { libc::getpgid(member) } == group
            {
                unsafe {
                    libc::kill(-group, signal);
                }
                groups.insert(group);
            }
        }
    }
    unsafe {
        libc::kill(-session, signal);
    }
}

#[cfg(test)]
mod process_tests {
    use super::*;
    #[test]
    fn proc_session_parser_handles_parentheses_and_non_utf8_comm() {
        assert_eq!(
            process_group_session(b"12 (has ) spaces \xff) S 1 55 44 0"),
            Some((55, 44))
        );
        assert_eq!(process_group_session(b"broken"), None);
        assert_eq!(process_group_session(b"12 (x) S 1 55"), None);
    }
}
