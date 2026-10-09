use crate::{Daemon, protocol::*};
use anyhow::{Context, Result, bail};
use quosh_proto::{
    Hello, HelloOk, MSG_ACK_STATE, MSG_HANGUP, MSG_HELLO, MSG_INPUT, MSG_PING, MSG_RESIZE,
    MSG_SHARED_SCREEN, PROTOCOL_VERSION, SharedScreen, decode_input, decode_resize, decode_u64,
    encode_error, encode_exit, encode_frame, encode_input_ack, encode_pong,
};
use sheepdog_core::Geometry;
use sheepdog_runtime::{Connection as ViewConnection, InputError, InputResult};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use wtransport::{Connection, RecvStream, SendStream, VarInt, endpoint::IncomingSession};

pub async fn read_frame(recv: &mut RecvStream, limit: usize) -> Result<(u8, Vec<u8>)> {
    let mut length = [0; 4];
    recv.read_exact(&mut length).await?;
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > limit || length > quosh_proto::MAX_FRAME {
        bail!("invalid frame length");
    }
    let mut bytes = vec![0; length];
    recv.read_exact(&mut bytes).await?;
    Ok((bytes[0], bytes[1..].to_vec()))
}

async fn write(send: &mut SendStream, bytes: &[u8]) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(5), send.write_all(bytes))
        .await
        .context("stream write deadline")??;
    Ok(())
}

async fn json<T: serde::Serialize>(send: &mut SendStream, kind: u8, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() + 1 > MAX_REPLY {
        bail!("reply too large");
    }
    write(send, &encode_frame(kind, &bytes)).await
}

pub async fn handle(incoming: IncomingSession, daemon: Arc<Daemon>) -> Result<()> {
    let request = tokio::time::timeout(Duration::from_secs(5), incoming).await??;
    let path = request.path().to_owned();
    if path != CONTROL_PATH && path != TERMINAL_PATH {
        request.not_found().await;
        bail!("unknown path");
    }
    let connection = request.accept().await?;
    let (mut send, mut receive) =
        tokio::time::timeout(Duration::from_secs(5), connection.accept_bi()).await??;
    let result = if path == CONTROL_PATH {
        control(&daemon, &mut send, &mut receive).await
    } else {
        terminal(&daemon, &connection, &mut send, receive).await
    };
    if let Err(error) = &result {
        let _ = write(&mut send, &encode_error(1, &format!("{error:#}"))).await;
        let _ = tokio::time::timeout(Duration::from_millis(150), send.finish()).await;
    }
    connection.close(VarInt::from_u32(0), b"attachment ended");
    result
}

async fn control(daemon: &Daemon, send: &mut SendStream, recv: &mut RecvStream) -> Result<()> {
    let (kind, bytes) =
        tokio::time::timeout(Duration::from_secs(5), read_frame(recv, MAX_REQUEST)).await??;
    if kind != AUTH || bytes.len() != 32 {
        bail!("expected control credential");
    }
    let token: [u8; 32] = bytes.try_into().unwrap();
    daemon.state.authenticate(token).await?;
    json(
        send,
        AUTH_OK,
        &AuthOk {
            protocol: VERSION,
            hashes: daemon.chain.forward_hashes()?,
        },
    )
    .await?;
    loop {
        let (kind, bytes) = read_frame(recv, MAX_REQUEST).await?;
        if kind != REQUEST {
            bail!("expected control request");
        }
        let request: Request = serde_json::from_slice(&bytes)?;
        let response = match daemon.state.execute(token, request).await {
            Ok(response) => response,
            Err(error) => Response::Error(format!("{error:#}")),
        };
        json(send, REPLY, &response).await?;
    }
}

struct AbortReader(tokio::task::AbortHandle);
impl Drop for AbortReader {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn terminal(
    daemon: &Daemon,
    conn: &Connection,
    send: &mut SendStream,
    mut recv: RecvStream,
) -> Result<()> {
    let (kind, bytes) =
        tokio::time::timeout(Duration::from_secs(5), read_frame(&mut recv, 1024)).await??;
    if kind != MSG_HELLO {
        bail!("expected Quosh Hello");
    }
    let hello = Hello::decode(&bytes)?;
    if hello.protocol != PROTOCOL_VERSION {
        bail!("Quosh protocol mismatch");
    }
    let geometry = Geometry::new(hello.cols, hello.rows)
        .map_err(|_| anyhow::anyhow!("invalid terminal dimensions"))?;
    let view = daemon.state.view(hello.session_id, hello.token).await?;
    let current: ViewConnection = {
        let mut stored = view.connection.lock().await;
        let claimed = view.terminal.resume(*stored).await?;
        *stored = claimed;
        claimed
    };
    let result: Result<()> = async {
        view.terminal.resize(current, geometry).await?;
        let first = view.terminal.snapshot();
        write(send, &HelloOk { protocol: PROTOCOL_VERSION, session_id: hello.session_id, version: first.version, cols: first.geometry.cols, rows: first.geometry.rows }.encode()).await?;
        // A separate bounded reader retains partial-frame progress while screen
        // changes wake the select loop. Never cancel read_exact in that loop.
        let (frames, mut incoming) = mpsc::channel(32);
        let reader = tokio::spawn(async move {
            loop {
                let frame = read_frame(&mut recv, sheepdog_runtime::MAX_INPUT + 64).await;
                let failed = frame.is_err();
                if frames.send(frame).await.is_err() || failed { break; }
            }
        });
        let _reader = AbortReader(reader.abort_handle());
        let mut snapshots = view.terminal.subscribe();
        let mut last = 0;
        let mut pending: Option<(u64, Vec<u8>)> = None;
        let mut retry = tokio::time::interval(Duration::from_millis(20));
        loop {
            let snapshot = snapshots.borrow_and_update().clone();
            let checkpoint = snapshot.checkpoints.get(&current.attachment_id()).context("attachment retired")?;
            if checkpoint.connection_epoch != current.epoch() || !checkpoint.connected { bail!("transport replaced"); }
            if snapshot.version > last {
                let screen = snapshot.screen(current).context("stale terminal view")?;
                let shared = SharedScreen { prediction_epoch: checkpoint.prediction_epoch, prediction_allowed: checkpoint.prediction_allowed, screen };
                write(send, &encode_frame(MSG_SHARED_SCREEN, &shared.encode_compressed()?)).await?;
                if checkpoint.accepted > 0 { write(send, &encode_input_ack(checkpoint.accepted)).await?; }
                last = snapshot.version;
            }
            if let Some(exit) = &snapshot.exit {
                write(send, &encode_exit(exit.code.unwrap_or_else(|| -exit.signal.unwrap_or(1)))).await?;
                let _ = tokio::time::timeout(Duration::from_millis(150), send.finish()).await;
                break;
            }
            tokio::select! {
                _ = conn.closed() => break,
                change = snapshots.changed() => { change.context("terminal owner stopped")?; }
                _ = retry.tick() => {
                    daemon.state.authenticate(view.parent).await?;
                    if let Some((seq, bytes)) = pending.take() {
                        match view.terminal.input(current, seq, bytes.clone()).await {
                            Ok(InputResult::Accepted | InputResult::Replay) => {},
                            Err(error) if error.downcast_ref::<InputError>() == Some(&InputError::Busy) => pending = Some((seq, bytes)),
                            Err(error) => return Err(error),
                        }
                    }
                }
                frame = incoming.recv(), if pending.is_none() => {
                    let (kind, bytes) = frame.context("terminal reader stopped")??;
                    match kind {
                        MSG_INPUT => {
                            let (seq, input) = decode_input(&bytes)?;
                            match view.terminal.input(current, seq, input.clone()).await {
                                Ok(InputResult::Accepted | InputResult::Replay) => {},
                                Err(error) if error.downcast_ref::<InputError>() == Some(&InputError::Busy) => pending = Some((seq, input)),
                                Err(error) => return Err(error),
                            }
                        }
                        MSG_RESIZE => {
                            let (cols, rows) = decode_resize(&bytes)?;
                            let geometry = Geometry::new(cols, rows).map_err(|_| anyhow::anyhow!("invalid dimensions"))?;
                            view.terminal.resize(current, geometry).await?;
                        }
                        MSG_ACK_STATE => { let _ = decode_u64(&bytes)?; }
                        MSG_PING if bytes.is_empty() => write(send, &encode_pong()).await?,
                        // Closing a view is not closing the server-owned Tab.
                        MSG_HANGUP if bytes.is_empty() => break,
                        _ => bail!("unexpected terminal message"),
                    }
                }
            }
        }
        Ok(())
    }.await;
    let _ = view.terminal.detach(current).await;
    result
}
