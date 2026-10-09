#[cfg(test)]
mod tests;

use crate::protocol::{Bootstrap, Request, Response, Ticket, VERSION};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

fn credential_key(token: [u8; 32]) -> [u8; 32] {
    Sha256::digest(token).into()
}
use anyhow::{Context, Result, bail};
use sheepdog_core::{
    ClientId, EndReason, Geometry, Organization, Server, TerminalId, TerminalState,
};
use sheepdog_runtime::{Connection, Program, Terminal};
use std::collections::BTreeMap;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

// Native SSH-issued capabilities follow Quosh's seven-day roaming lifetime.
// Browser/passkey session tokens will have their separate short-lived policy.
const IDLE: Duration = Duration::from_secs(7 * 24 * 3600);
const MAX_CLIENTS: usize = 128;
const MAX_SPACES: usize = 128;
const MAX_TABS: usize = 256;

struct Client {
    id: ClientId,
    seen: Instant,
}

pub(crate) struct View {
    pub ticket: Ticket,
    pub terminal: Terminal,
    pub connection: Mutex<Connection>,
    pub parent: [u8; 32],
}

struct Inner {
    model: Server,
    terminals: BTreeMap<TerminalId, Terminal>,
    clients: BTreeMap<[u8; 32], Client>,
    views: BTreeMap<[u8; 16], Arc<View>>,
}

pub struct State {
    inner: Mutex<Inner>,
    directory: PathBuf,
    shell: PathBuf,
}

fn domain<T>(value: std::result::Result<T, sheepdog_core::Error>) -> Result<T> {
    value.map_err(|e| anyhow::anyhow!("{e:?}"))
}

impl State {
    pub fn load(directory: PathBuf, shell: PathBuf) -> Result<Arc<Self>> {
        let path = directory.join("organization.json");
        let model = if path.exists() {
            if std::fs::metadata(&path)?.len() > 4 * 1024 * 1024 {
                bail!("organization file too large");
            }
            let saved: Organization = serde_json::from_slice(&std::fs::read(path)?)?;
            if saved.spaces.len() > MAX_SPACES || saved.tabs.len() > MAX_TABS {
                bail!("organization limits exceeded");
            }
            domain(Server::restore(saved, domain(Geometry::new(80, 24))?))?
        } else {
            Server::default()
        };
        Ok(Arc::new(Self {
            inner: Mutex::new(Inner {
                model,
                terminals: BTreeMap::new(),
                clients: BTreeMap::new(),
                views: BTreeMap::new(),
            }),
            directory,
            shell,
        }))
    }

    fn save(&self, model: &Server) -> Result<()> {
        let bytes = serde_json::to_vec(&model.snapshot())?;
        let temporary = self
            .directory
            .join(format!("organization-{}.tmp", rand::random::<u64>()));
        let result = (|| -> Result<()> {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            std::fs::rename(&temporary, self.directory.join("organization.json"))?;
            std::fs::File::open(&self.directory)?.sync_all()?;
            Ok(())
        })();
        let _ = std::fs::remove_file(temporary);
        result
    }

    /// Only the SO_PEERCRED-authenticated bootstrap socket calls this.
    pub async fn bootstrap(&self, port: u16, hashes: Vec<[u8; 32]>) -> Result<Bootstrap> {
        self.prune().await;
        let mut inner = self.inner.lock().await;
        if inner.clients.len() >= MAX_CLIENTS {
            bail!("client limit reached");
        }
        let id = domain(inner.model.connect(domain(Geometry::new(80, 24))?))?;
        let token = rand::random();
        inner.clients.insert(
            credential_key(token),
            Client {
                id,
                seen: Instant::now(),
            },
        );
        Ok(Bootstrap {
            protocol: VERSION,
            port,
            hashes,
            token,
        })
    }

    pub async fn authenticate(&self, token: [u8; 32]) -> Result<()> {
        let mut inner = self.inner.lock().await;
        Self::client(&mut inner, token)?;
        Ok(())
    }

    fn client(inner: &mut Inner, token: [u8; 32]) -> Result<ClientId> {
        let client = inner
            .clients
            .get_mut(&credential_key(token))
            .context("unknown credential")?;
        if client.seen.elapsed() >= IDLE {
            bail!("credential expired; bootstrap again over SSH");
        }
        client.seen = Instant::now();
        Ok(client.id)
    }

    pub async fn prune(&self) {
        let mut inner = self.inner.lock().await;
        let expired: Vec<_> = inner
            .clients
            .iter()
            .filter(|(_, c)| c.seen.elapsed() >= IDLE)
            .map(|(token, _)| *token)
            .collect();
        for token in expired {
            if let Some(client) = inner.clients.remove(&token) {
                inner.model.disconnect(client.id);
            }
        }
        let stale: Vec<_> = inner
            .views
            .iter()
            .filter(|(_, view)| !inner.clients.contains_key(&credential_key(view.parent)))
            .map(|(id, v)| (*id, v.clone()))
            .collect();
        for (id, view) in stale {
            let c = *view.connection.lock().await;
            let _ = view.terminal.detach(c).await;
            if view.terminal.retire(c).await.is_ok() || view.terminal.snapshot().exit.is_some() {
                inner.views.remove(&id);
            }
        }
    }

    pub(crate) async fn view(&self, id: [u8; 16], token: [u8; 32]) -> Result<Arc<View>> {
        let mut inner = self.inner.lock().await;
        let view = inner
            .views
            .get(&id)
            .cloned()
            .context("unknown terminal attachment")?;
        if !bool::from(view.ticket.token.ct_eq(&token)) {
            bail!("bad terminal credential");
        }
        Self::client(&mut inner, view.parent)?;
        Ok(view)
    }

    pub async fn execute(&self, token: [u8; 32], request: Request) -> Result<Response> {
        let mut inner = self.inner.lock().await;
        let client = Self::client(&mut inner, token)?;
        // Reconcile authoritative owner geometry and natural exits. Never store
        // a process handle in persistent organization or implicitly restart it.
        let updates: Vec<_> = inner
            .model
            .tabs()
            .filter_map(|tab| {
                let TerminalState::Running(id) = tab.terminal else {
                    return None;
                };
                Some((tab.id, id, inner.terminals.get(&id)?.snapshot()))
            })
            .collect();
        for (tab, id, snapshot) in updates {
            domain(inner.model.terminal_geometry(tab, id, snapshot.geometry))?;
            if let Some(exit) = &snapshot.exit {
                let code = exit.code.unwrap_or_else(|| -exit.signal.unwrap_or(1));
                inner.model.ended(tab, id, EndReason::Exited(code));
                inner.terminals.remove(&id);
                inner.views.retain(|_, view| view.ticket.tab != tab);
            }
        }
        match request {
            Request::Disconnect => {
                inner.clients.remove(&credential_key(token));
                inner.model.disconnect(client);
                drop(inner);
                self.prune().await;
                Ok(Response::Closed)
            }
            Request::Snapshot => Ok(Response::Snapshot {
                spaces: inner.model.spaces().cloned().collect(),
                tabs: inner.model.tabs().cloned().collect(),
                selected: domain(inner.model.selected(client))?,
            }),
            Request::CreateSpace { name, directory } => {
                if name.is_empty()
                    || name.len() > 128
                    || name.chars().any(char::is_control)
                    || !directory.is_absolute()
                    || !directory.is_dir()
                    || directory.as_os_str().len() > 4096
                {
                    bail!("invalid Space name or directory");
                }
                if inner.model.spaces().count() >= MAX_SPACES {
                    bail!("Space limit reached");
                }
                let mut staged = inner.model.clone();
                let id = domain(staged.create_space(name, directory))?;
                self.save(&staged)?;
                inner.model = staged;
                Ok(Response::Space(
                    inner.model.spaces().find(|s| s.id == id).unwrap().clone(),
                ))
            }
            Request::CreateTab {
                space,
                label,
                geometry,
            } => {
                if label.is_empty() || label.len() > 128 || label.chars().any(char::is_control) {
                    bail!("invalid Tab label");
                }
                if inner.model.tabs().count() >= MAX_TABS {
                    bail!("Tab limit reached");
                }
                let mut staged = inner.model.clone();
                let launch = domain(staged.create_tab(space, label, geometry))?;
                self.save(&staged)?;
                inner.model = staged;
                self.launch(&mut inner, launch)
            }
            Request::ReopenTab { tab } => {
                let launch = domain(inner.model.reopen_tab(tab))?;
                self.launch(&mut inner, launch)
            }
            Request::Attach { tab, geometry } => {
                domain(Geometry::new(geometry.cols, geometry.rows))?;
                let record = inner.model.tab(tab).context("unknown Tab")?;
                let TerminalState::Running(id) = record.terminal else {
                    bail!("terminal ended; explicitly reopen the Tab");
                };
                let terminal = inner
                    .terminals
                    .get(&id)
                    .context("terminal owner missing")?
                    .clone();
                domain(inner.model.select(client, tab))?;
                domain(inner.model.resize_client(client, geometry))?;
                if let Some(view) = inner
                    .views
                    .values()
                    .find(|v| {
                        v.parent == token
                            && v.ticket.tab == tab
                            && v.terminal.snapshot().exit.is_none()
                    })
                    .cloned()
                {
                    return Ok(Response::Attachment(view.ticket.clone()));
                }
                let c = terminal.attach().await?;
                terminal.resize(c, geometry).await?;
                terminal.detach(c).await?;
                let ticket = Ticket {
                    tab,
                    session_id: rand::random(),
                    token: rand::random(),
                };
                inner.views.insert(
                    ticket.session_id,
                    Arc::new(View {
                        ticket: ticket.clone(),
                        terminal,
                        connection: Mutex::new(c),
                        parent: token,
                    }),
                );
                Ok(Response::Attachment(ticket))
            }
            Request::CloseTab { tab, confirmed } => {
                if !confirmed {
                    bail!("closing a Tab requires confirmation");
                }
                let mut staged = inner.model.clone();
                let stop = domain(staged.close_tab(tab))?;
                self.save(&staged)?;
                inner.model = staged;
                if let Some(id) = stop
                    && let Some(terminal) = inner.terminals.remove(&id)
                {
                    terminal.close().await?;
                }
                inner.views.retain(|_, v| v.ticket.tab != tab);
                Ok(Response::Closed)
            }
            Request::CloseSpace { space, confirmed } => {
                if !confirmed {
                    bail!("closing a Space requires confirmation");
                }
                let mut staged = inner.model.clone();
                let stop = domain(staged.close_space(space))?;
                self.save(&staged)?;
                inner.model = staged;
                let mut closing = tokio::task::JoinSet::new();
                for id in stop {
                    if let Some(terminal) = inner.terminals.remove(&id) {
                        closing.spawn(async move { terminal.close().await });
                    }
                }
                while let Some(result) = closing.join_next().await {
                    result??;
                }
                let tabs: std::collections::BTreeSet<_> =
                    inner.model.tabs().map(|t| t.id).collect();
                inner.views.retain(|_, v| tabs.contains(&v.ticket.tab));
                Ok(Response::Closed)
            }
        }
    }

    fn launch(&self, inner: &mut Inner, launch: sheepdog_core::Launch) -> Result<Response> {
        match Terminal::spawn(Program {
            executable: self.shell.clone(),
            args: vec!["-l".into()],
            directory: launch.directory,
            geometry: launch.geometry,
        }) {
            Ok(terminal) => {
                inner.model.launched(launch.tab, launch.terminal);
                inner.terminals.insert(launch.terminal, terminal);
            }
            Err(error) => {
                inner.model.ended(
                    launch.tab,
                    launch.terminal,
                    EndReason::LaunchFailed(format!("{error:#}")),
                );
            }
        }
        Ok(Response::Tab(inner.model.tab(launch.tab).unwrap().clone()))
    }

    pub async fn shutdown(&self) {
        let mut inner = self.inner.lock().await;
        let terminals = std::mem::take(&mut inner.terminals);
        let mut closing = tokio::task::JoinSet::new();
        for (_, terminal) in terminals {
            closing.spawn(async move { terminal.close().await });
        }
        while closing.join_next().await.is_some() {}
    }
}
