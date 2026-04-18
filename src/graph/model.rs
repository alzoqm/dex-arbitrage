use std::collections::HashMap;

use alloy::primitives::Address;
use smallvec::SmallVec;

use crate::{
    amm,
    types::{
        AmmKind, BlockRef, Edge, EdgeRef, LiquidityInfo, PoolSpecificState, PoolState, TokenInfo,
    },
};

#[derive(Debug, Clone)]
pub struct GraphSnapshot {
    pub snapshot_id: u64,
    pub block_ref: Option<BlockRef>,
    pub tokens: Vec<TokenInfo>,
    pub token_to_index: HashMap<Address, usize>,
    pub stable_token_indices: Vec<usize>,
    pub cycle_anchor_indices: Vec<usize>,
    pub adjacency: Vec<Vec<Edge>>,
    pub reverse_adj: Vec<Vec<EdgeRef>>,
    pub pools: HashMap<Address, PoolState>,
    pub pool_to_edges: HashMap<Address, SmallVec<[EdgeRef; 8]>>,
    pub pair_to_edges: HashMap<(usize, usize), SmallVec<[EdgeRef; 8]>>,
}

impl GraphSnapshot {
    pub fn build(
        snapshot_id: u64,
        block_ref: Option<BlockRef>,
        tokens: Vec<TokenInfo>,
        pools: HashMap<Address, PoolState>,
    ) -> Self {
        let token_to_index = tokens
            .iter()
            .enumerate()
            .map(|(idx, token)| (token.address, idx))
            .collect::<HashMap<_, _>>();
        let stable_token_indices = tokens
            .iter()
            .enumerate()
            .filter_map(|(idx, token)| token.is_stable.then_some(idx))
            .collect::<Vec<_>>();

        let cycle_anchor_indices = tokens
            .iter()
            .enumerate()
            .filter_map(|(idx, token)| token.is_cycle_anchor.then_some(idx))
            .collect::<Vec<_>>();

        let mut adjacency = vec![Vec::new(); tokens.len()];
        let mut reverse_adj = vec![Vec::new(); tokens.len()];
        let mut pool_to_edges: HashMap<Address, SmallVec<[EdgeRef; 8]>> = HashMap::new();
        let mut pair_to_edges: HashMap<(usize, usize), SmallVec<[EdgeRef; 8]>> = HashMap::new();

        for pool in pools.values() {
            let new_edges = build_edges_for_pool(pool, &token_to_index);
            for edge in new_edges {
                let from = edge.from;
                let to = edge.to;
                adjacency[from].push(edge);
                let edge_idx = adjacency[from].len() - 1;
                let edge_ref = EdgeRef { from, edge_idx };
                reverse_adj[to].push(edge_ref);
                pool_to_edges
                    .entry(pool.pool_id)
                    .or_default()
                    .push(edge_ref);
                pair_to_edges.entry((from, to)).or_default().push(edge_ref);
            }
        }

        Self {
            snapshot_id,
            block_ref,
            tokens,
            token_to_index,
            stable_token_indices,
            cycle_anchor_indices,
            adjacency,
            reverse_adj,
            pools,
            pool_to_edges,
            pair_to_edges,
        }
    }

    pub fn token_index(&self, address: Address) -> Option<usize> {
        self.token_to_index.get(&address).copied()
    }

    pub fn token_symbol(&self, address: Address) -> String {
        self.token_index(address)
            .and_then(|idx| self.tokens.get(idx))
            .map(|token| token.symbol.clone())
            .unwrap_or_else(|| address.to_string())
    }

    pub fn pair_edges(&self, from: Address, to: Address) -> Vec<EdgeRef> {
        match (self.token_index(from), self.token_index(to)) {
            (Some(fi), Some(ti)) => self
                .pair_to_edges
                .get(&(fi, ti))
                .map(|v| v.iter().copied().collect())
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    pub fn edge(&self, edge_ref: EdgeRef) -> Option<&Edge> {
        self.adjacency.get(edge_ref.from)?.get(edge_ref.edge_idx)
    }

    pub fn pool(&self, pool_id: Address) -> Option<&PoolState> {
        self.pools.get(&pool_id)
    }
}

fn build_edges_for_pool(pool: &PoolState, token_to_index: &HashMap<Address, usize>) -> Vec<Edge> {
    match &pool.state {
        PoolSpecificState::UniswapV2Like(state) => build_two_token_edges(
            pool,
            token_to_index,
            pool.kind,
            state.fee_ppm,
            |zero_for_one| amm::uniswap_v2::spot_rate(state, zero_for_one),
        ),
        PoolSpecificState::AerodromeV2Like(state) => build_two_token_edges(
            pool,
            token_to_index,
            pool.kind,
            state.fee_ppm,
            |zero_for_one| amm::aerodrome_v2::spot_rate(state, zero_for_one),
        ),
        PoolSpecificState::UniswapV3Like(state) => {
            build_two_token_edges(pool, token_to_index, pool.kind, state.fee, |zero_for_one| {
                amm::uniswap_v3::spot_rate(state, zero_for_one)
            })
        }
        PoolSpecificState::CurvePlain(state) => {
            build_n_token_edges(pool, token_to_index, pool.kind, |i, j| {
                let (spot_rate_q128, weight_log_q32, liquidity) =
                    amm::curve::spot_rate(state, i, j);
                (spot_rate_q128, weight_log_q32, liquidity, state.fee)
            })
        }
        PoolSpecificState::BalancerWeighted(state) => {
            build_n_token_edges(pool, token_to_index, pool.kind, |i, j| {
                let (spot_rate_q128, weight_log_q32, liquidity) =
                    amm::balancer::spot_rate(state, i, j);
                (
                    spot_rate_q128,
                    weight_log_q32,
                    liquidity,
                    state.swap_fee_ppm,
                )
            })
        }
    }
}

fn build_two_token_edges<F>(
    pool: &PoolState,
    token_to_index: &HashMap<Address, usize>,
    kind: AmmKind,
    fee_ppm: u32,
    mut compute: F,
) -> Vec<Edge>
where
    F: FnMut(bool) -> (alloy::primitives::U256, i64, LiquidityInfo),
{
    if pool.token_addresses.len() != 2 {
        return Vec::new();
    }
    let a = match token_to_index.get(&pool.token_addresses[0]).copied() {
        Some(v) => v,
        None => return Vec::new(),
    };
    let b = match token_to_index.get(&pool.token_addresses[1]).copied() {
        Some(v) => v,
        None => return Vec::new(),
    };

    let (spot_ab, w_ab, l_ab) = compute(true);
    let (spot_ba, w_ba, l_ba) = compute(false);

    vec![
        Edge {
            from: a,
            to: b,
            pool_id: pool.pool_id,
            amm_kind: kind,
            fee_ppm,
            weight_log_q32: w_ab,
            spot_rate_q128: spot_ab,
            liquidity: l_ab,
            pool_health: pool.health.clone(),
            dex_name: pool.dex_name.clone(),
        },
        Edge {
            from: b,
            to: a,
            pool_id: pool.pool_id,
            amm_kind: kind,
            fee_ppm,
            weight_log_q32: w_ba,
            spot_rate_q128: spot_ba,
            liquidity: l_ba,
            pool_health: pool.health.clone(),
            dex_name: pool.dex_name.clone(),
        },
    ]
}

fn build_n_token_edges<F>(
    pool: &PoolState,
    token_to_index: &HashMap<Address, usize>,
    kind: AmmKind,
    mut compute: F,
) -> Vec<Edge>
where
    F: FnMut(usize, usize) -> (alloy::primitives::U256, i64, LiquidityInfo, u32),
{
    let mut edges = Vec::new();
    for i in 0..pool.token_addresses.len() {
        for j in 0..pool.token_addresses.len() {
            if i == j {
                continue;
            }
            let from = match token_to_index.get(&pool.token_addresses[i]).copied() {
                Some(v) => v,
                None => continue,
            };
            let to = match token_to_index.get(&pool.token_addresses[j]).copied() {
                Some(v) => v,
                None => continue,
            };
            let (spot_rate_q128, weight_log_q32, liquidity, fee_ppm) = compute(i, j);
            edges.push(Edge {
                from,
                to,
                pool_id: pool.pool_id,
                amm_kind: kind,
                fee_ppm,
                weight_log_q32,
                spot_rate_q128,
                liquidity,
                pool_health: pool.health.clone(),
                dex_name: pool.dex_name.clone(),
            });
        }
    }
    edges
}
