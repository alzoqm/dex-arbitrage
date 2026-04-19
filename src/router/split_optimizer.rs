use std::{cmp::Ordering, sync::Arc};

use anyhow::Result;
use futures::{stream, StreamExt};

use crate::{
    graph::GraphSnapshot,
    types::{AdapterType, SplitExtra, SplitPlan},
};

use super::{
    exact_quoter::ExactQuoter, split_decision::should_split, v3_limits::sqrt_price_limit_x96,
};

#[derive(Debug, Clone)]
pub struct SplitOptimizer {
    exact_quoter: ExactQuoter,
    max_parallel_pools: usize,
    pair_edge_scan_limit: usize,
    pair_quote_concurrency: usize,
    split_min_output_bps: u128,
}

impl SplitOptimizer {
    pub fn new(settings: Arc<crate::config::Settings>, exact_quoter: ExactQuoter) -> Self {
        Self::with_exact_quoter(settings, exact_quoter)
    }

    pub fn with_exact_quoter(
        settings: Arc<crate::config::Settings>,
        exact_quoter: ExactQuoter,
    ) -> Self {
        let max_parallel_pools = settings.search.max_split_parallel_pools.max(1);
        Self {
            exact_quoter,
            max_parallel_pools,
            pair_edge_scan_limit: env_usize(
                "ROUTE_PAIR_EDGE_SCAN_LIMIT",
                max_parallel_pools
                    .saturating_mul(4)
                    .clamp(max_parallel_pools, 32),
            )
            .max(max_parallel_pools),
            pair_quote_concurrency: env_usize("ROUTE_PAIR_QUOTE_CONCURRENCY", 1).clamp(1, 32),
            split_min_output_bps: env_usize("SPLIT_MIN_OUTPUT_BPS", 9_950).clamp(1, 10_000) as u128,
        }
    }

    pub async fn optimize_pair(
        &self,
        snapshot: &GraphSnapshot,
        token_in: alloy::primitives::Address,
        token_out: alloy::primitives::Address,
        total_in: u128,
    ) -> Result<(Vec<SplitPlan>, u128)> {
        let edge_refs = ranked_pair_edges(
            snapshot,
            snapshot.pair_edges(token_in, token_out),
            self.pair_edge_scan_limit,
            total_in,
        );
        if edge_refs.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let min_slice = (total_in / 8).max(1);
        if !should_split(edge_refs.len(), total_in, min_slice) {
            return self
                .single_best(snapshot, edge_refs, token_in, token_out, total_in)
                .await;
        }

        let v2_edge_refs = edge_refs
            .iter()
            .copied()
            .take(self.max_parallel_pools)
            .collect::<Vec<_>>();
        if let Some(allocations) = super::v2_split::optimal_v2_allocations(
            snapshot,
            &v2_edge_refs,
            token_in,
            token_out,
            total_in,
        ) {
            let materialized = self
                .materialize_allocations(snapshot, &v2_edge_refs, token_in, token_out, allocations)
                .await?;
            if !materialized.0.is_empty() {
                return Ok(materialized);
            }
        }

        let mut allocations = vec![0u128; edge_refs.len()];
        let mut quoted_outputs = vec![0u128; edge_refs.len()];
        let mut assigned = 0u128;
        let mut active_allocations = 0usize;

        while assigned < total_in {
            let slice = (total_in - assigned).min(min_slice);
            let mut quote_jobs = Vec::new();
            for (idx, edge_ref) in edge_refs.iter().copied().enumerate() {
                if allocations[idx] == 0 && active_allocations >= self.max_parallel_pools {
                    continue;
                }
                let edge = match snapshot.edge(edge_ref) {
                    Some(edge) => edge,
                    None => continue,
                };
                let quote_amount = allocations[idx].saturating_add(slice);
                if edge.liquidity.safe_capacity_in > 0
                    && quote_amount > edge.liquidity.safe_capacity_in
                {
                    continue;
                }
                quote_jobs.push((idx, edge.pool_id, quote_amount));
            }

            let exact_quoter = self.exact_quoter.clone();
            let mut quote_stream = stream::iter(quote_jobs)
                .map(|(idx, pool_id, quote_amount)| {
                    let exact_quoter = exact_quoter.clone();
                    async move {
                        let Some(pool) = snapshot.pool(pool_id) else {
                            return Ok((idx, 0u128));
                        };
                        let total_quote = exact_quoter
                            .quote_pool(snapshot, pool, token_in, token_out, quote_amount)
                            .await?;
                        Ok::<_, anyhow::Error>((idx, total_quote))
                    }
                })
                .buffer_unordered(self.pair_quote_concurrency);

            let mut ranked = Vec::new();
            while let Some(quoted) = quote_stream.next().await {
                let (idx, total_quote) = quoted?;
                let marginal = total_quote.saturating_sub(quoted_outputs[idx]);
                ranked.push((idx, marginal, total_quote));
            }
            ranked.sort_by(|a, b| b.1.cmp(&a.1));
            let Some((winner, marginal, total_quote)) = ranked.first().copied() else {
                break;
            };
            if marginal == 0 {
                break;
            }
            if allocations[winner] == 0 {
                active_allocations += 1;
            }
            allocations[winner] = allocations[winner].saturating_add(slice);
            quoted_outputs[winner] = total_quote;
            assigned = assigned.saturating_add(slice);
        }

        self.materialize_allocations_with_quotes(
            snapshot,
            &edge_refs,
            token_in,
            token_out,
            allocations,
            Some(quoted_outputs),
        )
        .await
    }

    async fn single_best(
        &self,
        snapshot: &GraphSnapshot,
        edge_refs: Vec<crate::types::EdgeRef>,
        token_in: alloy::primitives::Address,
        token_out: alloy::primitives::Address,
        total_in: u128,
    ) -> Result<(Vec<SplitPlan>, u128)> {
        let mut quote_jobs = Vec::new();
        for edge_ref in edge_refs {
            let edge = match snapshot.edge(edge_ref) {
                Some(edge) => edge,
                None => continue,
            };
            if edge.liquidity.safe_capacity_in > 0 && total_in > edge.liquidity.safe_capacity_in {
                continue;
            }
            quote_jobs.push((edge_ref, edge.pool_id));
        }

        let exact_quoter = self.exact_quoter.clone();
        let mut quote_stream = stream::iter(quote_jobs)
            .map(|(edge_ref, pool_id)| {
                let exact_quoter = exact_quoter.clone();
                async move {
                    let Some(pool) = snapshot.pool(pool_id) else {
                        return Ok((edge_ref, 0u128));
                    };
                    let quote = exact_quoter
                        .quote_pool(snapshot, pool, token_in, token_out, total_in)
                        .await?;
                    Ok::<_, anyhow::Error>((edge_ref, quote))
                }
            })
            .buffer_unordered(self.pair_quote_concurrency);

        let mut best = None::<(crate::types::EdgeRef, u128)>;
        while let Some(quoted) = quote_stream.next().await {
            let (edge_ref, quote) = quoted?;
            if quote > 0 && best.as_ref().map(|(_, out)| quote > *out).unwrap_or(true) {
                best = Some((edge_ref, quote));
            }
        }

        if let Some((edge_ref, out)) = best {
            self.materialize_allocations_with_quotes(
                snapshot,
                &[edge_ref],
                token_in,
                token_out,
                vec![total_in],
                Some(vec![out]),
            )
            .await
        } else {
            Ok((Vec::new(), 0))
        }
    }

    async fn materialize_allocations(
        &self,
        snapshot: &GraphSnapshot,
        edge_refs: &[crate::types::EdgeRef],
        token_in: alloy::primitives::Address,
        token_out: alloy::primitives::Address,
        allocations: Vec<u128>,
    ) -> Result<(Vec<SplitPlan>, u128)> {
        self.materialize_allocations_with_quotes(
            snapshot,
            edge_refs,
            token_in,
            token_out,
            allocations,
            None,
        )
        .await
    }

    async fn materialize_allocations_with_quotes(
        &self,
        snapshot: &GraphSnapshot,
        edge_refs: &[crate::types::EdgeRef],
        token_in: alloy::primitives::Address,
        token_out: alloy::primitives::Address,
        allocations: Vec<u128>,
        expected_outputs: Option<Vec<u128>>,
    ) -> Result<(Vec<SplitPlan>, u128)> {
        let mut plans = Vec::new();
        let mut total_out = 0u128;

        for (idx, allocation) in allocations.into_iter().enumerate() {
            if allocation == 0 {
                continue;
            }
            let Some(edge) = snapshot.edge(edge_refs[idx]) else {
                continue;
            };
            let Some(pool) = snapshot.pool(edge.pool_id) else {
                continue;
            };
            let expected = match expected_outputs
                .as_ref()
                .and_then(|outputs| outputs.get(idx))
                .copied()
            {
                Some(expected) if expected > 0 => expected,
                _ => {
                    self.exact_quoter
                        .quote_pool(snapshot, pool, token_in, token_out, allocation)
                        .await?
                }
            };
            if expected == 0 {
                continue;
            }
            let min_amount_out = expected.saturating_mul(self.split_min_output_bps) / 10_000;
            let extra = match &pool.state {
                crate::types::PoolSpecificState::UniswapV2Like(state) => SplitExtra::V2 {
                    fee_ppm: state.fee_ppm,
                },
                crate::types::PoolSpecificState::AerodromeV2Like(state) => {
                    SplitExtra::AerodromeV2 {
                        stable: state.stable,
                        fee_ppm: state.fee_ppm,
                    }
                }
                crate::types::PoolSpecificState::UniswapV3Like(state) => {
                    let zero_for_one = pool.token_addresses.first().copied() == Some(token_in);
                    SplitExtra::V3 {
                        zero_for_one,
                        sqrt_price_limit_x96: sqrt_price_limit_x96(
                            state.sqrt_price_x96,
                            zero_for_one,
                        ),
                    }
                }
                crate::types::PoolSpecificState::CurvePlain(state) => {
                    let Some(i) = pool
                        .token_addresses
                        .iter()
                        .position(|token| *token == token_in)
                    else {
                        continue;
                    };
                    let Some(j) = pool
                        .token_addresses
                        .iter()
                        .position(|token| *token == token_out)
                    else {
                        continue;
                    };
                    SplitExtra::Curve {
                        i: i as i128,
                        j: j as i128,
                        underlying: state.supports_underlying,
                    }
                }
                crate::types::PoolSpecificState::BalancerWeighted(state) => SplitExtra::Balancer {
                    pool_id: state.pool_id,
                },
            };
            plans.push(SplitPlan {
                dex_name: pool.dex_name.clone(),
                pool_id: pool.pool_id,
                adapter_type: AdapterType::from(pool.kind),
                token_in,
                token_out,
                amount_in: allocation,
                min_amount_out,
                expected_amount_out: expected,
                extra,
            });
            total_out = total_out.saturating_add(expected);
        }

        plans.sort_by(|a, b| {
            let lhs = a.expected_amount_out.cmp(&b.expected_amount_out);
            if lhs == Ordering::Equal {
                a.pool_id.cmp(&b.pool_id)
            } else {
                lhs.reverse()
            }
        });

        Ok((plans, total_out))
    }

    pub async fn quote_split(&self, snapshot: &GraphSnapshot, split: &SplitPlan) -> Result<u128> {
        let Some(pool) = snapshot.pool(split.pool_id) else {
            return Ok(0);
        };
        self.exact_quoter
            .quote_pool(
                snapshot,
                pool,
                split.token_in,
                split.token_out,
                split.amount_in,
            )
            .await
    }
}

fn ranked_pair_edges(
    snapshot: &GraphSnapshot,
    edge_refs: Vec<crate::types::EdgeRef>,
    max_edges: usize,
    target_input: u128,
) -> Vec<crate::types::EdgeRef> {
    let max_edges = max_edges.max(1);
    let mut by_price = edge_refs.clone();
    by_price.sort_by(|a, b| compare_edge_refs_for_price(snapshot, *a, *b));

    let mut selected = Vec::<crate::types::EdgeRef>::new();
    let price_slots = max_edges.div_ceil(2);
    push_unique(
        &mut selected,
        by_price.iter().copied().take(price_slots),
        max_edges,
    );

    let mut by_capacity = edge_refs;
    by_capacity.sort_by(|a, b| compare_edge_refs_for_capacity(snapshot, *a, *b, target_input));
    push_unique(&mut selected, by_capacity.into_iter(), max_edges);

    selected.sort_by(|a, b| compare_edge_refs_for_price(snapshot, *a, *b));
    selected
}

fn push_unique<I>(out: &mut Vec<crate::types::EdgeRef>, refs: I, max_edges: usize)
where
    I: IntoIterator<Item = crate::types::EdgeRef>,
{
    for edge_ref in refs {
        if out.len() >= max_edges {
            break;
        }
        if !out.contains(&edge_ref) {
            out.push(edge_ref);
        }
    }
}

fn compare_edge_refs_for_price(
    snapshot: &GraphSnapshot,
    a: crate::types::EdgeRef,
    b: crate::types::EdgeRef,
) -> Ordering {
    let a_edge = snapshot.edge(a);
    let b_edge = snapshot.edge(b);
    match (a_edge, b_edge) {
        (Some(a_edge), Some(b_edge)) => a_edge
            .weight_log_q32
            .cmp(&b_edge.weight_log_q32)
            .then_with(|| {
                b_edge
                    .liquidity
                    .safe_capacity_in
                    .cmp(&a_edge.liquidity.safe_capacity_in)
            })
            .then_with(|| a_edge.pool_id.cmp(&b_edge.pool_id)),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn compare_edge_refs_for_capacity(
    snapshot: &GraphSnapshot,
    a: crate::types::EdgeRef,
    b: crate::types::EdgeRef,
    target_input: u128,
) -> Ordering {
    let a_edge = snapshot.edge(a);
    let b_edge = snapshot.edge(b);
    match (a_edge, b_edge) {
        (Some(a_edge), Some(b_edge)) => {
            let a_fits = target_input > 0 && a_edge.liquidity.safe_capacity_in >= target_input;
            let b_fits = target_input > 0 && b_edge.liquidity.safe_capacity_in >= target_input;
            b_fits
                .cmp(&a_fits)
                .then_with(|| {
                    b_edge
                        .liquidity
                        .safe_capacity_in
                        .cmp(&a_edge.liquidity.safe_capacity_in)
                })
                .then_with(|| a_edge.weight_log_q32.cmp(&b_edge.weight_log_q32))
                .then_with(|| a_edge.pool_id.cmp(&b_edge.pool_id))
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}
