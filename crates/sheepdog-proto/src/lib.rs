//! Native Control Plane v1. Terminal streams use Quosh framing separately.
use serde::{Deserialize, Serialize};
use sheepdog_core::{Geometry, Space, SpaceId, Tab, TabId};
use std::path::PathBuf;

pub const VERSION: u16 = 1;
pub const AUTH: u8 = 32;
pub const AUTH_OK: u8 = 33;
pub const REQUEST: u8 = 34;
pub const REPLY: u8 = 35;
pub const CONTROL_PATH: &str = "/sheepdog/control";
pub const TERMINAL_PATH: &str = "/sheepdog/terminal";
pub const MAX_REQUEST: usize = 16 * 1024;
pub const MAX_REPLY: usize = 2 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
pub struct Bootstrap {
    pub protocol: u16,
    pub port: u16,
    pub hashes: Vec<[u8; 32]>,
    pub token: [u8; 32],
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AuthOk {
    pub protocol: u16,
    pub hashes: Vec<[u8; 32]>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Snapshot,
    /// Explicit client quit revokes this capability, not the server's work.
    Disconnect,
    CreateSpace {
        name: String,
        directory: PathBuf,
    },
    CreateTab {
        space: SpaceId,
        label: String,
        geometry: Geometry,
    },
    ReopenTab {
        tab: TabId,
    },
    Attach {
        tab: TabId,
        geometry: Geometry,
    },
    CloseTab {
        tab: TabId,
        confirmed: bool,
    },
    CloseSpace {
        space: SpaceId,
        confirmed: bool,
    },
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Ticket {
    pub tab: TabId,
    pub session_id: [u8; 16],
    pub token: [u8; 32],
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum Response {
    Snapshot {
        spaces: Vec<Space>,
        tabs: Vec<Tab>,
        selected: Option<TabId>,
    },
    Space(Space),
    Tab(Tab),
    Attachment(Ticket),
    Closed,
    Error(String),
}
