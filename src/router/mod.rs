pub mod exact_quoter;
pub mod quantity_search;
pub mod split_decision;
pub mod split_optimizer;

use std::sync::Arc;

use anyhow::Result;

use crate::{
    config::Settings,
    execution::flash_loan::FlashLoanEngine,
    graph::GraphSnapshot,
    types::{CandidatePath, CapitalSource, ExactPlan, HopPlan},
};

use self::{
    exact_quoter::ExactQuoter, quantity_search::QuantitySearcher, split_optimizer::SplitOptimizer,
};

#[derive(Debug)]
pub struct Router {
    split_optimizer: SplitOptimizer,
    quantity_searcher: QuantitySearcher,
    flash: FlashLoanEngine,
}

impl Router {
    pub fn new(settings: Arc<Settings>, rpc: Arc<crate::rpc::RpcClients>) -> Self {
        let exact_quoter = ExactQuoter::new(settings.clone(), rpc.clone());
        let split_optimizer = SplitOptimizer::new(settings.clone(), exact_quoter.clone());
        let quantity_searcher = QuantitySearcher::new(settings.clone());
        let flash = FlashLoanEngine::new(settings, rpc);
        Self {
            split_optimizer,
            quantity_searcher,
            flash,
        }
    }

    pub async fn search_best_plan(
        &self,
        snapshot: &GraphSnapshot,
        candidate: &CandidatePath,
    ) -> Result<Option<ExactPlan>> {
        let mut best = None::<ExactPlan>;
        let premium_ppm = self.flash.premium_ppm().await?;
        let Some((min_amount, max_amount)) =
            self.quantity_searcher.search_range(snapshot, candidate)
        else {
            return Ok(None);
        };
        let mut evaluated = std::collections::HashMap::<u128, Option<ExactPlan>>::new();
        let mut ordered = self.quantity_searcher.ladder(snapshot, candidate);
        if ordered.is_empty() {
            return Ok(None);
        }

        for &amount in &ordered {
            if let Some(plan) = self
                .evaluate_amount(snapshot, candidate, amount, premium_ppm, &mut evaluated)
                .await?
            {
                best = better_plan(best, plan);
            }
        }

        if best.is_none() {
            return Ok(None);
        }
        ordered.sort_unstable();

        let mut seeds = evaluated
            .iter()
            .filter_map(|(amount, plan)| plan.as_ref().map(|plan| (plan.expected_profit, *amount)))
            .collect::<Vec<_>>();
        seeds.sort_by(|a, b| b.0.cmp(&a.0));
        seeds.dedup_by_key(|(_, amount)| *amount);
        seeds.truncate(8);

        for (_, center) in seeds {
            let center_pos = ordered
                .binary_search(&center)
                .unwrap_or_else(|idx| idx.min(ordered.len().saturating_sub(1)));
            let mut low = if center_pos == 0 {
                min_amount
            } else {
                ordered[center_pos - 1]
            };
            let mut high = ordered
                .get(center_pos + 1)
                .copied()
                .unwrap_or(max_amount)
                .max(center);

            for amount in self.quantity_searcher.refinement_points(center, max_amount) {
                if let Some(plan) = self
                    .evaluate_amount(snapshot, candidate, amount, premium_ppm, &mut evaluated)
                    .await?
                {
                    best = better_plan(best, plan);
                }
            }

            for _ in 0..32 {
                if high.saturating_sub(low) <= 32 {
                    break;
                }
                let width = high - low;
                let mid_left = low + width / 3;
                let mid_right = high - width / 3;
                let left_profit = self
                    .evaluate_profit(snapshot, candidate, mid_left, premium_ppm, &mut evaluated)
                    .await?;
                let right_profit = self
                    .evaluate_profit(snapshot, candidate, mid_right, premium_ppm, &mut evaluated)
                    .await?;
                if left_profit < right_profit {
                    low = mid_left.saturating_add(1);
                } else {
                    high = mid_right.saturating_sub(1).max(low);
                }
            }

            for amount in dense_points(low, high, 64) {
                if let Some(plan) = self
                    .evaluate_amount(snapshot, candidate, amount, premium_ppm, &mut evaluated)
                    .await?
                {
                    best = better_plan(best, plan);
                }
            }
        }

        Ok(best)
    }

    async fn evaluate_profit(
        &self,
        snapshot: &GraphSnapshot,
        candidate: &CandidatePath,
        amount: u128,
        premium_ppm: u128,
        evaluated: &mut std::collections::HashMap<u128, Option<ExactPlan>>,
    ) -> Result<i128> {
        Ok(self
            .evaluate_amount(snapshot, candidate, amount, premium_ppm, evaluated)
            .await?
            .map(|plan| plan.expected_profit)
            .unwrap_or(i128::MIN))
    }

    async fn evaluate_amount(
        &self,
        snapshot: &GraphSnapshot,
        candidate: &CandidatePath,
        amount: u128,
        premium_ppm: u128,
        evaluated: &mut std::collections::HashMap<u128, Option<ExactPlan>>,
    ) -> Result<Option<ExactPlan>> {
        if let Some(plan) = evaluated.get(&amount) {
            return Ok(plan.clone());
        }
        let plan = self
            .quote_candidate(snapshot, candidate, amount, premium_ppm)
            .await?;
        evaluated.insert(amount, plan.clone());
        Ok(plan)
    }

    async fn quote_candidate(
        &self,
        snapshot: &GraphSnapshot,
        candidate: &CandidatePath,
        input_amount: u128,
        flash_premium_ppm: u128,
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
        let gross_profit_raw = next_amount as i128 - input_amount as i128;
        let flash_fee_raw = input_amount.saturating_mul(flash_premium_ppm) / 1_000_000;
        let net_profit_before_gas_raw = gross_profit_raw - flash_fee_raw as i128;
        if net_profit_before_gas_raw <= 0 {
            return Ok(None);
        }

        // Router only computes raw gross profit; USD conversion and gas costs
        // are handled later in the Validator
        Ok(Some(ExactPlan {
            snapshot_id: candidate.snapshot_id,
            input_token: candidate.start_token,
            output_token: candidate.start_token,
            input_amount,
            output_amount: next_amount,
            gross_profit_raw,
            flash_premium_ppm,
            flash_fee_raw,
            net_profit_before_gas_raw,
            contract_min_profit_raw: 0, // Set later in validator
            input_value_usd_e8: 0,      // Calculated later in validator
            flash_loan_value_usd_e8: 0,
            gross_profit_usd_e8: 0, // Calculated later in validator
            flash_fee_usd_e8: 0,
            actual_flash_fee_usd_e8: 0,
            gas_cost_usd_e8: 0,
            net_profit_usd_e8: 0,
            expected_profit: net_profit_before_gas_raw,
            gas_limit: rough_gas_limit,
            gas_cost_wei: alloy::primitives::U256::ZERO,
            capital_source: CapitalSource::SelfFunded,
            flash_loan_amount: 0,
            actual_flash_fee_raw: 0,
            hops,
        }))
    }
}

fn better_plan(current: Option<ExactPlan>, next: ExactPlan) -> Option<ExactPlan> {
    if current
        .as_ref()
        .map(|best_plan| next.expected_profit > best_plan.expected_profit)
        .unwrap_or(true)
    {
        Some(next)
    } else {
        current
    }
}

fn dense_points(low: u128, high: u128, target_count: u128) -> Vec<u128> {
    if low > high {
        return Vec::new();
    }
    let width = high - low;
    if width <= target_count {
        return (low..=high).collect();
    }
    let step = (width / target_count).max(1);
    let mut out = Vec::new();
    let mut current = low;
    while current <= high {
        out.push(current);
        let next = current.saturating_add(step);
        if next <= current {
            break;
        }
        current = next;
    }
    if out.last().copied() != Some(high) {
        out.push(high);
    }
    out
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
        gas = gas
            .saturating_add(base)
            .saturating_add(split_count.saturating_sub(1) * 30_000);
    }
    gas
}
