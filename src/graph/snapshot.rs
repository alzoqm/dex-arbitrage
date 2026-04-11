use std::{collections::VecDeque, sync::Arc};

use parking_lot::RwLock;

use super::model::GraphSnapshot;

#[derive(Debug)]
pub struct GraphStore {
    current: RwLock<Arc<GraphSnapshot>>,
    ring: RwLock<VecDeque<Arc<GraphSnapshot>>>,
    max_snapshots: usize,
}

impl GraphStore {
    pub fn new(initial: GraphSnapshot, max_snapshots: usize) -> Self {
        let initial = Arc::new(initial);
        let mut ring = VecDeque::new();
        ring.push_back(initial.clone());
        Self {
            current: RwLock::new(initial),
            ring: RwLock::new(ring),
            max_snapshots,
        }
    }

    pub fn load(&self) -> Arc<GraphSnapshot> {
        self.current.read().clone()
    }

    pub fn publish(&self, snapshot: GraphSnapshot) -> Arc<GraphSnapshot> {
        let snapshot = Arc::new(snapshot);
        *self.current.write() = snapshot.clone();
        let mut ring = self.ring.write();
        ring.push_back(snapshot.clone());
        while ring.len() > self.max_snapshots {
            ring.pop_front();
        }
        snapshot
    }

    pub fn rollback_to_snapshot(&self, snapshot_id: u64) -> Option<Arc<GraphSnapshot>> {
        let snapshot = self
            .ring
            .read()
            .iter()
            .find(|snapshot| snapshot.snapshot_id == snapshot_id)
            .cloned();
        if let Some(snapshot) = snapshot.clone() {
            *self.current.write() = snapshot;
        }
        snapshot
    }
}
