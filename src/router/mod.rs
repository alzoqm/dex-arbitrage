pub mod exact_quoter;
pub mod fast_sizer;
pub mod quantity_search;
pub mod split_decision;
pub mod split_optimizer;
pub mod v2_split;
pub mod v3_limits;

use std::sync::Arc;

use alloy::primitives::Address;
use anyhow::Result;

use crate::{
    config::Settings,
    execution::flash_loan::FlashLoanEngine,
    graph::GraphSnapshot,
    types::{AmmKind, CandidatePath, CapitalSource, ExactPlan, HopPlan, SplitPlan},
};

use self::{
    exact_quoter::ExactQuoter, fast_sizer::FastSizer, quantity_search::QuantitySearcher,
    split_optimizer::SplitOptimizer,
};

#[derive(Debug, Clone, Default)]
pub struct RouteSearchStats {
    pub amount_range_missing: bool,
    pub evaluated_amounts: usize,
    pub no_output_quotes: usize,
    pub no_output_hop0: usize,
    pub no_output_hop1: usize,
    pub no_output_hop2: usize,
    pub no_output_hop3_plus: usize,
    pub no_output_v2: usize,
    pub no_output_aerodrome_v2: usize,
    pub no_output_v3: usize,
    pub no_output_curve: usize,
    pub no_output_balancer: usize,
    pub gross_nonpositive: usize,
    pub flash_fee_nonpositive: usize,
    pub profitable_plans: usize,
    pub fast_sizer_points: usize,
    pub used_fast_sizer: bool,
    pub used_wide_fallback: bool,
    pub min_amount: u128,
    pub max_amount: u128,
    pub first_no_output_hop: Option<usize>,
    pub first_no_output_pool: Option<Address>,
    pub first_no_output_amount_in: u128,
}

impl RouteSearchStats {
    fn record_no_output(
        &mut self,
        hop_index: usize,
        amm_kind: AmmKind,
        pool_id: Option<Address>,
        amount_in: u128,
    ) {
        self.no_output_quotes += 1;
        if self.first_no_output_hop.is_none() {
            self.first_no_output_hop = Some(hop_index);
            self.first_no_output_pool = pool_id;
            self.first_no_output_amount_in = amount_in;
        }
        match hop_index {
            0 => self.no_output_hop0 += 1,
            1 => self.no_output_hop1 += 1,
            2 => self.no_output_hop2 += 1,
            _ => self.no_output_hop3_plus += 1,
        }
        match amm_kind {
            AmmKind::UniswapV2Like => self.no_output_v2 += 1,
            AmmKind::AerodromeV2Like => self.no_output_aerodrome_v2 += 1,
            AmmKind::UniswapV3Like => self.no_output_v3 += 1,
            AmmKind::CurvePlain => self.no_output_curve += 1,
            AmmKind::BalancerWeighted => self.no_output_balancer += 1,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RouteSearchResult {
    pub plan: Option<ExactPlan>,
    pub stats: RouteSearchStats,
}

#[derive(Debug)]
pub struct Router {
    split_optimizer: SplitOptimizer,
    verify_split_optimizer: Option<SplitOptimizer>,
    quantity_searcher: QuantitySearcher,
    fast_sizer: FastSizer,
    flash: FlashLoanEngine,
    verify_v3_refinement_points: usize,
    verify_v3_early_exit: bool,
    verify_v3_early_reoptimize: bool,
    verify_v3_final_reoptimize: bool,
    verify_v3_early_probe_points: usize,
}

impl Router {
    pub fn new(settings: Arc<Settings>, rpc: Arc<crate::rpc::RpcClients>) -> Self {
        let exact_quoter = ExactQuoter::new(settings.clone(), rpc.clone());
        let split_optimizer = SplitOptimizer::new(settings.clone(), exact_quoter.clone());
        let verify_split_optimizer = if env_bool("VERIFY_V3_WITH_RPC_QUOTER", true) {
            let exact_quoter = ExactQuoter::with_v3_rpc_quoter(settings.clone(), rpc.clone(), true);
            Some(SplitOptimizer::with_exact_quoter(
                settings.clone(),
                exact_quoter,
            ))
        } else {
            None
        };
        let quantity_searcher = QuantitySearcher::new(settings.clone());
        let fast_sizer = FastSizer::new();
        let verify_v3_refinement_points = env_usize("VERIFY_V3_REFINEMENT_POINTS", 3);
        let verify_v3_early_exit = env_bool("VERIFY_V3_EARLY_EXIT", true);
        let verify_v3_early_reoptimize = env_bool("VERIFY_V3_EARLY_REOPTIMIZE", false);
        let verify_v3_final_reoptimize = env_bool("VERIFY_V3_FINAL_REOPTIMIZE", false);
        let verify_v3_early_probe_points = env_usize("VERIFY_V3_EARLY_PROBE_POINTS", 1);
        let flash = FlashLoanEngine::new(settings, rpc);
        Self {
            split_optimizer,
            verify_split_optimizer,
            quantity_searcher,
            fast_sizer,
            flash,
            verify_v3_refinement_points,
            verify_v3_early_exit,
            verify_v3_early_reoptimize,
            verify_v3_final_reoptimize,
            verify_v3_early_probe_points,
        }
    }

    pub async fn search_best_plan(
        &self,
        snapshot: &GraphSnapshot,
        candidate: &CandidatePath,
    ) -> Result<Option<ExactPlan>> {
        Ok(self
            .search_best_plan_with_stats(snapshot, candidate)
            .await?
            .plan)
    }

    pub async fn search_best_plan_with_stats(
        &self,
        snapshot: &GraphSnapshot,
        candidate: &CandidatePath,
    ) -> Result<RouteSearchResult> {
        self.search_best_plan_with_stats_mode(snapshot, candidate, true)
            .await
    }

    pub async fn search_prerank_plan_with_stats(
        &self,
        snapshot: &GraphSnapshot,
        candidate: &CandidatePath,
    ) -> Result<RouteSearchResult> {
        self.search_best_plan_with_stats_mode(snapshot, candidate, false)
            .await
    }

    async fn search_best_plan_with_stats_mode(
        &self,
        snapshot: &GraphSnapshot,
        candidate: &CandidatePath,
        verify_final_plan: bool,
    ) -> Result<RouteSearchResult> {
        let mut stats = RouteSearchStats::default();
        let mut best = None::<ExactPlan>;
        let premium_ppm = self.flash.premium_ppm().await?;
        let Some((min_amount, max_amount)) =
            self.quantity_searcher.search_range(snapshot, candidate)
        else {
            stats.amount_range_missing = true;
            return Ok(RouteSearchResult { plan: None, stats });
        };
        stats.min_amount = min_amount;
        stats.max_amount = max_amount;
        let mut evaluated = std::collections::HashMap::<u128, Option<ExactPlan>>::new();

        if let Some(plan) = self
            .search_fast_sized(
                snapshot,
                candidate,
                min_amount,
                max_amount,
                premium_ppm,
                &mut evaluated,
                &mut stats,
            )
            .await?
        {
            stats.used_fast_sizer = true;
            let plan = if verify_final_plan {
                self.verify_best_plan(
                    snapshot,
                    candidate,
                    plan,
                    premium_ppm,
                    min_amount,
                    max_amount,
                    &mut stats,
                )
                .await?
            } else {
                Some(plan)
            };
            return Ok(RouteSearchResult { plan, stats });
        }

        let mut ordered = self.quantity_searcher.ladder(snapshot, candidate);
        if ordered.is_empty() {
            return Ok(RouteSearchResult { plan: None, stats });
        }

        for &amount in &ordered {
            if let Some(plan) = self
                .evaluate_amount(
                    snapshot,
                    candidate,
                    amount,
                    premium_ppm,
                    &mut evaluated,
                    &mut stats,
                )
                .await?
            {
                best = better_plan(best, plan);
            }
        }

        if best.is_none() {
            return Ok(RouteSearchResult { plan: None, stats });
        }

        if verify_final_plan
            && self.verify_v3_early_exit
            && self.verify_split_optimizer.is_some()
            && plan_uses_v3(best.as_ref().expect("best checked above"))
        {
            let Some(verified_probe) = self
                .verify_probe_plan(
                    snapshot,
                    candidate,
                    best.expect("best checked above"),
                    premium_ppm,
                    min_amount,
                    max_amount,
                    &mut stats,
                )
                .await?
            else {
                return Ok(RouteSearchResult { plan: None, stats });
            };
            let plan = self
                .verify_best_plan(
                    snapshot,
                    candidate,
                    verified_probe,
                    premium_ppm,
                    min_amount,
                    max_amount,
                    &mut stats,
                )
                .await?;
            return Ok(RouteSearchResult { plan, stats });
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
                    .evaluate_amount(
                        snapshot,
                        candidate,
                        amount,
                        premium_ppm,
                        &mut evaluated,
                        &mut stats,
                    )
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
                    .evaluate_profit(
                        snapshot,
                        candidate,
                        mid_left,
                        premium_ppm,
                        &mut evaluated,
                        &mut stats,
                    )
                    .await?;
                let right_profit = self
                    .evaluate_profit(
                        snapshot,
                        candidate,
                        mid_right,
                        premium_ppm,
                        &mut evaluated,
                        &mut stats,
                    )
                    .await?;
                if left_profit < right_profit {
                    low = mid_left.saturating_add(1);
                } else {
                    high = mid_right.saturating_sub(1).max(low);
                }
            }

            for amount in dense_points(low, high, 64) {
                if let Some(plan) = self
                    .evaluate_amount(
                        snapshot,
                        candidate,
                        amount,
                        premium_ppm,
                        &mut evaluated,
                        &mut stats,
                    )
                    .await?
                {
                    best = better_plan(best, plan);
                }
            }
        }

        let plan = match best {
            Some(plan) if verify_final_plan => {
                self.verify_best_plan(
                    snapshot,
                    candidate,
                    plan,
                    premium_ppm,
                    min_amount,
                    max_amount,
                    &mut stats,
                )
                .await?
            }
            Some(plan) => Some(plan),
            None => None,
        };

        Ok(RouteSearchResult { plan, stats })
    }

    async fn search_fast_sized(
        &self,
        snapshot: &GraphSnapshot,
        candidate: &CandidatePath,
        min_amount: u128,
        max_amount: u128,
        premium_ppm: u128,
        evaluated: &mut std::collections::HashMap<u128, Option<ExactPlan>>,
        stats: &mut RouteSearchStats,
    ) -> Result<Option<ExactPlan>> {
        let fast_points = self.fast_sizer.suggest_input_amounts(
            snapshot,
            candidate,
            min_amount,
            max_amount,
            premium_ppm,
        );
        stats.fast_sizer_points = fast_points.len();
        if fast_points.is_empty() {
            return Ok(None);
        }

        let mut best = None::<ExactPlan>;
        for &amount in &fast_points {
            if let Some(plan) = self
                .evaluate_amount(snapshot, candidate, amount, premium_ppm, evaluated, stats)
                .await?
            {
                best = better_plan(best, plan);
            }
        }
        let Some(initial_best) = best.clone() else {
            return Ok(None);
        };

        let center = initial_best.input_amount;
        let mut low = center
            .saturating_sub(center / 2)
            .max(min_amount)
            .min(max_amount);
        let mut high = center
            .saturating_add(center / 2)
            .max(center.saturating_add(1))
            .min(max_amount);
        if low >= high {
            low = min_amount.min(center);
            high = max_amount.max(center);
        }

        for _ in 0..12 {
            if high.saturating_sub(low) <= 32 {
                break;
            }
            let width = high - low;
            let mid_left = low + width / 3;
            let mid_right = high - width / 3;
            let left_profit = self
                .evaluate_profit(snapshot, candidate, mid_left, premium_ppm, evaluated, stats)
                .await?;
            let right_profit = self
                .evaluate_profit(
                    snapshot,
                    candidate,
                    mid_right,
                    premium_ppm,
                    evaluated,
                    stats,
                )
                .await?;
            if left_profit < right_profit {
                low = mid_left.saturating_add(1);
            } else {
                high = mid_right.saturating_sub(1).max(low);
            }
        }

        for amount in dense_points(low, high, 16) {
            if let Some(plan) = self
                .evaluate_amount(snapshot, candidate, amount, premium_ppm, evaluated, stats)
                .await?
            {
                best = better_plan(best, plan);
            }
        }

        let best = best.expect("fast search keeps an initial profitable plan");
        let needs_wide_fallback = best.input_amount == low && low > min_amount
            || best.input_amount == high && high < max_amount;
        if needs_wide_fallback {
            stats.used_wide_fallback = true;
            Ok(None)
        } else {
            Ok(Some(best))
        }
    }

    async fn evaluate_profit(
        &self,
        snapshot: &GraphSnapshot,
        candidate: &CandidatePath,
        amount: u128,
        premium_ppm: u128,
        evaluated: &mut std::collections::HashMap<u128, Option<ExactPlan>>,
        stats: &mut RouteSearchStats,
    ) -> Result<i128> {
        Ok(self
            .evaluate_amount(snapshot, candidate, amount, premium_ppm, evaluated, stats)
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
        stats: &mut RouteSearchStats,
    ) -> Result<Option<ExactPlan>> {
        if let Some(plan) = evaluated.get(&amount) {
            return Ok(plan.clone());
        }
        let plan = self
            .quote_candidate(snapshot, candidate, amount, premium_ppm, stats)
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
        stats: &mut RouteSearchStats,
    ) -> Result<Option<ExactPlan>> {
        self.quote_candidate_with_optimizer(
            &self.split_optimizer,
            snapshot,
            candidate,
            input_amount,
            flash_premium_ppm,
            stats,
        )
        .await
    }

    async fn quote_candidate_with_optimizer(
        &self,
        split_optimizer: &SplitOptimizer,
        snapshot: &GraphSnapshot,
        candidate: &CandidatePath,
        input_amount: u128,
        flash_premium_ppm: u128,
        stats: &mut RouteSearchStats,
    ) -> Result<Option<ExactPlan>> {
        stats.evaluated_amounts += 1;
        let mut next_amount = input_amount;
        let mut hops = Vec::<HopPlan>::new();

        for (hop_index, hop) in candidate.path.iter().enumerate() {
            let (splits, out) = split_optimizer
                .optimize_pair(snapshot, hop.from, hop.to, next_amount)
                .await?;
            if out == 0 || splits.is_empty() {
                stats.record_no_output(hop_index, hop.amm_kind, Some(hop.pool_id), next_amount);
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

        let rough_gas_limit = estimate_gas_limit(&hops);
        let gross_profit_raw = next_amount as i128 - input_amount as i128;
        if gross_profit_raw <= 0 {
            stats.gross_nonpositive += 1;
            return Ok(None);
        }
        let search_flash_fee_raw =
            estimated_full_flash_fee_raw(snapshot, candidate, input_amount, flash_premium_ppm);
        let expected_profit =
            gross_profit_raw.saturating_sub(u128_to_i128_saturating(search_flash_fee_raw));
        if expected_profit <= 0 {
            stats.flash_fee_nonpositive += 1;
            return Ok(None);
        }
        stats.profitable_plans += 1;

        // Capital selection owns flash-fee accounting because self-funded and
        // mixed routes only pay premium on the borrowed shortfall.
        Ok(Some(ExactPlan {
            snapshot_id: candidate.snapshot_id,
            input_token: candidate.start_token,
            output_token: candidate.start_token,
            input_amount,
            output_amount: next_amount,
            gross_profit_raw,
            flash_premium_ppm,
            flash_fee_raw: search_flash_fee_raw,
            net_profit_before_gas_raw: expected_profit,
            contract_min_profit_raw: 0, // Set later in validator
            input_value_usd_e8: 0,      // Calculated later in validator
            flash_loan_value_usd_e8: 0,
            gross_profit_usd_e8: 0, // Calculated later in validator
            flash_fee_usd_e8: 0,
            actual_flash_fee_usd_e8: 0,
            gas_cost_usd_e8: 0,
            net_profit_usd_e8: 0,
            expected_profit,
            gas_limit: rough_gas_limit,
            gas_l2_execution_cost_wei: alloy::primitives::U256::ZERO,
            gas_l1_data_fee_wei: alloy::primitives::U256::ZERO,
            gas_cost_wei: alloy::primitives::U256::ZERO,
            capital_source: CapitalSource::SelfFunded,
            flash_loan_amount: 0,
            actual_flash_fee_raw: 0,
            hops,
        }))
    }

    async fn verify_best_plan(
        &self,
        snapshot: &GraphSnapshot,
        candidate: &CandidatePath,
        plan: ExactPlan,
        premium_ppm: u128,
        min_amount: u128,
        max_amount: u128,
        stats: &mut RouteSearchStats,
    ) -> Result<Option<ExactPlan>> {
        let Some(split_optimizer) = &self.verify_split_optimizer else {
            return Ok(Some(plan));
        };
        if !plan_uses_v3(&plan) {
            return Ok(Some(plan));
        }

        let mut best = None::<ExactPlan>;
        for amount in verification_amounts(
            plan.input_amount,
            min_amount,
            max_amount,
            self.verify_v3_refinement_points,
        ) {
            let verified = if self.verify_v3_final_reoptimize {
                self.quote_candidate_with_optimizer(
                    split_optimizer,
                    snapshot,
                    candidate,
                    amount,
                    premium_ppm,
                    stats,
                )
                .await?
            } else {
                self.quote_existing_plan_splits(
                    split_optimizer,
                    snapshot,
                    candidate,
                    &plan,
                    amount,
                    premium_ppm,
                    stats,
                )
                .await?
            };
            if let Some(verified) = verified {
                best = better_plan(best, verified);
            }
        }
        Ok(best)
    }

    async fn verify_probe_plan(
        &self,
        snapshot: &GraphSnapshot,
        candidate: &CandidatePath,
        plan: ExactPlan,
        premium_ppm: u128,
        min_amount: u128,
        max_amount: u128,
        stats: &mut RouteSearchStats,
    ) -> Result<Option<ExactPlan>> {
        let Some(split_optimizer) = &self.verify_split_optimizer else {
            return Ok(Some(plan));
        };

        let mut best = None::<ExactPlan>;
        for amount in verification_amounts(
            plan.input_amount,
            min_amount,
            max_amount,
            self.verify_v3_early_probe_points,
        ) {
            let verified = if self.verify_v3_early_reoptimize {
                self.quote_candidate_with_optimizer(
                    split_optimizer,
                    snapshot,
                    candidate,
                    amount,
                    premium_ppm,
                    stats,
                )
                .await?
            } else {
                self.quote_existing_plan_splits(
                    split_optimizer,
                    snapshot,
                    candidate,
                    &plan,
                    amount,
                    premium_ppm,
                    stats,
                )
                .await?
            };
            if let Some(verified) = verified {
                best = better_plan(best, verified);
            }
        }
        Ok(best)
    }

    async fn quote_existing_plan_splits(
        &self,
        split_optimizer: &SplitOptimizer,
        snapshot: &GraphSnapshot,
        candidate: &CandidatePath,
        plan: &ExactPlan,
        input_amount: u128,
        flash_premium_ppm: u128,
        stats: &mut RouteSearchStats,
    ) -> Result<Option<ExactPlan>> {
        stats.evaluated_amounts += 1;
        let mut next_amount = input_amount;
        let mut verified_hops = Vec::<HopPlan>::new();

        for (hop_index, hop) in plan.hops.iter().enumerate() {
            let mut splits = scale_splits_for_input(&hop.splits, hop.total_in, next_amount);
            let mut total_out = 0u128;
            for split in &mut splits {
                let amount_out = split_optimizer.quote_split(snapshot, split).await?;
                if amount_out == 0 {
                    let amm_kind = snapshot
                        .pool(split.pool_id)
                        .map(|pool| pool.kind)
                        .unwrap_or(hop_amm_kind(candidate, hop_index));
                    stats.record_no_output(
                        hop_index,
                        amm_kind,
                        Some(split.pool_id),
                        split.amount_in,
                    );
                    return Ok(None);
                }
                split.expected_amount_out = amount_out;
                split.min_amount_out = min_amount_out(amount_out);
                total_out = total_out.saturating_add(amount_out);
            }
            if splits.is_empty() || total_out == 0 {
                stats.record_no_output(
                    hop_index,
                    hop_amm_kind(candidate, hop_index),
                    candidate.path.get(hop_index).map(|hop| hop.pool_id),
                    next_amount,
                );
                return Ok(None);
            }
            verified_hops.push(HopPlan {
                token_in: hop.token_in,
                token_out: hop.token_out,
                total_in: next_amount,
                total_out,
                splits,
            });
            next_amount = total_out;
        }

        let rough_gas_limit = estimate_gas_limit(&verified_hops);
        let gross_profit_raw = next_amount as i128 - input_amount as i128;
        if gross_profit_raw <= 0 {
            stats.gross_nonpositive += 1;
            return Ok(None);
        }
        let search_flash_fee_raw =
            estimated_full_flash_fee_raw(snapshot, candidate, input_amount, flash_premium_ppm);
        let expected_profit =
            gross_profit_raw.saturating_sub(u128_to_i128_saturating(search_flash_fee_raw));
        if expected_profit <= 0 {
            stats.flash_fee_nonpositive += 1;
            return Ok(None);
        }
        stats.profitable_plans += 1;

        Ok(Some(ExactPlan {
            snapshot_id: candidate.snapshot_id,
            input_token: candidate.start_token,
            output_token: candidate.start_token,
            input_amount,
            output_amount: next_amount,
            gross_profit_raw,
            flash_premium_ppm,
            flash_fee_raw: search_flash_fee_raw,
            net_profit_before_gas_raw: expected_profit,
            contract_min_profit_raw: 0,
            input_value_usd_e8: 0,
            flash_loan_value_usd_e8: 0,
            gross_profit_usd_e8: 0,
            flash_fee_usd_e8: 0,
            actual_flash_fee_usd_e8: 0,
            gas_cost_usd_e8: 0,
            net_profit_usd_e8: 0,
            expected_profit,
            gas_limit: rough_gas_limit,
            gas_l2_execution_cost_wei: alloy::primitives::U256::ZERO,
            gas_l1_data_fee_wei: alloy::primitives::U256::ZERO,
            gas_cost_wei: alloy::primitives::U256::ZERO,
            capital_source: CapitalSource::SelfFunded,
            flash_loan_amount: 0,
            actual_flash_fee_raw: 0,
            hops: verified_hops,
        }))
    }
}

fn scale_splits_for_input(
    splits: &[SplitPlan],
    original_total_in: u128,
    new_total_in: u128,
) -> Vec<SplitPlan> {
    if splits.is_empty() || original_total_in == 0 || new_total_in == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(splits.len());
    let mut assigned = 0u128;
    for (idx, split) in splits.iter().enumerate() {
        let mut scaled = split.clone();
        scaled.amount_in = if idx + 1 == splits.len() {
            new_total_in.saturating_sub(assigned)
        } else {
            new_total_in.saturating_mul(split.amount_in) / original_total_in
        };
        assigned = assigned.saturating_add(scaled.amount_in);
        if scaled.amount_in > 0 {
            out.push(scaled);
        }
    }
    out
}

fn hop_amm_kind(candidate: &CandidatePath, hop_index: usize) -> AmmKind {
    candidate
        .path
        .get(hop_index)
        .map(|hop| hop.amm_kind)
        .unwrap_or(AmmKind::UniswapV3Like)
}

fn estimated_full_flash_fee_raw(
    snapshot: &GraphSnapshot,
    candidate: &CandidatePath,
    input_amount: u128,
    flash_premium_ppm: u128,
) -> u128 {
    let assumes_full_flash_loan = snapshot
        .token_index(candidate.start_token)
        .and_then(|idx| snapshot.tokens.get(idx))
        .map(|token| token.flash_loan_enabled && !token.allow_self_funded)
        .unwrap_or(false);

    if assumes_full_flash_loan {
        input_amount.saturating_mul(flash_premium_ppm) / 1_000_000
    } else {
        0
    }
}

fn u128_to_i128_saturating(value: u128) -> i128 {
    i128::try_from(value).unwrap_or(i128::MAX)
}

fn min_amount_out(expected: u128) -> u128 {
    expected.saturating_mul(env_usize("SPLIT_MIN_OUTPUT_BPS", 9_950) as u128) / 10_000
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

fn estimate_gas_limit(hops: &[HopPlan]) -> u64 {
    let mut gas = 90_000u64;
    for hop in hops {
        gas = gas.saturating_add(20_000);
        for split in &hop.splits {
            gas = gas.saturating_add(match split.adapter_type {
                crate::types::AdapterType::UniswapV2Like => 75_000,
                crate::types::AdapterType::AerodromeV2Like => 100_000,
                crate::types::AdapterType::UniswapV3Like => 155_000,
                crate::types::AdapterType::CurvePlain => 240_000,
                crate::types::AdapterType::BalancerWeighted => 150_000,
            });
        }
    }
    gas
}

fn plan_uses_v3(plan: &ExactPlan) -> bool {
    plan.hops.iter().any(|hop| {
        hop.splits
            .iter()
            .any(|split| split.adapter_type == crate::types::AdapterType::UniswapV3Like)
    })
}

fn verification_amounts(
    center: u128,
    min_amount: u128,
    max_amount: u128,
    limit: usize,
) -> Vec<u128> {
    if limit == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let center = center.clamp(min_amount, max_amount);
    if center > 0 {
        out.push(center);
    }
    let factors_bps = [8_500u128, 11_500, 7_000, 13_000, 15_000, 5_000];
    for factor in factors_bps {
        if out.len() >= limit {
            break;
        }
        let amount = center
            .saturating_mul(factor)
            .checked_div(10_000)
            .unwrap_or(center)
            .clamp(min_amount, max_amount);
        if amount > 0 && !out.contains(&amount) {
            out.push(amount);
        }
    }
    out
}

fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .ok()
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(default)
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use alloy::primitives::Address;
    use smallvec::SmallVec;

    use crate::{
        graph::GraphSnapshot,
        types::{
            AdapterType, CandidatePath, HopPlan, SplitExtra, SplitPlan, TokenBehavior, TokenInfo,
        },
    };

    use super::{estimate_gas_limit, estimated_full_flash_fee_raw, scale_splits_for_input};

    fn addr(byte: u8) -> Address {
        Address::from_slice(&[byte; 20])
    }

    fn token(flash_loan_enabled: bool, allow_self_funded: bool) -> TokenInfo {
        TokenInfo {
            address: addr(1),
            symbol: "USDC".to_string(),
            decimals: 6,
            is_stable: true,
            is_cycle_anchor: flash_loan_enabled,
            flash_loan_enabled,
            allow_self_funded,
            behavior: TokenBehavior::default(),
            manual_price_usd_e8: Some(100_000_000),
            max_position_usd_e8: None,
            max_flash_loan_usd_e8: None,
        }
    }

    fn candidate() -> CandidatePath {
        CandidatePath {
            snapshot_id: 1,
            start_token: addr(1),
            start_symbol: "USDC".to_string(),
            screening_score_q32: 0,
            route_capacity_usd_e8: 0,
            route_quality_bps: 10_000,
            cycle_key: "cycle".to_string(),
            path: SmallVec::new(),
        }
    }

    #[test]
    fn search_objective_charges_fee_for_full_flash_loan_entry() {
        let snapshot = GraphSnapshot::build(1, None, vec![token(true, false)], HashMap::new());

        let fee = estimated_full_flash_fee_raw(&snapshot, &candidate(), 1_000_000, 500);

        assert_eq!(fee, 500);
    }

    #[test]
    fn search_objective_keeps_self_funded_entries_fee_neutral() {
        let snapshot = GraphSnapshot::build(1, None, vec![token(true, true)], HashMap::new());

        let fee = estimated_full_flash_fee_raw(&snapshot, &candidate(), 1_000_000, 500);

        assert_eq!(fee, 0);
    }

    #[test]
    fn scale_splits_preserves_total_with_rounding_remainder() {
        let split = |amount_in| SplitPlan {
            dex_name: "dex".to_string(),
            pool_id: addr(amount_in as u8),
            adapter_type: AdapterType::UniswapV2Like,
            token_in: addr(1),
            token_out: addr(2),
            amount_in,
            min_amount_out: 0,
            expected_amount_out: 0,
            extra: SplitExtra::V2 { fee_ppm: 3000 },
        };
        let scaled = scale_splits_for_input(&[split(3), split(7)], 10, 101);

        assert_eq!(scaled.len(), 2);
        assert_eq!(scaled[0].amount_in, 30);
        assert_eq!(scaled[1].amount_in, 71);
        assert_eq!(
            scaled.iter().map(|split| split.amount_in).sum::<u128>(),
            101
        );
    }

    #[test]
    fn gas_estimate_uses_materialized_split_adapter_types() {
        let split = |adapter_type, pool_id| SplitPlan {
            dex_name: "dex".to_string(),
            pool_id,
            adapter_type,
            token_in: addr(1),
            token_out: addr(2),
            amount_in: 1,
            min_amount_out: 1,
            expected_amount_out: 1,
            extra: SplitExtra::None,
        };
        let hops = vec![HopPlan {
            token_in: addr(1),
            token_out: addr(2),
            total_in: 2,
            total_out: 2,
            splits: vec![
                split(AdapterType::UniswapV3Like, addr(11)),
                split(AdapterType::BalancerWeighted, addr(12)),
            ],
        }];

        assert_eq!(
            estimate_gas_limit(&hops),
            90_000 + 20_000 + 155_000 + 150_000
        );
    }
}
