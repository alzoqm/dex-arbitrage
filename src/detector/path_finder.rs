use std::{
    collections::{HashSet, VecDeque},
    sync::Arc,
};

use crate::{
    config::Settings,
    detector::pruning,
    graph::{DistanceCache, GraphSnapshot},
    types::{CandidateHop, CandidatePath, EdgeRef},
};

#[derive(Debug)]
pub struct PathFinder {
    max_hops: usize,
}

impl PathFinder {
    pub fn new(settings: Arc<Settings>) -> Self {
        Self {
            max_hops: settings.risk.max_hops,
        }
    }

    pub fn find_candidates(
        &self,
        snapshot: &GraphSnapshot,
        changed_edges: &[EdgeRef],
        distance_cache: &DistanceCache,
    ) -> Vec<CandidatePath> {
        let changed_set = changed_edges.iter().copied().collect::<HashSet<_>>();
        let mut candidates = Vec::new();
        let mut dedup = HashSet::<String>::new();

        for &anchor_idx in &snapshot.cycle_anchor_indices {
            let start_addr = snapshot.tokens[anchor_idx].address;
            let start_symbol = snapshot.tokens[anchor_idx].symbol.clone();
            let mut current_path = VecDeque::new();
            self.dfs(
                snapshot,
                distance_cache,
                &changed_set,
                &mut candidates,
                &mut dedup,
                &mut current_path,
                anchor_idx,
                anchor_idx,
                start_addr,
                &start_symbol,
                0,
                0,
                false,
            );
        }

        candidates
    }

    #[allow(clippy::too_many_arguments)]
    fn dfs(
        &self,
        snapshot: &GraphSnapshot,
        distance_cache: &DistanceCache,
        changed_set: &HashSet<EdgeRef>,
        candidates: &mut Vec<CandidatePath>,
        dedup: &mut HashSet<String>,
        current_path: &mut VecDeque<CandidateHop>,
        start_idx: usize,
        current_idx: usize,
        start_addr: alloy::primitives::Address,
        start_symbol: &str,
        depth: usize,
        score_q32: i64,
        touched_changed: bool,
    ) {
        if depth >= self.max_hops {
            return;
        }

        for edge_ref in pruning::ranked_outgoing(snapshot, current_idx) {
            let Some(edge) = snapshot.edge(edge_ref) else {
                continue;
            };
            if !distance_cache.reachable_to_anchor[edge.to] {
                continue;
            }
            let is_cycle = edge.to == start_idx && depth >= 1;

            let next_score = score_q32.saturating_add(edge.weight_log_q32);
            let next_touched_changed = touched_changed || changed_set.contains(&edge_ref);
            current_path.push_back(CandidateHop {
                from: snapshot.tokens[edge.from].address,
                to: snapshot.tokens[edge.to].address,
                pool_id: edge.pool_id,
                amm_kind: edge.amm_kind,
                dex_name: edge.dex_name.clone(),
            });

            if is_cycle && next_touched_changed {
                let key = current_path
                    .iter()
                    .map(|hop| format!("{}:{}:{}", hop.from, hop.to, hop.pool_id))
                    .collect::<Vec<_>>()
                    .join("|");
                if dedup.insert(key.clone()) {
                    candidates.push(CandidatePath {
                        snapshot_id: snapshot.snapshot_id,
                        start_token: start_addr,
                        start_symbol: start_symbol.to_string(),
                        screening_score_q32: next_score,
                        cycle_key: key,
                        path: current_path.iter().cloned().collect(),
                    });
                }
            }

            if !is_cycle {
                self.dfs(
                    snapshot,
                    distance_cache,
                    changed_set,
                    candidates,
                    dedup,
                    current_path,
                    start_idx,
                    edge.to,
                    start_addr,
                    start_symbol,
                    depth + 1,
                    next_score,
                    next_touched_changed,
                );
            }

            current_path.pop_back();
        }
    }
}
