use quosh_proto::{
    Hello, MSG_ERROR, MSG_HELLO_OK, MSG_SHARED_SCREEN, PROTOCOL_VERSION, SharedScreen,
    encode_frame, encode_input,
};
use sheepdog_core::{Geometry, TerminalState};
use sheepdog_server::{Daemon, bootstrap, protocol::*, read_frame};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use wtransport::{
    ClientConfig, Endpoint, RecvStream, SendStream, VarInt, endpoint::endpoint_side,
    tls::Sha256Digest,
};

struct Harness {
    daemon: Arc<Daemon>,
    directory: PathBuf,
    stop: Arc<Notify>,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl Harness {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "sheepdog-net-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let daemon = Daemon::bind(
            "127.0.0.1:0".parse().unwrap(),
            directory.clone(),
            "/bin/sh".into(),
        )
        .unwrap();
        let stop = Arc::new(Notify::new());
        let task = tokio::spawn(daemon.clone().serve(stop.clone()));
        Self {
            daemon,
            directory,
            stop,
            task,
        }
    }
    async fn credential(&self) -> Bootstrap {
        bootstrap(self.directory.clone()).await.unwrap()
    }
    async fn connect(&self, path: &str, hashes: Vec<[u8; 32]>) -> anyhow::Result<Link> {
        let config = ClientConfig::builder()
            .with_bind_default()
            .with_server_certificate_hashes(hashes.into_iter().map(Sha256Digest::new))
            .build();
        let endpoint = Endpoint::client(config)?;
        let connection = endpoint
            .connect(format!("https://127.0.0.1:{}{path}", self.daemon.port))
            .await?;
        let (send, recv) = connection.open_bi().await?.await?;
        Ok(Link {
            _endpoint: endpoint,
            connection,
            send,
            recv,
        })
    }
    async fn control(&self, credential: &Bootstrap) -> Link {
        let mut link = self
            .connect(CONTROL_PATH, credential.hashes.clone())
            .await
            .unwrap();
        link.send
            .write_all(&encode_frame(AUTH, &credential.token))
            .await
            .unwrap();
        assert_eq!(link.frame().await.0, AUTH_OK);
        link
    }
    async fn terminal(&self, credential: &Bootstrap, ticket: &Ticket) -> Link {
        let mut link = self
            .connect(TERMINAL_PATH, credential.hashes.clone())
            .await
            .unwrap();
        link.send
            .write_all(
                &Hello {
                    protocol: PROTOCOL_VERSION,
                    session_id: ticket.session_id,
                    token: ticket.token,
                    cols: 80,
                    rows: 24,
                }
                .encode(),
            )
            .await
            .unwrap();
        assert_eq!(link.frame().await.0, MSG_HELLO_OK);
        link
    }
    async fn shutdown(self) {
        self.stop.notify_one();
        self.task.await.unwrap().unwrap();
        drop(self.daemon);
        let _ = std::fs::remove_dir_all(self.directory);
    }
}

struct Link {
    _endpoint: Endpoint<endpoint_side::Client>,
    connection: wtransport::Connection,
    send: SendStream,
    recv: RecvStream,
}

impl Link {
    async fn frame(&mut self) -> (u8, Vec<u8>) {
        tokio::time::timeout(
            Duration::from_secs(5),
            read_frame(&mut self.recv, quosh_proto::MAX_FRAME),
        )
        .await
        .unwrap()
        .unwrap()
    }
    async fn request(&mut self, request: Request) -> Response {
        self.send
            .write_all(&encode_frame(
                REQUEST,
                &serde_json::to_vec(&request).unwrap(),
            ))
            .await
            .unwrap();
        let (kind, bytes) = self.frame().await;
        assert_eq!(kind, REPLY);
        serde_json::from_slice(&bytes).unwrap()
    }
    async fn checkpoint(&mut self, seq: u64) -> SharedScreen {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let (kind, bytes) = read_frame(&mut self.recv, quosh_proto::MAX_FRAME)
                    .await
                    .unwrap();
                if kind == MSG_SHARED_SCREEN {
                    let screen = SharedScreen::decode_compressed(&bytes).unwrap();
                    if screen.screen.echo_ack >= seq {
                        return screen;
                    }
                } else {
                    assert_ne!(kind, MSG_ERROR, "{}", String::from_utf8_lossy(&bytes));
                }
            }
        })
        .await
        .unwrap()
    }
}

fn text(screen: &SharedScreen) -> String {
    let frame = &screen.screen.frame;
    let mut result = String::new();
    for row in 0..frame.rows() {
        for col in 0..frame.cols() {
            result.push_str(frame.cell_content(row, col));
        }
        result.push('\n');
    }
    result
}

async fn create(control: &mut Link) -> sheepdog_core::TabId {
    let Response::Space(space) = control
        .request(Request::CreateSpace {
            name: "tests".into(),
            directory: std::env::temp_dir(),
        })
        .await
    else {
        panic!("create Space failed");
    };
    let Response::Tab(tab) = control
        .request(Request::CreateTab {
            space: space.id,
            label: "shell".into(),
            geometry: Geometry::new(80, 24).unwrap(),
        })
        .await
    else {
        panic!("create Tab failed");
    };
    assert!(matches!(tab.terminal, TerminalState::Running(_)));
    tab.id
}

async fn attach(control: &mut Link, tab: sheepdog_core::TabId) -> Ticket {
    let Response::Attachment(ticket) = control
        .request(Request::Attach {
            tab,
            geometry: Geometry::new(80, 24).unwrap(),
        })
        .await
    else {
        panic!("attach failed");
    };
    ticket
}

#[tokio::test]
async fn bootstrap_is_private_and_invalid_control_credentials_are_rejected() {
    use std::os::unix::fs::PermissionsExt;
    let harness = Harness::new();
    let credential = harness.credential().await;
    assert_eq!(
        std::fs::metadata(&harness.directory)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(&harness.daemon.socket)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let mut bad = harness
        .connect(CONTROL_PATH, credential.hashes)
        .await
        .unwrap();
    bad.send
        .write_all(&encode_frame(AUTH, &[0; 32]))
        .await
        .unwrap();
    assert_eq!(bad.frame().await.0, MSG_ERROR);
    harness.shutdown().await;
}

#[tokio::test]
async fn wrong_certificate_pin_cannot_establish_transport() {
    let harness = Harness::new();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        harness.connect(CONTROL_PATH, vec![[0; 32]]),
    )
    .await
    .unwrap();
    assert!(result.is_err());
    harness.shutdown().await;
}

#[tokio::test]
async fn real_network_shared_input_and_reconnect_replay_preserve_one_terminal() {
    let harness = Harness::new();
    let a = harness.credential().await;
    let b = harness.credential().await;
    let mut control_a = harness.control(&a).await;
    let mut control_b = harness.control(&b).await;
    let tab = create(&mut control_a).await;
    let ticket_a = attach(&mut control_a, tab).await;
    let ticket_b = attach(&mut control_b, tab).await;
    let mut view_a = harness.terminal(&a, &ticket_a).await;
    let mut view_b = harness.terminal(&b, &ticket_b).await;
    // The octal marker cannot appear merely from the PTY echoing this command.
    view_a
        .send
        .write_all(&encode_input(
            1,
            b"stty -echo; printf '\\122\\105\\101\\104\\131\\n'\n",
        ))
        .await
        .unwrap();
    let mut ready = view_a.checkpoint(1).await;
    while !text(&ready).contains("READY") {
        ready = view_a.checkpoint(1).await;
    }
    view_a
        .send
        .write_all(&encode_input(2, b"printf 'A_MARKER\\n'\n"))
        .await
        .unwrap();
    let first = view_a.checkpoint(2).await;
    assert!(text(&first).contains("A_MARKER"));
    assert!(!first.prediction_allowed);
    view_b
        .send
        .write_all(&encode_input(1, b"printf 'B_MARKER\\n'\n"))
        .await
        .unwrap();
    let second = view_b.checkpoint(1).await;
    assert!(text(&second).contains("A_MARKER") && text(&second).contains("B_MARKER"));
    assert_eq!(second.screen.echo_ack, 1);
    view_a.connection.close(VarInt::from_u32(0), b"sleep");
    let mut resumed = harness.terminal(&a, &ticket_a).await;
    resumed
        .send
        .write_all(&encode_input(2, b"printf 'A_MARKER\\n'\n"))
        .await
        .unwrap();
    resumed
        .send
        .write_all(&encode_input(3, b"printf 'AFTER_REPLAY\\n'\n"))
        .await
        .unwrap();
    let last = resumed.checkpoint(3).await;
    assert_eq!(text(&last).matches("A_MARKER").count(), 1);
    assert!(text(&last).contains("AFTER_REPLAY"));
    assert!(matches!(
        control_a
            .request(Request::CloseTab {
                tab,
                confirmed: false
            })
            .await,
        Response::Error(_)
    ));
    assert!(matches!(
        control_a
            .request(Request::CloseTab {
                tab,
                confirmed: true
            })
            .await,
        Response::Closed
    ));
    harness.shutdown().await;
}

#[tokio::test]
async fn fragmented_input_survives_screen_updates_from_another_view() {
    let harness = Harness::new();
    let a = harness.credential().await;
    let b = harness.credential().await;
    let mut control_a = harness.control(&a).await;
    let mut control_b = harness.control(&b).await;
    let tab = create(&mut control_a).await;
    let ticket_a = attach(&mut control_a, tab).await;
    let ticket_b = attach(&mut control_b, tab).await;
    let mut view_a = harness.terminal(&a, &ticket_a).await;
    let mut view_b = harness.terminal(&b, &ticket_b).await;
    let frame = encode_input(1, b"printf 'FRAGMENT_SURVIVED\\n'\n");
    view_a.send.write_all(&frame[..6]).await.unwrap();
    view_b
        .send
        .write_all(&quosh_proto::encode_resize(45, 12))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    for chunk in frame[6..].chunks(3) {
        view_a.send.write_all(chunk).await.unwrap();
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let screen = view_a.checkpoint(1).await;
    assert!(text(&screen).contains("FRAGMENT_SURVIVED"));
    assert_eq!(
        screen.screen.frame.cols(),
        80,
        "input promotes the sender's remembered dimensions"
    );
    harness.shutdown().await;
}

#[tokio::test]
async fn oversized_control_frame_is_rejected_before_its_body_arrives() {
    let harness = Harness::new();
    let credential = harness.credential().await;
    let mut control = harness.control(&credential).await;
    control
        .send
        .write_all(&((MAX_REQUEST as u32) + 1).to_le_bytes())
        .await
        .unwrap();
    assert_eq!(control.frame().await.0, MSG_ERROR);
    harness.shutdown().await;
}

#[tokio::test]
async fn reusable_quosh_client_consumes_real_shared_screens_and_acks() {
    let harness = Harness::new();
    let credential = harness.credential().await;
    let mut control = harness.control(&credential).await;
    let tab = create(&mut control).await;
    let ticket = attach(&mut control, tab).await;
    let mut link = harness
        .connect(TERMINAL_PATH, credential.hashes.clone())
        .await
        .unwrap();
    let mut client = quosh_client::Client::new(ticket.session_id, ticket.token, 80, 24, false);
    client.begin_connection(0);
    let hello = client.outbound().to_vec();
    link.send.write_all(&hello).await.unwrap();
    client.advance_outbound(hello.len());
    while client.display().is_none() {
        let (kind, bytes) = link.frame().await;
        client.recv_control(&encode_frame(kind, &bytes), 1).unwrap();
    }
    assert!(client.hello_ok());
    client.queue_input(b"printf 'CLIENT_PATH\\n'\n".to_vec(), 2);
    client.pump();
    while client.writing() {
        let output = client.outbound().to_vec();
        link.send.write_all(&output).await.unwrap();
        client.advance_outbound(output.len());
        client.pump();
    }
    let mut echo_seen = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (kind, bytes) = link.frame().await;
            if kind == MSG_SHARED_SCREEN {
                let screen = SharedScreen::decode_compressed(&bytes).unwrap();
                echo_seen |= screen.screen.echo_ack == 1 && text(&screen).contains("CLIENT_PATH");
            }
            client
                .recv_control(&encode_frame(kind, &bytes), 100)
                .unwrap();
            if echo_seen && client.unacked_bytes() == 0 {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert!(client.display().is_some());
    assert!(!client.is_hungup());
    harness.shutdown().await;
}

#[tokio::test]
async fn persistence_restores_ended_tabs_without_launching_commands() {
    let harness = Harness::new();
    let credential = harness.credential().await;
    let mut control = harness.control(&credential).await;
    let tab = create(&mut control).await;
    harness.stop.notify_one();
    // Read the organization via a fresh model, never restart a process.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let restored =
        sheepdog_server::state::State::load(harness.directory.clone(), "/bin/sh".into()).unwrap();
    let boot = restored.bootstrap(1, vec![]).await.unwrap();
    let Response::Snapshot { tabs, .. } = restored
        .execute(boot.token, Request::Snapshot)
        .await
        .unwrap()
    else {
        panic!("snapshot failed");
    };
    assert_eq!(tabs[0].id, tab);
    assert_eq!(
        tabs[0].terminal,
        TerminalState::Ended(sheepdog_core::EndReason::ServerRestart)
    );
    harness.shutdown().await;
}

#[tokio::test]
async fn explicit_client_disconnect_revokes_access_but_preserves_running_tabs() {
    let harness = Harness::new();
    let a = harness.credential().await;
    let b = harness.credential().await;
    let mut control_a = harness.control(&a).await;
    let mut control_b = harness.control(&b).await;
    let tab = create(&mut control_a).await;
    let ticket = attach(&mut control_a, tab).await;
    let _view = harness.terminal(&a, &ticket).await;
    assert!(matches!(
        control_a.request(Request::Disconnect).await,
        Response::Closed
    ));
    assert!(harness.daemon.state.authenticate(a.token).await.is_err());
    let Response::Snapshot { tabs, .. } = control_b.request(Request::Snapshot).await else {
        panic!("snapshot");
    };
    assert_eq!(tabs[0].id, tab);
    assert!(matches!(tabs[0].terminal, TerminalState::Running(_)));
    let mut old = harness.connect(TERMINAL_PATH, a.hashes).await.unwrap();
    old.send
        .write_all(
            &Hello {
                protocol: PROTOCOL_VERSION,
                session_id: ticket.session_id,
                token: ticket.token,
                cols: 80,
                rows: 24,
            }
            .encode(),
        )
        .await
        .unwrap();
    assert_eq!(old.frame().await.0, MSG_ERROR);
    harness.shutdown().await;
}

#[tokio::test]
async fn only_one_server_can_own_a_state_directory() {
    let harness = Harness::new();
    assert!(
        Daemon::bind(
            "127.0.0.1:0".parse().unwrap(),
            harness.directory.clone(),
            "/bin/sh".into()
        )
        .is_err()
    );
    harness.shutdown().await;
}

#[tokio::test]
async fn bad_terminal_ticket_is_rejected_without_creating_a_shell() {
    let harness = Harness::new();
    let credential = harness.credential().await;
    let mut link = harness
        .connect(TERMINAL_PATH, credential.hashes)
        .await
        .unwrap();
    link.send
        .write_all(
            &Hello {
                protocol: PROTOCOL_VERSION,
                session_id: [0; 16],
                token: [0; 32],
                cols: 80,
                rows: 24,
            }
            .encode(),
        )
        .await
        .unwrap();
    assert_eq!(link.frame().await.0, MSG_ERROR);
    let Response::Snapshot { tabs, .. } = harness
        .daemon
        .state
        .execute(credential.token, Request::Snapshot)
        .await
        .unwrap()
    else {
        panic!("snapshot");
    };
    assert!(tabs.is_empty());
    harness.shutdown().await;
}
