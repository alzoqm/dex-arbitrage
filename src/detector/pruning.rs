use crate::{graph::GraphSnapshot, types::EdgeRef};

pub fn ranked_outgoing(snapshot: &GraphSnapshot, vertex: usize) -> Vec<EdgeRef> {
    let mut refs = snapshot
        .adjacency
        .get(vertex)
        .map(|edges| {
            edges
                .iter()
                .enumerate()
                .map(|(edge_idx, edge)| (edge_idx, edge))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    refs.sort_by(|(_, a), (_, b)| {
        let health_cmp = b
            .pool_health
            .confidence_bps
            .cmp(&a.pool_health.confidence_bps);
        if health_cmp != std::cmp::Ordering::Equal {
            return health_cmp;
        }
        let liq_cmp = b
            .liquidity
            .safe_capacity_in
            .cmp(&a.liquidity.safe_capacity_in);
        if liq_cmp != std::cmp::Ordering::Equal {
            return liq_cmp;
        }
        a.weight_log_q32.cmp(&b.weight_log_q32)
    });

    refs.into_iter()
        .map(|(edge_idx, _)| EdgeRef {
            from: vertex,
            edge_idx,
        })
        .collect()
}
