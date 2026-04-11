use std::{cmp::Ordering, sync::Arc};

use anyhow::Result;

use crate::{
    config::Settings,
    graph::GraphSnapshot,
    types::{AdapterType, SplitExtra, SplitPlan},
};

use super::{exact_quoter::ExactQuoter, split_decision::should_split};

#[derive(Debug, Clone)]
pub struct SplitOptimizer {
    settings: Arc<Settings>,
    exact_quoter: ExactQuoter,
}

impl SplitOptimizer {
    pub fn new(settings: Arc<Settings>, exact_quoter: ExactQuoter) -> Self {
        Self {
            settings,
            exact_quoter,
        }
    }

    pub async fn optimize_pair(
        &self,
        snapshot: &GraphSnapshot,
        token_in: alloy::primitives::Address,
        token_out: alloy::primitives::Address,
        total_in: u128,
    ) -> Result<(Vec<SplitPlan>, u128)> {
        let edge_refs = snapshot.pair_edges(token_in, token_out);
        if edge_refs.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let min_slice = (total_in / 8).max(1);
        if !should_split(edge_refs.len(), total_in, min_slice) {
            return self.single_best(snapshot, edge_refs, token_in, token_out, total_in).await;
        }

        let mut allocations = vec![0u128; edge_refs.len()];
        let mut quoted_outputs = vec![0u128; edge_refs.len()];
        let mut assigned = 0u128;

        while assigned < total_in {
            let slice = (total_in - assigned).min(min_slice);
            let mut ranked = Vec::new();
            for (idx, edge_ref) in edge_refs.iter().enumerate() {
                let edge = match snapshot.edge(*edge_ref) {
                    Some(edge) => edge,
                    None => continue,
                };
                if edge.liquidity.safe_capacity_in > 0
                    && allocations[idx].saturating_add(slice) > edge.liquidity.safe_capacity_in
                {
                    continue;
                }
                let Some(pool) = snapshot.pool(edge.pool_id) else { continue };
                let total_quote = self
                    .exact_quoter
                    .quote_pool(snapshot, pool, token_in, token_out, allocations[idx].saturating_add(slice))
                    .await?;
                let marginal = total_quote.saturating_sub(quoted_outputs[idx]);
                ranked.push((idx, marginal));
            }
            ranked.sort_by(|a, b| b.1.cmp(&a.1));
            let Some((winner, marginal)) = ranked.first().copied() else { break };
            if marginal == 0 {
                break;
            }
            allocations[winner] = allocations[winner].saturating_add(slice);
            quoted_outputs[winner] = self
                .exact_quoter
                .quote_pool(
                    snapshot,
                    snapshot.pool(snapshot.edge(edge_refs[winner]).unwrap().pool_id).unwrap(),
                    token_in,
                    token_out,
                    allocations[winner],
                )
                .await?;
            assigned = assigned.saturating_add(slice);
        }

        self.materialize_allocations(snapshot, &edge_refs, token_in, token_out, allocations)
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
        let mut best = None::<(crate::types::EdgeRef, u128)>;
        for edge_ref in edge_refs {
            let edge = match snapshot.edge(edge_ref) {
                Some(edge) => edge,
                None => continue,
            };
            let Some(pool) = snapshot.pool(edge.pool_id) else { continue };
            let quote = self
                .exact_quoter
                .quote_pool(snapshot, pool, token_in, token_out, total_in)
                .await?;
            if best.as_ref().map(|(_, out)| quote > *out).unwrap_or(true) {
                best = Some((edge_ref, quote));
            }
        }

        if let Some((edge_ref, _out)) = best {
            self.materialize_allocations(snapshot, &[edge_ref], token_in, token_out, vec![total_in])
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
        let mut plans = Vec::new();
        let mut total_out = 0u128;

        for (idx, allocation) in allocations.into_iter().enumerate() {
            if allocation == 0 {
                continue;
            }
            let Some(edge) = snapshot.edge(edge_refs[idx]) else { continue };
            let Some(pool) = snapshot.pool(edge.pool_id) else { continue };
            let expected = self
                .exact_quoter
                .quote_pool(snapshot, pool, token_in, token_out, allocation)
                .await?;
            if expected == 0 {
                continue;
            }
            let min_amount_out = ((expected as f64) * 0.995) as u128;
            let extra = match &pool.state {
                crate::types::PoolSpecificState::UniswapV2Like(_) => SplitExtra::None,
                crate::types::PoolSpecificState::UniswapV3Like(_) => SplitExtra::V3 {
                    zero_for_one: pool.token_addresses.first().copied() == Some(token_in),
                    sqrt_price_limit_x96: alloy::primitives::U256::ZERO,
                },
                crate::types::PoolSpecificState::CurvePlain(state) => SplitExtra::Curve {
                    i: pool.token_addresses.iter().position(|token| *token == token_in).unwrap_or(0) as i128,
                    j: pool.token_addresses.iter().position(|token| *token == token_out).unwrap_or(1) as i128,
                    underlying: state.supports_underlying,
                },
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
}
