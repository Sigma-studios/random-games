//! What the bot, the daily job and the site share.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::config::Settings;
use crate::sheet::Store;
use crate::state::StateFile;
use crate::steam::Steam;

pub struct App {
    pub settings: Arc<Settings>,
    pub store: Store,
    pub state: StateFile,
    pub steam: Steam,
    /// Person → their Discord avatar URL (never sent to the site as is: it
    /// contains their Discord id).
    pub avatars: RwLock<HashMap<String, String>>,
}
