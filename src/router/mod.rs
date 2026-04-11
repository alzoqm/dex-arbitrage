pub mod exact_quoter;
pub mod quantity_search;
pub mod split_decision;
pub mod split_optimizer;

use std::sync::Arc;

use anyhow::Result;

use crate::{
    config::Settings,
    graph::GraphSnapshot,
    types::{CandidatePath, CapitalSource, ExactPlan, HopPlan},
};

use self::{
    exact_quoter::ExactQuoter,
    quantity_search::QuantitySearcher,
    split_optimizer::SplitOptimizer,
};

#[derive(Debug)]
pub struct Router {
    exact_quoter: ExactQuoter,
    split_optimizer: SplitOptimizer,
    quantity_searcher: QuantitySearcher,
}

impl Router {
    pub fn new(settings: Arc<Settings>, rpc: Arc<crate::rpc::RpcClients>) -> Self {
        let exact_quoter = ExactQuoter::new(settings.clone(), rpc.clone());
        let split_optimizer = SplitOptimizer::new(settings.clone(), exact_quoter.clone());
        let quantity_searcher = QuantitySearcher::new(settings);
        Self {
            exact_quoter,
            split_optimizer,
            quantity_searcher,
        }
    }

    pub async fn search_best_plan(
        &self,
        snapshot: &GraphSnapshot,
        candidate: &CandidatePath,
    ) -> Result<Option<ExactPlan>> {
        let mut best = None::<ExactPlan>;
        for amount in self.quantity_searcher.ladder(snapshot, candidate) {
            if let Some(plan) = self.quote_candidate(snapshot, candidate, amount).await? {
                if best
                    .as_ref()
                    .map(|best_plan| plan.expected_profit > best_plan.expected_profit)
                    .unwrap_or(true)
                {
                    best = Some(plan);
                }
            }
        }

        if let Some(center) = best.as_ref().map(|plan| plan.input_amount) {
            for amount in self.quantity_searcher.refinement_points(center) {
                if let Some(plan) = self.quote_candidate(snapshot, candidate, amount).await? {
                    if best
                        .as_ref()
                        .map(|best_plan| plan.expected_profit > best_plan.expected_profit)
                        .unwrap_or(true)
                    {
                        best = Some(plan);
                    }
                }
            }
        }

        Ok(best)
    }

    async fn quote_candidate(
        &self,
        snapshot: &GraphSnapshot,
        candidate: &CandidatePath,
        input_amount: u128,
    ) -> Result<Option<ExactPlan>> {
        let mut next_amount = input_amount;
        let mut hops = Vec::<HopPlan>::new();

        for hop in &candidate.path {
            let (splits, out) = self
                .split_optimizer
                .optimize_pair(snapshot, hop.from, hop.to, next_amount)
                .await?;
            if out == 0 || splits.is_empty() {
                return Ok(None);
            }
            hops.push(HopPlan {
                token_in: hop.from,
                token_out: hop.to,
                total_in: next_amount,
                total_out: out,
                splits,
            });
            next_amount = out;
        }

        let rough_gas_limit = estimate_gas_limit(candidate, &hops);
        let expected_profit = next_amount as i128 - input_amount as i128;
        if expected_profit <= 0 {
            return Ok(None);
        }

        Ok(Some(ExactPlan {
            snapshot_id: candidate.snapshot_id,
            input_token: candidate.start_token,
            output_token: candidate.start_token,
            input_amount,
            output_amount: next_amount,
            expected_profit,
            gas_limit: rough_gas_limit,
            gas_cost_wei: alloy::primitives::U256::ZERO,
            capital_source: CapitalSource::SelfFunded,
            hops,
        }))
    }
}

fn estimate_gas_limit(candidate: &CandidatePath, hops: &[HopPlan]) -> u64 {
    let mut gas = 80_000u64;
    for (idx, hop) in candidate.path.iter().enumerate() {
        let split_count = hops.get(idx).map(|h| h.splits.len()).unwrap_or(1) as u64;
        let base = match hop.amm_kind {
            crate::types::AmmKind::UniswapV2Like => 70_000,
            crate::types::AmmKind::UniswapV3Like => 140_000,
            crate::types::AmmKind::CurvePlain => 220_000,
            crate::types::AmmKind::BalancerWeighted => 130_000,
        };
        gas = gas.saturating_add(base).saturating_add(split_count.saturating_sub(1) * 30_000);
    }
    gas
}
