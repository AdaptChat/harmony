use std::sync::LazyLock;

use ahash::{HashMap, HashMapExt};
use tokio::{task::AbortHandle, sync::watch::Receiver};
use uuid::Uuid;

pub static TASK_MANAGER: LazyLock<TaskManager> = LazyLock::new(TaskManager::new);
pub struct TaskManager {
    map: HashMap<Uuid, Vec<AbortHandle>>
}

impl TaskManager {
    fn new() -> Self {
        Self { map: HashMap::new() }
    }

    pub fn init_listener(receiver: Receiver<bool>) {
        tokio::spawn(async move {
            receiver.changed().await; // Don't care about Result because the program will end if sender is dropped.

            TASK_MANAGER.shutdown_all();
        });
    }

    pub fn insert(&mut self, uuid: Uuid, abort_handle: AbortHandle) {
        self.map.entry(uuid)
            .and_modify(|handles| handles.push(abort_handle))
            .or_insert_with(|| vec![abort_handle]);
    }

    pub fn shutdown(&mut self, uuid: &Uuid) {
        if let Some(handles) = self.map.remove(uuid) {
            for handle in handles {
                handle.abort()
            }
        }
    }

    pub fn shutdown_all(&self) {
        for handles in self.map.values() {
            for handle in handles {
                handle.abort();
            }
        }
    }
}
