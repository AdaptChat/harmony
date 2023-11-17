use std::sync::LazyLock;

use ahash::{HashMap, HashMapExt};
use tokio::task::AbortHandle;
use uuid::Uuid;

pub static TASK_MANAGER: LazyLock<TaskManager> = LazyLock::new(TaskManager::new);
pub struct TaskManager {
    map: HashMap<Uuid, Vec<AbortHandle>>,
}

impl TaskManager {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    pub fn insert(&mut self, uuid: Uuid, abort_handle: AbortHandle) {
        self.map
            .entry(uuid)
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
