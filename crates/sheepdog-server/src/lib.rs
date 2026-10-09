pub use sheepdog_proto as protocol;
pub mod state;
mod transport;

use anyhow::{Context, Result, bail};
use quosh_trust::CertChain;
use std::fs::File;
use std::net::SocketAddr;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::sync::{Notify, Semaphore};
use wtransport::{Endpoint, ServerConfig, VarInt, endpoint::endpoint_side};

pub use transport::read_frame;

pub struct Daemon {
    pub state: Arc<state::State>,
    pub chain: Arc<CertChain>,
    pub port: u16,
    pub socket: PathBuf,
    endpoint: Endpoint<endpoint_side::Server>,
    helper: UnixListener,
    _lock: File,
}

impl Daemon {
    pub fn bind(bind: SocketAddr, directory: PathBuf, shell: PathBuf) -> Result<Arc<Self>> {
        std::fs::create_dir_all(&directory)?;
        if std::fs::metadata(&directory)?.uid() != unsafe { libc::geteuid() } {
            bail!("state directory belongs to another user");
        }
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
        let lock = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(directory.join("server.lock"))?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } < 0 {
            bail!("a Sheepdog server already owns this state directory");
        }
        let chain = CertChain::load(&directory.join("tls"), quosh_trust::DEFAULT_CHAIN)?;
        let config = ServerConfig::builder()
            .with_bind_address(bind)
            .with_custom_tls(chain.tls_config()?)
            .build();
        let endpoint = Endpoint::server(config)?;
        let port = endpoint.local_addr()?.port();
        let socket = directory.join("server.sock");
        if socket.exists() {
            std::fs::remove_file(&socket)?;
        }
        let helper = UnixListener::bind(&socket)?;
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
        let state = state::State::load(directory, shell)?;
        Ok(Arc::new(Self {
            state,
            chain,
            port,
            socket,
            endpoint,
            helper,
            _lock: lock,
        }))
    }

    pub async fn serve(self: Arc<Self>, shutdown: Arc<Notify>) -> Result<()> {
        let slots = Arc::new(Semaphore::new(256));
        let mut tasks = tokio::task::JoinSet::new();
        let mut prune = tokio::time::interval(Duration::from_secs(30));
        loop {
            tokio::select! {
                _ = shutdown.notified() => break,
                _ = prune.tick() => self.state.prune().await,
                result = tasks.join_next(), if !tasks.is_empty() => {
                    if let Some(Err(error)) = result { eprintln!("connection task failed: {error}"); }
                }
                accepted = self.helper.accept() => {
                    let (mut stream, _) = accepted?;
                    let Ok(slot) = slots.clone().try_acquire_owned() else { continue; };
                    let daemon = self.clone();
                    tasks.spawn(async move {
                        let _slot = slot;
                        let result = tokio::time::timeout(Duration::from_secs(5), async {
                            if stream.peer_cred()?.uid() != unsafe { libc::geteuid() } { bail!("bootstrap peer is not the server user"); }
                            let mut request = Vec::new();
                            loop {
                                if request.len() >= 32 { bail!("bootstrap request too large"); }
                                let byte = stream.read_u8().await?;
                                if byte == b'\n' { break; }
                                request.push(byte);
                            }
                            if request != b"bootstrap" { bail!("unknown bootstrap operation"); }
                            let bootstrap = daemon.state.bootstrap(daemon.port, daemon.chain.forward_hashes()?).await?;
                            stream.write_all(&serde_json::to_vec(&bootstrap)?).await?;
                            stream.shutdown().await?;
                            Ok::<(), anyhow::Error>(())
                        }).await;
                        if !matches!(result, Ok(Ok(()))) { let _ = stream.shutdown().await; }
                    });
                }
                incoming = self.endpoint.accept() => {
                    let Ok(slot) = slots.clone().try_acquire_owned() else { continue; };
                    let daemon = self.clone();
                    tasks.spawn(async move {
                        let _slot = slot;
                        let _ = transport::handle(incoming, daemon).await;
                    });
                }
            }
        }
        self.endpoint.close(VarInt::from_u32(0), b"server shutdown");
        self.state.shutdown().await;
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        let _ = std::fs::remove_file(&self.socket);
        Ok(())
    }
}

/// Called locally on the remote host through the SSH-authenticated channel.
pub async fn bootstrap(directory: PathBuf) -> Result<protocol::Bootstrap> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut stream = tokio::net::UnixStream::connect(directory.join("server.sock"))
            .await
            .context("connect to Sheepdog bootstrap socket")?;
        stream.write_all(b"bootstrap\n").await?;
        let mut response = Vec::new();
        stream.take(16 * 1024).read_to_end(&mut response).await?;
        Ok(serde_json::from_slice(&response)?)
    })
    .await
    .context("bootstrap deadline")?
}
