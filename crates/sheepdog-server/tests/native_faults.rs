//! Deliberately faulty authenticated peers: protocol errors must be fatal,
//! and canceled RPCs must never reuse a partially consumed stream.
use quosh_proto::{HelloOk, PROTOCOL_VERSION, encode_frame};
use sheepdog_client::{Client, ViewStatus};
use sheepdog_core::{Geometry, TabId};
use sheepdog_proto::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::sync::Notify;
use wtransport::{Endpoint, ServerConfig, VarInt};

async fn read_frame(recv: &mut wtransport::RecvStream) -> (u8, Vec<u8>) {
    let mut header = [0; 4];
    recv.read_exact(&mut header).await.unwrap();
    let len = quosh_proto::peek_len(&header).unwrap().unwrap();
    assert!(len <= MAX_REQUEST);
    let mut bytes = vec![0; len];
    recv.read_exact(&mut bytes).await.unwrap();
    (bytes[0], bytes[1..].to_vec())
}

struct Peer {
    directory: std::path::PathBuf,
    bootstrap: Bootstrap,
    stop: Arc<Notify>,
    partial: Arc<Notify>,
    controls: Arc<AtomicUsize>,
    terminals: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl Peer {
    fn new(partial_rpc: bool) -> Self {
        let directory =
            std::env::temp_dir().join(format!("sheepdog-fault-peer-{}", rand::random::<u64>()));
        let chain = quosh_trust::CertChain::load(&directory, 1).unwrap();
        let hashes = chain.forward_hashes().unwrap();
        let endpoint = Endpoint::server(
            ServerConfig::builder()
                .with_bind_address("127.0.0.1:0".parse().unwrap())
                .with_custom_tls(chain.tls_config().unwrap())
                .build(),
        )
        .unwrap();
        let bootstrap = Bootstrap {
            protocol: VERSION,
            port: endpoint.local_addr().unwrap().port(),
            hashes: hashes.clone(),
            token: [1; 32],
        };
        let stop = Arc::new(Notify::new());
        let partial = Arc::new(Notify::new());
        let controls = Arc::new(AtomicUsize::new(0));
        let terminals = Arc::new(AtomicUsize::new(0));
        let task = {
            let stop = stop.clone();
            let partial = partial.clone();
            let controls = controls.clone();
            let terminals = terminals.clone();
            tokio::spawn(async move {
                let mut tasks = tokio::task::JoinSet::new();
                loop {
                    tokio::select! {
                        _ = stop.notified() => break,
                        incoming = endpoint.accept() => {
                            let hashes = hashes.clone(); let partial = partial.clone(); let controls = controls.clone(); let terminals = terminals.clone();
                            tasks.spawn(async move {
                                let request = incoming.await.unwrap(); let path = request.path().to_owned();
                                let connection = request.accept().await.unwrap();
                                let (mut send, mut recv) = connection.accept_bi().await.unwrap();
                                let _ = read_frame(&mut recv).await;
                                if path == TERMINAL_PATH {
                                    terminals.fetch_add(1, Ordering::SeqCst);
                                    send.write_all(&HelloOk { protocol: PROTOCOL_VERSION, session_id: [2; 16], version: 1, cols: 80, rows: 24 }.encode()).await.unwrap();
                                    // Zero length is invalid before any frame body is available.
                                    send.write_all(&0u32.to_le_bytes()).await.unwrap();
                                    connection.closed().await;
                                } else {
                                    assert_eq!(path, CONTROL_PATH);
                                    let ordinal = controls.fetch_add(1, Ordering::SeqCst);
                                    send.write_all(&encode_frame(AUTH_OK, &serde_json::to_vec(&AuthOk { protocol: VERSION, hashes }).unwrap())).await.unwrap();
                                    let (_, bytes) = read_frame(&mut recv).await;
                                    let request: Request = serde_json::from_slice(&bytes).unwrap();
                                    let reply = match request {
                                        Request::Attach { tab, .. } => Response::Attachment(Ticket { tab, session_id: [2; 16], token: [3; 32] }),
                                        Request::Snapshot => Response::Snapshot { spaces: vec![], tabs: vec![], selected: None },
                                        _ => panic!("unexpected fixture request"),
                                    };
                                    let frame = encode_frame(REPLY, &serde_json::to_vec(&reply).unwrap());
                                    if partial_rpc && ordinal == 0 { send.write_all(&frame[..2]).await.unwrap(); partial.notify_one(); }
                                    else { send.write_all(&frame).await.unwrap(); }
                                    connection.closed().await;
                                }
                            });
                        }
                    }
                }
                endpoint.close(VarInt::from_u32(0), b"fixture stopped");
                tasks.abort_all();
                while tasks.join_next().await.is_some() {}
            })
        };
        Self {
            directory,
            bootstrap,
            stop,
            partial,
            controls,
            terminals,
            task,
        }
    }
    async fn close(self) {
        self.stop.notify_one();
        self.task.await.unwrap();
        std::fs::remove_dir_all(self.directory).unwrap();
    }
}
#[tokio::test]
async fn malformed_terminal_framing_is_fatal_not_an_infinite_reconnect_loop() {
    let peer = Peer::new(false);
    let client = Client::connect("127.0.0.1".into(), peer.bootstrap.clone())
        .await
        .unwrap();
    let view = client
        .attach(TabId(9), Geometry::new(80, 24).unwrap(), true)
        .await
        .unwrap();
    let mut updates = view.subscribe();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !matches!(view.snapshot().status, ViewStatus::Failed(_)) {
            updates.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    if let ViewStatus::Failed(message) = view.snapshot().status {
        assert!(message.contains("frame"));
    }
    assert_eq!(peer.terminals.load(Ordering::SeqCst), 1);
    peer.close().await;
}
#[tokio::test]
async fn canceling_a_partial_rpc_drops_the_stream_and_next_request_authenticates_fresh() {
    let peer = Peer::new(true);
    let client = Client::connect("127.0.0.1".into(), peer.bootstrap.clone())
        .await
        .unwrap();
    let pending = {
        let client = client.clone();
        tokio::spawn(async move { client.request(Request::Snapshot).await })
    };
    tokio::time::timeout(Duration::from_secs(2), peer.partial.notified())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    pending.abort();
    assert!(matches!(pending.await, Err(error) if error.is_cancelled()));
    assert!(matches!(
        client.request(Request::Snapshot).await.unwrap(),
        Response::Snapshot { .. }
    ));
    assert_eq!(peer.controls.load(Ordering::SeqCst), 2);
    peer.close().await;
}
