use std::collections::HashMap;

use alloy::primitives::Address;

use crate::{
    graph::model::GraphSnapshot,
    types::{BlockRef, EdgeRef, PoolState, TokenInfo},
};

#[derive(Debug, Clone)]
pub struct GraphUpdater {
    pub max_ring_snapshots: usize,
}

impl GraphUpdater {
    pub fn new(max_ring_snapshots: usize) -> Self {
        Self { max_ring_snapshots }
    }

    pub fn rebuild(
        &self,
        next_snapshot_id: u64,
        block_ref: Option<BlockRef>,
        tokens: Vec<TokenInfo>,
        pools: HashMap<Address, PoolState>,
        changed_pool_ids: &[Address],
    ) -> (GraphSnapshot, Vec<EdgeRef>) {
        let snapshot = GraphSnapshot::build(next_snapshot_id, block_ref, tokens, pools);
        let changed_edges = changed_pool_ids
            .iter()
            .flat_map(|pool_id| {
                snapshot
                    .pool_to_edges
                    .get(pool_id)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
            })
            .collect::<Vec<_>>();
        (snapshot, changed_edges)
    }
}
