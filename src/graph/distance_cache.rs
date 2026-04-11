use std::collections::VecDeque;

use super::model::GraphSnapshot;

#[derive(Debug, Clone)]
pub struct DistanceCache {
    pub snapshot_id: u64,
    pub reachable_from_anchor: Vec<bool>,
    pub reachable_to_anchor: Vec<bool>,
}

impl DistanceCache {
    pub fn recompute(snapshot: &GraphSnapshot) -> Self {
        let mut reachable_from_anchor = vec![false; snapshot.tokens.len()];
        let mut reachable_to_anchor = vec![false; snapshot.tokens.len()];

        let mut queue = VecDeque::new();
        for &anchor_idx in &snapshot.cycle_anchor_indices {
            reachable_from_anchor[anchor_idx] = true;
            queue.push_back(anchor_idx);
        }
        while let Some(node) = queue.pop_front() {
            for edge in &snapshot.adjacency[node] {
                if !reachable_from_anchor[edge.to] {
                    reachable_from_anchor[edge.to] = true;
                    queue.push_back(edge.to);
                }
            }
        }

        queue.clear();
        for &anchor_idx in &snapshot.cycle_anchor_indices {
            reachable_to_anchor[anchor_idx] = true;
            queue.push_back(anchor_idx);
        }
        while let Some(node) = queue.pop_front() {
            for edge_ref in &snapshot.reverse_adj[node] {
                if !reachable_to_anchor[edge_ref.from] {
                    reachable_to_anchor[edge_ref.from] = true;
                    queue.push_back(edge_ref.from);
                }
            }
        }

        Self {
            snapshot_id: snapshot.snapshot_id,
            reachable_from_anchor,
            reachable_to_anchor,
        }
    }
}
