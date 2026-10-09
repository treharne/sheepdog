//! Portable native adapter: SSH trust bootstrap, Control Plane RPCs, and
//! reconnecting terminal views. Does not depend on Linux PTY/server code.
mod ssh;
mod view;
pub use ssh::{SshOptions, bootstrap_ssh};
pub use view::{View, ViewSnapshot, ViewStatus};

use anyhow::{Result, bail, ensure};
use sheepdog_core::{Geometry, TabId};
use sheepdog_proto::*;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use wtransport::endpoint::endpoint_side;
use wtransport::tls::Sha256Digest;
use wtransport::{ClientConfig, Connection, Endpoint, RecvStream, SendStream, VarInt};

const DEADLINE: Duration = Duration::from_secs(5);

struct Link {
    _endpoint: Endpoint<endpoint_side::Client>,
    connection: Connection,
    send: SendStream,
    recv: Option<RecvStream>,
}
impl Drop for Link {
    fn drop(&mut self) {
        self.connection
            .close(VarInt::from_u32(0), b"view disconnected");
    }
}

async fn dial(host: &str, port: u16, path: &str, hashes: Vec<[u8; 32]>) -> Result<Link> {
    validate_hashes(&hashes)?;
    let config = ClientConfig::builder()
        .with_bind_default()
        .with_server_certificate_hashes(hashes.into_iter().map(Sha256Digest::new))
        .keep_alive_interval(Some(Duration::from_secs(3)))
        .build();
    let endpoint = Endpoint::client(config)?;
    let host = if host.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    let mut url = url::Url::parse("https://placeholder.invalid")?;
    url.set_host(Some(&host))
        .map_err(|_| anyhow::anyhow!("invalid QUIC host"))?;
    url.set_port(Some(port))
        .map_err(|_| anyhow::anyhow!("invalid QUIC port"))?;
    url.set_path(path);
    let connection = endpoint.connect(url.as_str()).await?;
    let (send, recv) = connection.open_bi().await?.await?;
    Ok(Link {
        _endpoint: endpoint,
        connection,
        send,
        recv: Some(recv),
    })
}

#[derive(Debug)]
struct FramingViolation;
impl std::fmt::Display for FramingViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid or oversized server frame")
    }
}
impl std::error::Error for FramingViolation {}

// Security/protocol rejection is fatal, unlike temporary path failure.
fn retryable(error: &anyhow::Error) -> bool {
    use wtransport::error::{ConnectingError, ConnectionError};
    fn connection(error: &ConnectionError) -> bool {
        matches!(
            error,
            ConnectionError::TimedOut
                | ConnectionError::LocallyClosed
                | ConnectionError::ApplicationClosed(_)
        )
    }
    if error.is::<FramingViolation>() {
        return false;
    }
    if let Some(error) = error.downcast_ref::<ConnectingError>() {
        return match error {
            ConnectingError::DnsLookup(_)
            | ConnectingError::DnsNotFound
            | ConnectingError::EndpointStopping => true,
            ConnectingError::ConnectionError(error) => connection(error),
            _ => false,
        };
    }
    error
        .downcast_ref::<ConnectionError>()
        .is_none_or(connection)
}

async fn read_frame(recv: &mut RecvStream, max: usize) -> Result<Vec<u8>> {
    let mut header = [0; 4];
    recv.read_exact(&mut header).await?;
    let len = quosh_proto::peek_len(&header)
        .map_err(|_| FramingViolation)?
        .unwrap();
    if len > max {
        return Err(FramingViolation.into());
    }
    let mut bytes = vec![0; 4 + len];
    bytes[..4].copy_from_slice(&header);
    recv.read_exact(&mut bytes[4..]).await?;
    Ok(bytes)
}
fn validate_hashes(hashes: &[[u8; 32]]) -> Result<()> {
    ensure!(
        !hashes.is_empty() && hashes.len() <= quosh_proto::MAX_CHAIN_HASHES,
        "invalid certificate hash window"
    );
    Ok(())
}

/// One independently navigating client capability. Clone the Arc, not its
/// authority, to share it between local UI tasks. Lost RPC replies are not
/// automatically retried: the operation may already have committed.
pub struct Client {
    host: String,
    port: u16,
    token: [u8; 32],
    hashes: Arc<Mutex<Vec<[u8; 32]>>>,
    control: Mutex<Option<Link>>,
    // Retain sequencing while navigating away; never create a fresh core for
    // an already-used ticket merely because the physical stream was detached.
    views: Mutex<std::collections::BTreeMap<TabId, ([u8; 16], View)>>,
}
impl Client {
    pub async fn connect(host: String, bootstrap: Bootstrap) -> Result<Arc<Self>> {
        ensure!(
            bootstrap.protocol == VERSION,
            "unsupported Sheepdog Control Plane version"
        );
        ensure!(bootstrap.port != 0, "invalid server port");
        validate_hashes(&bootstrap.hashes)?;
        let client = Arc::new(Self {
            host,
            port: bootstrap.port,
            token: bootstrap.token,
            hashes: Arc::new(Mutex::new(bootstrap.hashes)),
            control: Mutex::new(None),
            views: Mutex::new(std::collections::BTreeMap::new()),
        });
        *client.control.lock().await = Some(client.control_link().await?);
        Ok(client)
    }
    async fn control_link(&self) -> Result<Link> {
        tokio::time::timeout(DEADLINE, async {
            let mut link = dial(
                &self.host,
                self.port,
                CONTROL_PATH,
                self.hashes.lock().await.clone(),
            )
            .await?;
            link.send
                .write_all(&quosh_proto::encode_frame(AUTH, &self.token))
                .await?;
            let frame = read_frame(link.recv.as_mut().unwrap(), MAX_REPLY).await?;
            if frame[4] == quosh_proto::MSG_ERROR {
                bail!("{}", String::from_utf8_lossy(&frame[5..]));
            }
            ensure!(frame[4] == AUTH_OK, "expected authentication reply");
            let ok: AuthOk = serde_json::from_slice(&frame[5..])?;
            ensure!(ok.protocol == VERSION, "unsupported Control Plane version");
            validate_hashes(&ok.hashes)?;
            *self.hashes.lock().await = ok.hashes;
            Ok(link)
        })
        .await?
    }
    pub async fn request(&self, request: Request) -> Result<Response> {
        let bytes = serde_json::to_vec(&request)?;
        ensure!(bytes.len() < MAX_REQUEST, "request exceeds budget");
        let mut guard = self.control.lock().await;
        // Take ownership before any cancellable I/O. Cancellation drops the
        // stream; a subsequent RPC never resumes a half-consumed frame.
        let mut link = match guard.take() {
            Some(link) if link.connection.quic_connection().close_reason().is_none() => link,
            _ => self.control_link().await?,
        };
        let result = tokio::time::timeout(DEADLINE, async {
            link.send
                .write_all(&quosh_proto::encode_frame(REQUEST, &bytes))
                .await?;
            let frame = read_frame(link.recv.as_mut().unwrap(), MAX_REPLY).await?;
            if frame[4] == quosh_proto::MSG_ERROR {
                bail!("{}", String::from_utf8_lossy(&frame[5..]));
            }
            ensure!(frame[4] == REPLY, "expected Control Plane reply");
            let reply: Response = serde_json::from_slice(&frame[5..])?;
            if let Response::Error(error) = reply {
                bail!("{error}");
            }
            Ok(reply)
        })
        .await;
        match result {
            Ok(Ok(reply)) => {
                if let Response::Snapshot { tabs, .. } = &reply {
                    self.views.lock().await.retain(|tab, (_, view)| {
                        let live = tabs.iter().any(|t| {
                            t.id == *tab
                                && matches!(t.terminal, sheepdog_core::TerminalState::Running(_))
                        });
                        if !tabs.iter().any(|t| t.id == *tab) {
                            view.stop();
                        }
                        live
                    });
                }
                *guard = Some(link);
                Ok(reply)
            }
            Ok(Err(error)) => Err(error),
            Err(error) => Err(error.into()),
        }
    }
    pub async fn attach(
        self: &Arc<Self>,
        tab: TabId,
        geometry: Geometry,
        predict_never: bool,
    ) -> Result<View> {
        Geometry::new(geometry.cols, geometry.rows).map_err(|e| anyhow::anyhow!("{e:?}"))?;
        let Response::Attachment(ticket) = self.request(Request::Attach { tab, geometry }).await?
        else {
            bail!("expected attachment ticket");
        };
        ensure!(ticket.tab == tab, "attachment returned the wrong Tab");
        let mut views = self.views.lock().await;
        if let Some((session, view)) = views.get(&tab)
            && *session == ticket.session_id
        {
            ensure!(
                !matches!(
                    view.snapshot().status,
                    ViewStatus::Stopped | ViewStatus::Failed(_) | ViewStatus::Ended(_)
                ),
                "view sequencing stopped; bootstrap a new client rather than replaying as fresh input"
            );
            ensure!(
                view.predict_never() == predict_never,
                "prediction preference is fixed for this view; bootstrap a new client to change it"
            );
            view.resize(geometry)?;
            view.resume();
            return Ok(view.clone());
        }
        ensure!(
            views.contains_key(&tab) || views.len() < 256,
            "view cache full; refresh organization before attaching"
        );
        let session = ticket.session_id;
        let view = View::spawn(self.clone(), ticket, geometry, predict_never);
        if let Some((_, old)) = views.insert(tab, (session, view.clone())) {
            old.stop();
        }
        Ok(view)
    }
    /// Explicit client quit, never a destructive Tab/Space close.
    pub async fn disconnect(&self) -> Result<()> {
        for (_, view) in self.views.lock().await.values() {
            view.stop();
        }
        ensure!(
            matches!(self.request(Request::Disconnect).await?, Response::Closed),
            "expected disconnect reply"
        );
        self.control.lock().await.take();
        Ok(())
    }
}
