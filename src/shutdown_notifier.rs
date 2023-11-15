use std::sync::LazyLock;

use ahash::{HashMap, HashMapExt};
use tokio::sync::watch::{Receiver, Sender};
use uuid::Uuid;

pub static SHUTDOWN_NOTIFIER: LazyLock<ShutdownNotifier> =
    LazyLock::new(|| ShutdownNotifier::new());
pub struct ShutdownNotifier {
    map: HashMap<Uuid, Sender<bool>>,
}

impl ShutdownNotifier {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    pub fn insert(&mut self, session_id: Uuid, sender: Sender<bool>) {
        self.map.insert(session_id, sender);
    }

    /// Returns true if successfully notified.
    /// Returns false if session id doesn't exist, or an error occured.
    pub fn shutdown(&self, session_id: &Uuid) -> bool {
        if let Some(sender) = self.map.get(session_id) {
            !sender.send(true).is_err()
        } else {
            false
        }
    }

    pub fn get_receiver(&self, session_id: &Uuid) -> Option<Receiver<bool>> {
        if let Some(sender) = self.map.get(session_id) {
            Some(sender.subscribe())
        } else {
            None
        }
    }
}
