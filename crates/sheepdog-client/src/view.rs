use super::{Client, DEADLINE, Link, dial, read_frame, retryable};
use anyhow::{Result, bail, ensure};
use blit_remote::FrameState;
use quosh_client::{Tick, UNACKED_CAP};
use sheepdog_core::Geometry;
use sheepdog_proto::{TERMINAL_PATH, Ticket};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, mpsc, watch};

const MAX_INPUT: usize = 64 * 1024;
const COMMANDS: usize = 32;
const FRAMES: usize = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewStatus {
    Connecting,
    Connected,
    Offline,
    Paused,
    Ended(i32),
    Stopped,
    Failed(String),
}
#[derive(Clone)]
pub struct ViewSnapshot {
    pub frame: Option<Arc<FrameState>>,
    pub status: ViewStatus,
    pub unacked_bytes: usize,
    pub mode: u16,
}
enum Command {
    Input(Vec<u8>),
    Resize(Geometry),
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Desired {
    Active,
    Paused,
    Stopped,
}
struct Route {
    host: String,
    port: u16,
    hashes: Arc<Mutex<Vec<[u8; 32]>>>,
}

/// Bounded, nonblocking UI handle. Pause detaches the physical stream but
/// retains replay/sequence state. The parent Client caches these handles;
/// Stop is irreversible and out of band, including under backpressure.
#[derive(Clone)]
pub struct View {
    never: bool,
    commands: mpsc::Sender<Command>,
    stop: watch::Sender<Desired>,
    updates: watch::Receiver<ViewSnapshot>,
}
impl View {
    pub(super) fn spawn(
        client: Arc<Client>,
        ticket: Ticket,
        geometry: Geometry,
        never: bool,
    ) -> Self {
        let (commands, receiver) = mpsc::channel(COMMANDS);
        let (stop, stopped) = watch::channel(Desired::Active);
        let (updates, published) = watch::channel(ViewSnapshot {
            frame: None,
            status: ViewStatus::Connecting,
            unacked_bytes: 0,
            mode: 0,
        });
        // No parent Arc in the task: Client -> cached View -> Client would leak.
        let route = Route {
            host: client.host.clone(),
            port: client.port,
            hashes: client.hashes.clone(),
        };
        tokio::spawn(async move {
            let mut core = quosh_client::Client::new(
                ticket.session_id,
                ticket.token,
                geometry.cols,
                geometry.rows,
                never,
            );
            let epoch = Instant::now();
            let status = run(&route, &mut core, &epoch, receiver, stopped, &updates).await;
            publish(&updates, &core, status);
        });
        Self {
            never,
            commands,
            stop,
            updates: published,
        }
    }
    pub fn predict_never(&self) -> bool {
        self.never
    }
    pub fn subscribe(&self) -> watch::Receiver<ViewSnapshot> {
        self.updates.clone()
    }
    pub fn snapshot(&self) -> ViewSnapshot {
        self.updates.borrow().clone()
    }
    pub fn input(&self, bytes: Vec<u8>) -> Result<()> {
        ensure!(*self.stop.borrow() == Desired::Active, "view is not active");
        ensure!(
            !bytes.is_empty() && bytes.len() <= MAX_INPUT,
            "invalid input size"
        );
        if self.commands.try_send(Command::Input(bytes)).is_err() {
            self.stop();
            bail!("terminal input queue full or stopped; view detached");
        }
        Ok(())
    }
    pub fn resize(&self, geometry: Geometry) -> Result<()> {
        ensure!(*self.stop.borrow() != Desired::Stopped, "view is stopped");
        Geometry::new(geometry.cols, geometry.rows).map_err(|e| anyhow::anyhow!("{e:?}"))?;
        if self.commands.try_send(Command::Resize(geometry)).is_err() {
            self.stop();
            bail!("terminal command queue full or stopped; view detached");
        }
        Ok(())
    }
    pub fn pause(&self) {
        self.set_desired(Desired::Paused);
    }
    pub fn resume(&self) {
        self.set_desired(Desired::Active);
    }
    fn set_desired(&self, desired: Desired) {
        self.stop.send_if_modified(|current| {
            if *current == Desired::Stopped || *current == desired {
                return false;
            }
            *current = desired;
            true
        });
    }
    pub fn stop(&self) {
        self.stop.send_replace(Desired::Stopped);
    }
}
fn now(epoch: &Instant) -> u64 {
    epoch.elapsed().as_millis().min(u64::MAX as u128) as u64
}
fn command(core: &mut quosh_client::Client, command: Command, epoch: &Instant) -> Result<()> {
    match command {
        Command::Input(bytes) => {
            ensure!(
                core.unacked_bytes().saturating_add(bytes.len()) <= UNACKED_CAP,
                "unacknowledged input budget exhausted; view detached"
            );
            core.queue_input(bytes, now(epoch));
        }
        Command::Resize(geometry) => core.set_size(geometry.cols, geometry.rows),
    }
    Ok(())
}
fn same_frame(a: &FrameState, b: &FrameState) -> bool {
    a == b
}
fn publish(updates: &watch::Sender<ViewSnapshot>, core: &quosh_client::Client, status: ViewStatus) {
    updates.send_if_modified(|old| {
        let frame = core.display().or_else(|| core.confirmed());
        let changed = match (&old.frame, frame) {
            (Some(a), Some(b)) => !same_frame(a, b),
            (None, None) => false,
            _ => true,
        };
        let metadata = old.status != status
            || old.unacked_bytes != core.unacked_bytes()
            || old.mode != core.mode();
        if changed {
            old.frame = frame.map(|f| Arc::new(f.clone()));
        }
        old.status = status;
        old.unacked_bytes = core.unacked_bytes();
        old.mode = core.mode();
        changed || metadata
    });
}
struct Reader(tokio::task::JoinHandle<()>);
impl Drop for Reader {
    fn drop(&mut self) {
        self.0.abort();
    }
}

enum End {
    Lost,
    Fatal(String),
    Stopped,
    Paused,
    Exited(i32),
}
async fn session(
    link: &mut Link,
    core: &mut quosh_client::Client,
    epoch: &Instant,
    commands: &mut mpsc::Receiver<Command>,
    stopped: &mut watch::Receiver<Desired>,
    updates: &watch::Sender<ViewSnapshot>,
) -> End {
    let mut recv = link.recv.take().unwrap();
    let (frames, mut incoming) = mpsc::channel(FRAMES);
    let _reader = Reader(tokio::spawn(async move {
        loop {
            let frame = read_frame(&mut recv, quosh_proto::MAX_FRAME).await;
            let failed = frame.is_err();
            if frames.send(frame).await.is_err() || failed {
                break;
            }
        }
    }));
    core.begin_connection(now(epoch));
    let mut ticker = tokio::time::interval(Duration::from_millis(20));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        match *stopped.borrow() {
            Desired::Stopped => return End::Stopped,
            Desired::Paused => return End::Paused,
            Desired::Active => {}
        }
        if core.is_hungup() {
            return End::Exited(core.exit_code());
        }
        core.pump();
        publish(
            updates,
            core,
            if core.hello_ok() {
                ViewStatus::Connected
            } else {
                ViewStatus::Connecting
            },
        );
        let bytes = core.outbound().to_vec();
        tokio::select! {
            biased;
            result = stopped.changed() => if result.is_err() { return End::Stopped; },
            message = commands.recv() => match message {
                Some(message) => if let Err(error) = command(core, message, epoch) { return End::Fatal(error.to_string()); },
                None => return End::Stopped,
            },
            frame = incoming.recv() => match frame {
                Some(Ok(frame)) => if let Err(error) = core.recv_control(&frame, now(epoch)) { return End::Fatal(error.to_string()); },
                Some(Err(error)) if !retryable(&error) => return End::Fatal(error.to_string()),
                _ => return End::Lost,
            },
            written = link.send.write(&bytes), if !bytes.is_empty() => match written {
                Ok(0) | Err(_) => return End::Lost,
                Ok(n) => core.advance_outbound(n),
            },
            _ = ticker.tick() => if core.tick(now(epoch)) == Tick::LinkDead { return End::Lost; },
        }
    }
}
async fn run(
    route: &Route,
    core: &mut quosh_client::Client,
    epoch: &Instant,
    mut commands: mpsc::Receiver<Command>,
    mut stopped: watch::Receiver<Desired>,
    updates: &watch::Sender<ViewSnapshot>,
) -> ViewStatus {
    let mut failures = 0;
    loop {
        while *stopped.borrow() != Desired::Active {
            if *stopped.borrow() == Desired::Stopped {
                return ViewStatus::Stopped;
            }
            publish(updates, core, ViewStatus::Paused);
            tokio::select! {
                result = stopped.changed() => if result.is_err() { return ViewStatus::Stopped; },
                message = commands.recv() => match message {
                    Some(message) => if let Err(error) = command(core, message, epoch) { return ViewStatus::Failed(error.to_string()); },
                    None => return ViewStatus::Stopped,
                },
            }
            core.reset();
        }
        let connect = tokio::time::timeout(DEADLINE, async {
            let hashes = route.hashes.lock().await.clone();
            dial(&route.host, route.port, TERMINAL_PATH, hashes).await
        });
        tokio::pin!(connect);
        let result = loop {
            if *stopped.borrow() == Desired::Stopped {
                return ViewStatus::Stopped;
            }
            if *stopped.borrow() == Desired::Paused {
                break None;
            }
            tokio::select! {
                biased;
                result = stopped.changed() => if result.is_err() { return ViewStatus::Stopped; },
                message = commands.recv() => match message {
                    Some(message) => if let Err(error) = command(core, message, epoch) { return ViewStatus::Failed(error.to_string()); },
                    None => return ViewStatus::Stopped,
                },
                result = &mut connect => break Some(result),
            }
            core.reset();
            publish(updates, core, ViewStatus::Offline);
        };
        match result {
            None => continue,
            Some(Ok(Ok(mut link))) => {
                let end =
                    session(&mut link, core, epoch, &mut commands, &mut stopped, updates).await;
                let established = core.hello_ok();
                core.end_connection();
                core.reset();
                match end {
                    End::Stopped => return ViewStatus::Stopped,
                    End::Paused => continue,
                    End::Exited(code) => return ViewStatus::Ended(code),
                    End::Fatal(error) => return ViewStatus::Failed(error),
                    End::Lost => {
                        if established {
                            failures = 0;
                        } else {
                            failures += 1;
                        }
                    }
                }
            }
            Some(Ok(Err(error))) if !retryable(&error) => {
                return ViewStatus::Failed(error.to_string());
            }
            _ => {
                failures += 1;
            }
        }
        if failures >= 5 {
            return ViewStatus::Failed(
                "five terminal handshake failures; re-bootstrap through SSH".into(),
            );
        }
        publish(updates, core, ViewStatus::Offline);
        let retry = tokio::time::sleep(Duration::from_millis(500));
        tokio::pin!(retry);
        loop {
            if *stopped.borrow() == Desired::Stopped {
                return ViewStatus::Stopped;
            }
            if *stopped.borrow() == Desired::Paused {
                break;
            }
            tokio::select! {
                biased;
                result = stopped.changed() => if result.is_err() { return ViewStatus::Stopped; },
                message = commands.recv() => match message {
                    Some(message) => if let Err(error) = command(core, message, epoch) { return ViewStatus::Failed(error.to_string()); },
                    None => return ViewStatus::Stopped,
                },
                _ = &mut retry => break,
            }
            core.reset();
            publish(updates, core, ViewStatus::Offline);
        }
    }
}
