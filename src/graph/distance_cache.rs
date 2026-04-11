use std::collections::VecDeque;

use super::model::GraphSnapshot;

#[derive(Debug, Clone)]
pub struct DistanceCache {
    pub snapshot_id: u64,
    pub reachable_from_stable: Vec<bool>,
    pub reachable_to_stable: Vec<bool>,
}

impl DistanceCache {
    pub fn recompute(snapshot: &GraphSnapshot) -> Self {
        let mut reachable_from_stable = vec![false; snapshot.tokens.len()];
        let mut reachable_to_stable = vec![false; snapshot.tokens.len()];

        let mut queue = VecDeque::new();
        for &stable_idx in &snapshot.stable_token_indices {
            reachable_from_stable[stable_idx] = true;
            queue.push_back(stable_idx);
        }
        while let Some(node) = queue.pop_front() {
            for edge in &snapshot.adjacency[node] {
                if !reachable_from_stable[edge.to] {
                    reachable_from_stable[edge.to] = true;
                    queue.push_back(edge.to);
                }
            }
        }

        queue.clear();
        for &stable_idx in &snapshot.stable_token_indices {
            reachable_to_stable[stable_idx] = true;
            queue.push_back(stable_idx);
        }
        while let Some(node) = queue.pop_front() {
            for edge_ref in &snapshot.reverse_adj[node] {
                if !reachable_to_stable[edge_ref.from] {
                    reachable_to_stable[edge_ref.from] = true;
                    queue.push_back(edge_ref.from);
                }
            }
        }

        Self {
            snapshot_id: snapshot.snapshot_id,
            reachable_from_stable,
            reachable_to_stable,
        }
    }
}
