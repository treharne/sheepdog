//! Server-owned organization and lifecycle, independent of transport and UI.
//! Runtime adapters execute launch/stop requests; client disconnection never
//! emits a stop request. Snapshots contain organization, never live processes.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SpaceId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TabId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClientId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TerminalId(pub u64);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EndReason {
    ServerRestart,
    Exited(i32),
    LaunchFailed(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminalState {
    Starting(TerminalId),
    Running(TerminalId),
    Ended(EndReason),
}

impl TerminalState {
    fn live_id(&self) -> Option<TerminalId> {
        match self {
            Self::Starting(id) | Self::Running(id) => Some(*id),
            Self::Ended(_) => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Space {
    pub id: SpaceId,
    pub name: String,
    pub default_directory: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tab {
    pub id: TabId,
    pub space: SpaceId,
    pub label: String,
    pub directory: PathBuf,
    pub terminal: TerminalState,
    pub geometry: Geometry,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Geometry {
    pub cols: u16,
    pub rows: u16,
}

impl Geometry {
    pub fn new(cols: u16, rows: u16) -> Result<Self, Error> {
        if cols == 0 || rows == 0 || cols > 500 || rows > 300 {
            return Err(Error::InvalidGeometry);
        }
        Ok(Self { cols, rows })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    pub tab: TabId,
    pub terminal: TerminalId,
    pub directory: PathBuf,
    pub geometry: Geometry,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedTab {
    pub id: TabId,
    pub space: SpaceId,
    pub label: String,
    pub directory: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Organization {
    pub spaces: Vec<Space>,
    pub tabs: Vec<SavedTab>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    MissingSpace,
    MissingTab,
    MissingClient,
    AlreadyLive,
    InvalidGeometry,
    InvalidSnapshot,
    IdExhausted,
}

#[derive(Clone, Debug)]
struct Client {
    selected: Option<TabId>,
    geometry: Geometry,
}

#[derive(Clone, Debug, Default)]
pub struct Server {
    spaces: BTreeMap<SpaceId, Space>,
    tabs: BTreeMap<TabId, Tab>,
    clients: BTreeMap<ClientId, Client>,
    next_id: u64,
}

impl Server {
    fn allocate(&mut self) -> Result<u64, Error> {
        self.next_id = self.next_id.checked_add(1).ok_or(Error::IdExhausted)?;
        Ok(self.next_id)
    }

    pub fn spaces(&self) -> impl Iterator<Item = &Space> {
        self.spaces.values()
    }

    pub fn tabs(&self) -> impl Iterator<Item = &Tab> {
        self.tabs.values()
    }

    pub fn tab(&self, id: TabId) -> Option<&Tab> {
        self.tabs.get(&id)
    }

    pub fn create_space(&mut self, name: String, directory: PathBuf) -> Result<SpaceId, Error> {
        let id = SpaceId(self.allocate()?);
        self.spaces.insert(
            id,
            Space {
                id,
                name,
                default_directory: directory,
            },
        );
        Ok(id)
    }

    pub fn create_tab(
        &mut self,
        space: SpaceId,
        label: String,
        geometry: Geometry,
    ) -> Result<Launch, Error> {
        Geometry::new(geometry.cols, geometry.rows)?;
        let directory = self
            .spaces
            .get(&space)
            .ok_or(Error::MissingSpace)?
            .default_directory
            .clone();
        let id = TabId(self.allocate()?);
        let terminal = TerminalId(self.allocate()?);
        self.tabs.insert(
            id,
            Tab {
                id,
                space,
                label,
                directory: directory.clone(),
                terminal: TerminalState::Starting(terminal),
                geometry,
            },
        );
        Ok(Launch {
            tab: id,
            terminal,
            directory,
            geometry,
        })
    }

    /// An explicit action, never called automatically during restore.
    pub fn reopen_tab(&mut self, id: TabId) -> Result<Launch, Error> {
        let tab = self.tabs.get(&id).ok_or(Error::MissingTab)?;
        if tab.terminal.live_id().is_some() {
            return Err(Error::AlreadyLive);
        }
        let terminal = TerminalId(self.allocate()?);
        let tab = self.tabs.get_mut(&id).expect("validated tab");
        tab.terminal = TerminalState::Starting(terminal);
        Ok(Launch {
            tab: id,
            terminal,
            directory: tab.directory.clone(),
            geometry: tab.geometry,
        })
    }

    /// Ignore stale runtime events after a Tab was closed or reopened.
    pub fn launched(&mut self, id: TabId, terminal: TerminalId) -> bool {
        let Some(tab) = self.tabs.get_mut(&id) else {
            return false;
        };
        if tab.terminal != TerminalState::Starting(terminal) {
            return false;
        }
        tab.terminal = TerminalState::Running(terminal);
        true
    }

    pub fn ended(&mut self, id: TabId, terminal: TerminalId, reason: EndReason) -> bool {
        let Some(tab) = self.tabs.get_mut(&id) else {
            return false;
        };
        if tab.terminal.live_id() != Some(terminal) {
            return false;
        }
        tab.terminal = TerminalState::Ended(reason);
        true
    }

    /// Reconcile an authoritative owner update without changing any client's
    /// independently remembered viewport. Stale terminal generations are ignored.
    pub fn terminal_geometry(
        &mut self,
        id: TabId,
        terminal: TerminalId,
        geometry: Geometry,
    ) -> Result<bool, Error> {
        Geometry::new(geometry.cols, geometry.rows)?;
        let Some(tab) = self.tabs.get_mut(&id) else {
            return Ok(false);
        };
        if tab.terminal.live_id() != Some(terminal) {
            return Ok(false);
        }
        tab.geometry = geometry;
        Ok(true)
    }

    pub fn connect(&mut self, geometry: Geometry) -> Result<ClientId, Error> {
        Geometry::new(geometry.cols, geometry.rows)?;
        let id = ClientId(self.allocate()?);
        self.clients.insert(
            id,
            Client {
                selected: None,
                geometry,
            },
        );
        Ok(id)
    }

    pub fn disconnect(&mut self, client: ClientId) {
        self.clients.remove(&client);
    }

    pub fn selected(&self, client: ClientId) -> Result<Option<TabId>, Error> {
        Ok(self
            .clients
            .get(&client)
            .ok_or(Error::MissingClient)?
            .selected)
    }

    pub fn select(&mut self, client: ClientId, id: TabId) -> Result<(), Error> {
        if !self.tabs.contains_key(&id) {
            return Err(Error::MissingTab);
        }
        self.clients
            .get_mut(&client)
            .ok_or(Error::MissingClient)?
            .selected = Some(id);
        self.interact(client)
    }

    /// Focus, selection, and input promote this client's terminal dimensions.
    /// The runtime adapter must apply the resulting geometry to the PTY.
    pub fn interact(&mut self, client: ClientId) -> Result<(), Error> {
        let client = self.clients.get(&client).ok_or(Error::MissingClient)?;
        if let Some(id) = client.selected {
            self.tabs.get_mut(&id).ok_or(Error::MissingTab)?.geometry = client.geometry;
        }
        Ok(())
    }

    pub fn resize_client(&mut self, client: ClientId, geometry: Geometry) -> Result<(), Error> {
        Geometry::new(geometry.cols, geometry.rows)?;
        self.clients
            .get_mut(&client)
            .ok_or(Error::MissingClient)?
            .geometry = geometry;
        self.interact(client)
    }

    /// Invoke only after the destructive action has been confirmed by the UI.
    /// Returned IDs tell the runtime which processes to stop.
    pub fn close_tab(&mut self, id: TabId) -> Result<Option<TerminalId>, Error> {
        let tab = self.tabs.remove(&id).ok_or(Error::MissingTab)?;
        for client in self.clients.values_mut() {
            if client.selected == Some(id) {
                client.selected = None;
            }
        }
        Ok(tab.terminal.live_id())
    }

    pub fn close_space(&mut self, id: SpaceId) -> Result<Vec<TerminalId>, Error> {
        if self.spaces.remove(&id).is_none() {
            return Err(Error::MissingSpace);
        }
        let tabs: Vec<_> = self
            .tabs
            .values()
            .filter(|tab| tab.space == id)
            .map(|tab| tab.id)
            .collect();
        let mut stop = Vec::new();
        for tab in tabs {
            if let Some(terminal) = self.close_tab(tab)? {
                stop.push(terminal);
            }
        }
        Ok(stop)
    }

    pub fn snapshot(&self) -> Organization {
        Organization {
            spaces: self.spaces.values().cloned().collect(),
            tabs: self
                .tabs
                .values()
                .map(|tab| SavedTab {
                    id: tab.id,
                    space: tab.space,
                    label: tab.label.clone(),
                    directory: tab.directory.clone(),
                })
                .collect(),
        }
    }

    pub fn restore(saved: Organization, geometry: Geometry) -> Result<Self, Error> {
        Geometry::new(geometry.cols, geometry.rows)?;
        let mut server = Self::default();
        let mut ids = std::collections::BTreeSet::new();
        for space in saved.spaces {
            if space.id.0 == 0 || !ids.insert(space.id.0) {
                return Err(Error::InvalidSnapshot);
            }
            server.next_id = server.next_id.max(space.id.0);
            server.spaces.insert(space.id, space);
        }
        for tab in saved.tabs {
            if tab.id.0 == 0 || !ids.insert(tab.id.0) || !server.spaces.contains_key(&tab.space) {
                return Err(Error::InvalidSnapshot);
            }
            server.next_id = server.next_id.max(tab.id.0);
            server.tabs.insert(
                tab.id,
                Tab {
                    id: tab.id,
                    space: tab.space,
                    label: tab.label,
                    directory: tab.directory,
                    terminal: TerminalState::Ended(EndReason::ServerRestart),
                    geometry,
                },
            );
        }
        Ok(server)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size() -> Geometry {
        Geometry::new(80, 24).unwrap()
    }
    fn setup() -> (Server, SpaceId, Launch) {
        let mut server = Server::default();
        let space = server
            .create_space("quosh".into(), "/home/jt/quosh".into())
            .unwrap();
        let launch = server.create_tab(space, "shell".into(), size()).unwrap();
        assert!(server.launched(launch.tab, launch.terminal));
        (server, space, launch)
    }

    #[test]
    fn disconnect_keeps_work_alive() {
        let (mut server, _, launch) = setup();
        let client = server.connect(size()).unwrap();
        server.select(client, launch.tab).unwrap();
        server.disconnect(client);
        assert_eq!(
            server.tab(launch.tab).unwrap().terminal,
            TerminalState::Running(launch.terminal)
        );
        let next = server.connect(size()).unwrap();
        server.select(next, launch.tab).unwrap();
        assert_eq!(server.selected(next), Ok(Some(launch.tab)));
    }

    #[test]
    fn clients_navigate_independently_and_share_geometry() {
        let (mut server, space, first) = setup();
        let second = server.create_tab(space, "agent".into(), size()).unwrap();
        let laptop = server.connect(size()).unwrap();
        let phone_size = Geometry::new(40, 20).unwrap();
        let phone = server.connect(phone_size).unwrap();
        server.select(laptop, first.tab).unwrap();
        server.select(phone, second.tab).unwrap();
        assert_eq!(server.selected(laptop), Ok(Some(first.tab)));
        assert_eq!(server.tab(first.tab).unwrap().geometry, size());
        server.select(phone, first.tab).unwrap();
        assert_eq!(server.tab(first.tab).unwrap().geometry, phone_size);
        server.interact(laptop).unwrap();
        assert_eq!(server.tab(first.tab).unwrap().geometry, size());
        assert_eq!(server.selected(phone), Ok(Some(first.tab)));
    }

    #[test]
    fn close_tab_emits_stop_and_clears_all_views() {
        let (mut server, _, launch) = setup();
        let a = server.connect(size()).unwrap();
        let b = server.connect(size()).unwrap();
        server.select(a, launch.tab).unwrap();
        server.select(b, launch.tab).unwrap();
        assert_eq!(server.close_tab(launch.tab), Ok(Some(launch.terminal)));
        assert_eq!(server.selected(a), Ok(None));
        assert_eq!(server.selected(b), Ok(None));
        assert!(!server.launched(launch.tab, launch.terminal));
    }

    #[test]
    fn close_space_stops_only_its_own_work() {
        let (mut server, space, first) = setup();
        let second = server.create_tab(space, "agent".into(), size()).unwrap();
        let other_space = server.create_space("other".into(), "/tmp".into()).unwrap();
        let other = server
            .create_tab(other_space, "shell".into(), size())
            .unwrap();
        assert_eq!(
            server.close_space(space),
            Ok(vec![first.terminal, second.terminal])
        );
        assert!(server.tab(other.tab).is_some());
    }

    #[test]
    fn restart_restores_organization_without_starting_processes() {
        let (server, _, launch) = setup();
        let snapshot = server.snapshot();
        let mut restored = Server::restore(snapshot.clone(), size()).unwrap();
        assert_eq!(restored.snapshot(), snapshot);
        assert_eq!(
            restored.tab(launch.tab).unwrap().terminal,
            TerminalState::Ended(EndReason::ServerRestart)
        );
        let new = restored.reopen_tab(launch.tab).unwrap();
        assert_eq!(new.directory, launch.directory);
        assert_eq!(restored.reopen_tab(launch.tab), Err(Error::AlreadyLive));
        assert!(restored.launched(new.tab, new.terminal));
    }

    #[test]
    fn stale_exit_cannot_end_replacement_terminal() {
        let (mut server, _, old) = setup();
        assert!(server.ended(old.tab, old.terminal, EndReason::Exited(0)));
        let new = server.reopen_tab(old.tab).unwrap();
        assert!(!server.ended(old.tab, old.terminal, EndReason::Exited(1)));
        assert!(!server.launched(old.tab, old.terminal));
        assert!(server.launched(new.tab, new.terminal));
    }

    #[test]
    fn new_tabs_use_space_directory() {
        let (_, _, launch) = setup();
        assert_eq!(launch.directory, PathBuf::from("/home/jt/quosh"));
    }

    #[test]
    fn corrupt_snapshots_are_rejected() {
        let (server, _, _) = setup();
        let mut saved = server.snapshot();
        saved.tabs[0].space = SpaceId(999);
        assert!(matches!(
            Server::restore(saved, size()),
            Err(Error::InvalidSnapshot)
        ));
        let mut saved = server.snapshot();
        saved.tabs.push(saved.tabs[0].clone());
        assert!(matches!(
            Server::restore(saved, size()),
            Err(Error::InvalidSnapshot)
        ));
    }

    #[test]
    fn owner_geometry_does_not_replace_client_viewport_or_accept_stale_generation() {
        let (mut server, _, launch) = setup();
        let client = server.connect(size()).unwrap();
        server.select(client, launch.tab).unwrap();
        let small = Geometry::new(40, 12).unwrap();
        assert_eq!(
            server.terminal_geometry(launch.tab, launch.terminal, small),
            Ok(true)
        );
        assert_eq!(server.tab(launch.tab).unwrap().geometry, small);
        server.interact(client).unwrap();
        assert_eq!(server.tab(launch.tab).unwrap().geometry, size());
        server.ended(launch.tab, launch.terminal, EndReason::Exited(0));
        server.reopen_tab(launch.tab).unwrap();
        assert_eq!(
            server.terminal_geometry(launch.tab, launch.terminal, small),
            Ok(false)
        );
    }

    #[test]
    fn invalid_geometry_does_not_change_client_or_terminal() {
        let (mut server, _, launch) = setup();
        let client = server.connect(size()).unwrap();
        server.select(client, launch.tab).unwrap();
        assert_eq!(
            server.resize_client(client, Geometry { cols: 0, rows: 24 }),
            Err(Error::InvalidGeometry)
        );
        server.interact(client).unwrap();
        assert_eq!(server.tab(launch.tab).unwrap().geometry, size());
    }
}
