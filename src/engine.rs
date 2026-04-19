use std::{
    collections::HashSet,
    sync::Arc,
    time::{Duration, Instant},
};

use alloy::primitives::Address;
use anyhow::{Context, Result};
use tokio::time::sleep;
use tracing::{debug, info, warn};

use crate::{
    config::Settings,
    detector::Detector,
    discovery::{factory_scanner::DiscoveredPool, DiscoveryManager},
    execution::{
        nonce_manager::NonceManager,
        submitter::{ReceiptOutcome, Submitter},
        validator::Validator,
    },
    graph::{DistanceCache, GraphSnapshot, GraphStore},
    monitoring::metrics as telemetry,
    reorg::{ReorgDetector, ReorgEvent},
    risk::{depeg_guard::DepegGuard, limits::RiskManager, valuation::amount_to_usd_e8},
    router::{RouteSearchStats, Router},
    rpc::RpcClients,
    types::{
        BlockRef, CandidatePath, EdgeRef, ExactPlan, FinalityLevel, PoolSpecificState,
        RefreshBatch, SubmissionResult,
    },
};

pub async fn run_chain(
    settings: Arc<Settings>,
    once: bool,
    simulate_only_flag: bool,
) -> Result<()> {
    let simulate_only = simulate_only_flag || settings.simulation_only;
    validate_runtime_prerequisites(&settings, simulate_only)?;

    std::fs::create_dir_all("state").ok();

    let rpc = Arc::new(RpcClients::from_settings(&settings.rpc)?);
    let discovery = DiscoveryManager::new(settings.clone(), rpc.clone());
    let detector = Detector::new(settings.clone());
    let router = Router::new(settings.clone(), rpc.clone());
    let validator = Validator::new(settings.clone(), rpc.clone());
    let submitter = Submitter::new(settings.clone(), rpc.clone());
    let risk = RiskManager::new(&settings);
    let depeg_guard = DepegGuard::new(&settings);
    let nonce_manager = NonceManager::with_replacement(settings.execution.enable_nonce_replacement);

    let operator = validator
        .operator_address()
        .context("failed to derive operator address from OPERATOR_PRIVATE_KEY")?;
    nonce_manager
        .sync(&rpc.best_read(), operator)
        .await
        .context("failed to sync operator nonce")?;

    info!(chain = %settings.chain, simulate_only, "bootstrapping discovery");
    let bootstrap = discovery
        .bootstrap()
        .await
        .context("bootstrap discovery failed")?;
    info!(
        chain = %settings.chain,
        token_count = bootstrap.tokens.len(),
        pool_count = bootstrap.pools.len(),
        snapshot_id = bootstrap.snapshot.snapshot_id,
        "bootstrap complete"
    );

    let store = GraphStore::new(bootstrap.snapshot, 8);
    let reorg_detector = ReorgDetector::new();
    let initial_snapshot = store.load();
    if let Some(block_ref) = initial_snapshot.block_ref.clone() {
        reorg_detector.update_chain(block_ref).ok();
    }
    let initial_distance_cache = DistanceCache::recompute(initial_snapshot.as_ref());
    let initial_changed_edges =
        initial_edge_refs(initial_snapshot.as_ref(), &initial_distance_cache);
    info!(
        selected_edges = initial_changed_edges.len(),
        total_edges = total_edge_count(initial_snapshot.as_ref()),
        "initial refresh edge set selected"
    );
    process_refresh(
        settings.clone(),
        initial_snapshot,
        initial_changed_edges,
        &detector,
        &router,
        &validator,
        &submitter,
        &risk,
        &depeg_guard,
        &nonce_manager,
        simulate_only,
    )
    .await?;

    if once {
        return Ok(());
    }

    let event_stream = discovery.event_stream(rpc.clone());
    let mut poll_from_block = store
        .load()
        .block_ref
        .as_ref()
        .map(|block| {
            block
                .number
                .saturating_sub(settings.risk.event_backfill_blocks)
        })
        .unwrap_or(0);

    loop {
        let snapshot = store.load();
        let poll_result = event_stream
            .poll_once(snapshot.as_ref(), poll_from_block)
            .await;
        let (latest_block, batch) = match poll_result {
            Ok(result) => result,
            Err(err) => {
                warn!(error = %err, from_block = poll_from_block, "event polling failed");
                sleep(Duration::from_millis(settings.risk.poll_interval_ms)).await;
                continue;
            }
        };
        poll_from_block = latest_block;

        let mut reorg_handled = false;
        if let Ok(Some(block_ref)) = current_block_ref(rpc.as_ref()).await.map(Some) {
            telemetry::record_state_lag(
                latest_block.saturating_sub(block_ref.number),
                settings.chain.as_str(),
            );
            if let Some(event) = reorg_detector.update_chain(block_ref)? {
                handle_reorg_event(&settings, &store, &event, &mut poll_from_block);
                reorg_handled = true;
            }
        }
        if reorg_handled {
            sleep(Duration::from_millis(settings.risk.poll_interval_ms)).await;
            continue;
        }

        if batch.triggers.is_empty() {
            sleep(Duration::from_millis(settings.risk.poll_interval_ms)).await;
            continue;
        }

        let changed_specs = collect_changed_specs(snapshot.as_ref(), &batch);
        let patches = collect_patches(&batch);
        if changed_specs.is_empty() && patches.is_empty() {
            debug!(
                trigger_count = batch.triggers.len(),
                "no matching pool specs for refresh batch"
            );
            sleep(Duration::from_millis(settings.risk.poll_interval_ms)).await;
            continue;
        }

        info!(
            trigger_count = batch.triggers.len(),
            refresh_pool_count = changed_specs.len(),
            patch_count = patches.len(),
            latest_block,
            "refreshing changed pools"
        );

        let block_ref = current_block_ref(rpc.as_ref()).await.ok();
        let (next_snapshot, changed_pool_ids) = discovery
            .refresh_pools(
                snapshot.snapshot_id,
                snapshot.tokens.clone(),
                snapshot.pools.clone(),
                changed_specs,
                patches,
                block_ref,
            )
            .await
            .context("refresh_pools failed")?;

        let changed_edges = changed_pool_ids
            .iter()
            .flat_map(|pool_id| {
                next_snapshot
                    .pool_to_edges
                    .get(pool_id)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
            })
            .collect::<Vec<_>>();

        if changed_edges.is_empty() {
            debug!("refresh produced no changed edges");
            sleep(Duration::from_millis(settings.risk.poll_interval_ms)).await;
            continue;
        }

        let published = store.publish(next_snapshot);
        telemetry::record_snapshot_update(
            published.snapshot_id,
            changed_pool_ids.len(),
            changed_edges.len(),
            settings.chain.as_str(),
        );
        process_refresh(
            settings.clone(),
            published,
            changed_edges,
            &detector,
            &router,
            &validator,
            &submitter,
            &risk,
            &depeg_guard,
            &nonce_manager,
            simulate_only,
        )
        .await?;

        sleep(Duration::from_millis(settings.risk.poll_interval_ms)).await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn process_refresh(
    settings: Arc<Settings>,
    snapshot: Arc<GraphSnapshot>,
    changed_edges: Vec<EdgeRef>,
    detector: &Detector,
    router: &Router,
    validator: &Validator,
    submitter: &Submitter,
    risk: &RiskManager,
    depeg_guard: &DepegGuard,
    nonce_manager: &NonceManager,
    simulate_only: bool,
) -> Result<()> {
    if changed_edges.is_empty() {
        return Ok(());
    }

    // Check stable token depeg status
    if !depeg_guard.stable_routes_allowed() {
        warn!("stable tokens detected as depegged, rejecting all candidates");
        return Ok(());
    }

    let distance_cache = DistanceCache::recompute(snapshot.as_ref());
    let detect_started = Instant::now();
    let mut candidates = detector.detect(snapshot.as_ref(), &changed_edges, &distance_cache);
    let detect_ms = detect_started.elapsed().as_millis();
    for _ in &candidates {
        telemetry::record_candidate_detected(snapshot.snapshot_id, settings.chain.as_str());
    }
    info!(
        snapshot_id = snapshot.snapshot_id,
        changed_edge_count = changed_edges.len(),
        candidate_count = candidates.len(),
        detect_ms,
        "candidate detection complete"
    );
    if candidates.is_empty() {
        return Ok(());
    }
    candidates.sort_by_key(|candidate| candidate.screening_score_q32);

    info!(
        snapshot_id = snapshot.snapshot_id,
        candidate_count = candidates.len(),
        changed_edge_count = changed_edges.len(),
        simulate_only,
        "processing candidate set"
    );

    let refresh_started = Instant::now();
    let mut profitability = ProfitabilityRefreshStats::new(candidates.len());
    let no_route_sample_limit = env_usize("NO_ROUTE_SAMPLE_LOG_LIMIT", 0);
    let mut no_route_samples = 0usize;

    for candidate in candidates {
        let result = process_candidate(
            settings.clone(),
            snapshot.as_ref(),
            candidate.clone(),
            router,
            validator,
            submitter,
            risk,
            depeg_guard,
            nonce_manager,
            simulate_only,
        )
        .await?;
        if matches!(result.verdict, CandidateVerdict::NoRoute)
            && no_route_samples < no_route_sample_limit
        {
            no_route_samples += 1;
            info!(
                snapshot_id = snapshot.snapshot_id,
                sample_index = no_route_samples,
                cycle_key = %candidate.cycle_key,
                anchor_symbol = %candidate.start_symbol,
                screening_score_q32 = candidate.screening_score_q32,
                route = %format_candidate_route(&candidate),
                dex_route = %format_candidate_dex_route(&candidate),
                path_len = candidate.path.len(),
                router_ms = result.router_ms,
                candidate_total_ms = result.total_ms,
                route_amount_range_missing = result.route_stats.amount_range_missing,
                route_evaluated_amounts = result.route_stats.evaluated_amounts,
                route_no_output_quotes = result.route_stats.no_output_quotes,
                route_no_output_hop0 = result.route_stats.no_output_hop0,
                route_no_output_hop1 = result.route_stats.no_output_hop1,
                route_no_output_hop2 = result.route_stats.no_output_hop2,
                route_no_output_hop3_plus = result.route_stats.no_output_hop3_plus,
                route_no_output_v2 = result.route_stats.no_output_v2,
                route_no_output_aerodrome_v2 = result.route_stats.no_output_aerodrome_v2,
                route_no_output_v3 = result.route_stats.no_output_v3,
                route_no_output_curve = result.route_stats.no_output_curve,
                route_no_output_balancer = result.route_stats.no_output_balancer,
                route_gross_nonpositive = result.route_stats.gross_nonpositive,
                route_flash_fee_nonpositive = result.route_stats.flash_fee_nonpositive,
                route_profitable_plans = result.route_stats.profitable_plans,
                min_amount = result.route_stats.min_amount,
                max_amount = result.route_stats.max_amount,
                "no-route candidate sample"
            );
        }
        profitability.record(&result);
        if let Some(result) = result.submission {
            info!(tx_hash = %result.tx_hash, channel = %result.channel, "submission accepted");
        }
    }
    profitability.log(
        &settings,
        snapshot.snapshot_id,
        changed_edges.len(),
        refresh_started.elapsed(),
    );

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn process_candidate(
    settings: Arc<Settings>,
    snapshot: &GraphSnapshot,
    candidate: CandidatePath,
    router: &Router,
    validator: &Validator,
    submitter: &Submitter,
    risk: &RiskManager,
    depeg_guard: &DepegGuard,
    nonce_manager: &NonceManager,
    simulate_only: bool,
) -> Result<CandidateProcessResult> {
    let total_started = Instant::now();
    let router_started = Instant::now();
    let route_result = match router
        .search_best_plan_with_stats(snapshot, &candidate)
        .await
    {
        Ok(result) => result,
        Err(err) => {
            let router_ms = router_started.elapsed().as_millis();
            warn!(
                error = %err,
                snapshot_id = snapshot.snapshot_id,
                cycle_key = %candidate.cycle_key,
                router_ms,
                "route search failed; rejecting candidate without stopping engine"
            );
            telemetry::record_candidate_rejected("router_error", settings.chain.as_str());
            return Ok(CandidateProcessResult::new(
                CandidateVerdict::NoRoute,
                None,
                None,
                RouteSearchStats::default(),
                router_ms,
                0,
                total_started.elapsed().as_millis(),
            ));
        }
    };
    let route_stats = route_result.stats;
    let Some(plan) = route_result.plan else {
        let router_ms = router_started.elapsed().as_millis();
        return Ok(CandidateProcessResult::new(
            CandidateVerdict::NoRoute,
            None,
            None,
            route_stats,
            router_ms,
            0,
            total_started.elapsed().as_millis(),
        ));
    };
    let router_ms = router_started.elapsed().as_millis();

    let validator_started = Instant::now();
    let prepared = match validator.prepare(plan, snapshot).await {
        Ok(prepared) => prepared,
        Err(err) => {
            let validator_ms = validator_started.elapsed().as_millis();
            warn!(
                error = %err,
                snapshot_id = snapshot.snapshot_id,
                cycle_key = %candidate.cycle_key,
                router_ms,
                validator_ms,
                "candidate validation failed; rejecting candidate without stopping engine"
            );
            telemetry::record_candidate_rejected("validation_error", settings.chain.as_str());
            return Ok(CandidateProcessResult::new(
                CandidateVerdict::ValidationRejected,
                None,
                None,
                route_stats,
                router_ms,
                validator_ms,
                total_started.elapsed().as_millis(),
            ));
        }
    };
    let Some(mut executable) = prepared else {
        telemetry::record_candidate_rejected("validation", settings.chain.as_str());
        return Ok(CandidateProcessResult::new(
            CandidateVerdict::ValidationRejected,
            None,
            None,
            route_stats,
            router_ms,
            validator_started.elapsed().as_millis(),
            total_started.elapsed().as_millis(),
        ));
    };
    let validator_ms = validator_started.elapsed().as_millis();
    telemetry::record_candidate_validated(executable.exact.snapshot_id, settings.chain.as_str());
    telemetry::record_opportunity_found(
        executable.exact.snapshot_id,
        i128_to_i64_saturating(executable.exact.net_profit_usd_e8),
        settings.chain.as_str(),
    );
    telemetry::record_pnl_expected(
        i128_to_i64_saturating(executable.exact.net_profit_usd_e8),
        settings.chain.as_str(),
    );
    telemetry::record_opportunity_profitability(
        settings.chain.as_str(),
        i128_to_i64_saturating(executable.exact.net_profit_usd_e8),
        i128_to_i64_saturating(executable.exact.gross_profit_usd_e8),
        i128_to_i64_saturating(executable.exact.gas_cost_usd_e8),
        i128_to_i64_saturating(executable.exact.actual_flash_fee_usd_e8),
        u128_to_u64_saturating(executable.exact.input_value_usd_e8),
        ratio_bps(
            executable.exact.net_profit_usd_e8,
            executable.exact.input_value_usd_e8,
        ),
        ratio_bps(
            executable.exact.gas_cost_usd_e8,
            executable.exact.gross_profit_usd_e8.max(0) as u128,
        ),
    );
    telemetry::record_route_latency_seconds("router", millis_to_secs(router_ms));
    telemetry::record_route_latency_seconds("validator", millis_to_secs(validator_ms));
    telemetry::record_route_latency_seconds(
        "candidate_total",
        millis_to_secs(total_started.elapsed().as_millis()),
    );

    // Check depeg status for all stable tokens in the route
    for hop in &executable.exact.hops {
        for pool_address in [&hop.token_in, &hop.token_out] {
            if !depeg_guard.token_is_healthy(*pool_address) {
                debug!(
                    token = %pool_address,
                    "token is unhealthy (depegged), rejecting candidate"
                );
                telemetry::record_candidate_rejected("depeg", settings.chain.as_str());
                return Ok(CandidateProcessResult::new(
                    CandidateVerdict::DepegRejected,
                    None,
                    Some(executable.exact.clone()),
                    route_stats,
                    router_ms,
                    validator_ms,
                    total_started.elapsed().as_millis(),
                ));
            }
        }
    }

    if let Err(err) = risk.pre_trade_check(&executable) {
        debug!(error = %err, path_len = candidate.path.len(), "risk pre-trade check rejected plan");
        telemetry::record_candidate_rejected("risk", settings.chain.as_str());
        return Ok(CandidateProcessResult::new(
            CandidateVerdict::RiskRejected,
            None,
            Some(executable.exact),
            route_stats,
            router_ms,
            validator_ms,
            total_started.elapsed().as_millis(),
        ));
    }

    if simulate_only {
        let total_ms = total_started.elapsed().as_millis();
        info!(
            chain = %settings.chain,
            snapshot_id = executable.exact.snapshot_id,
            anchor_symbol = %candidate.start_symbol,
            route = %format_candidate_route(&candidate),
            dex_route = %format_candidate_dex_route(&candidate),
            input_token = %candidate.start_token,
            path_len = candidate.path.len(),
            input_amount = executable.exact.input_amount,
            output_amount = executable.exact.output_amount,
            gross_profit_raw = executable.exact.gross_profit_raw,
            gross_profit_usd_e8 = executable.exact.gross_profit_usd_e8,
            flash_fee_raw = executable.exact.flash_fee_raw,
            flash_fee_usd_e8 = executable.exact.flash_fee_usd_e8,
            flash_loan_amount = executable.exact.flash_loan_amount,
            flash_loan_value_usd_e8 = executable.exact.flash_loan_value_usd_e8,
            actual_flash_fee_raw = executable.exact.actual_flash_fee_raw,
            actual_flash_fee_usd_e8 = executable.exact.actual_flash_fee_usd_e8,
            gas_cost_usd_e8 = executable.exact.gas_cost_usd_e8,
            gas_l2_execution_cost_wei = %executable.exact.gas_l2_execution_cost_wei,
            gas_l1_data_fee_wei = %executable.exact.gas_l1_data_fee_wei,
            gas_total_cost_wei = %executable.exact.gas_cost_wei,
            net_profit_usd_e8 = executable.exact.net_profit_usd_e8,
            profit_margin_bps = ratio_bps(executable.exact.net_profit_usd_e8, executable.exact.input_value_usd_e8),
            gas_to_gross_bps = ratio_bps(
                executable.exact.gas_cost_usd_e8,
                executable.exact.gross_profit_usd_e8.max(0) as u128,
            ),
            contract_min_profit_raw = executable.exact.contract_min_profit_raw,
            capital_source = ?executable.exact.capital_source,
            router_ms,
            validator_ms,
            candidate_total_ms = total_ms,
            "validated opportunity (simulation only)"
        );
        return Ok(CandidateProcessResult::new(
            CandidateVerdict::Simulated,
            None,
            Some(executable.exact),
            route_stats,
            router_ms,
            validator_ms,
            total_ms,
        ));
    }

    executable.nonce = nonce_manager.reserve();
    risk.mark_submitted();

    match submitter.submit(&executable).await {
        Ok(result) => {
            telemetry::record_transaction_submitted(
                &result.tx_hash.to_string(),
                &result.channel,
                settings.chain.as_str(),
            );
            nonce_manager.mark_submitted(
                executable.nonce,
                result.tx_hash,
                executable.max_priority_fee_per_gas,
                executable.max_fee_per_gas,
            );
            risk.on_submission_result(&result);
            match submitter.wait_for_receipt(result.tx_hash).await {
                Ok(ReceiptOutcome::Included) => {
                    nonce_manager.mark_included(executable.nonce);
                    let realized = i128_to_i64_saturating(executable.exact.net_profit_usd_e8);
                    risk.mark_finalized(i128::from(realized));
                    telemetry::record_transaction_included(
                        &result.tx_hash.to_string(),
                        settings.chain.as_str(),
                        executable.exact.gas_limit,
                    );
                    telemetry::record_pnl_realized(realized, settings.chain.as_str());
                    Ok(CandidateProcessResult::new(
                        CandidateVerdict::Submitted,
                        Some(result),
                        Some(executable.exact),
                        route_stats,
                        router_ms,
                        validator_ms,
                        total_started.elapsed().as_millis(),
                    ))
                }
                Ok(ReceiptOutcome::Reverted) => {
                    nonce_manager.mark_included(executable.nonce);
                    risk.mark_finalized(-executable.exact.gas_cost_usd_e8);
                    telemetry::record_transaction_reverted(
                        &result.tx_hash.to_string(),
                        settings.chain.as_str(),
                        "receipt_status_0",
                    );
                    telemetry::record_pnl_realized(
                        i128_to_i64_saturating(-executable.exact.gas_cost_usd_e8),
                        settings.chain.as_str(),
                    );
                    warn!(tx_hash = %result.tx_hash, nonce = executable.nonce, "submitted transaction reverted");
                    Ok(CandidateProcessResult::new(
                        CandidateVerdict::SubmissionRejected,
                        None,
                        Some(executable.exact),
                        route_stats,
                        router_ms,
                        validator_ms,
                        total_started.elapsed().as_millis(),
                    ))
                }
                Ok(ReceiptOutcome::TimedOut) => {
                    risk.mark_failed_submission();
                    telemetry::record_transaction_failed(
                        &result.tx_hash.to_string(),
                        settings.chain.as_str(),
                        "receipt_timeout",
                    );
                    warn!(tx_hash = %result.tx_hash, nonce = executable.nonce, "submitted transaction receipt wait timed out");
                    Ok(CandidateProcessResult::new(
                        CandidateVerdict::SubmissionRejected,
                        None,
                        Some(executable.exact),
                        route_stats,
                        router_ms,
                        validator_ms,
                        total_started.elapsed().as_millis(),
                    ))
                }
                Err(err) => {
                    risk.mark_failed_submission();
                    telemetry::record_transaction_failed(
                        &result.tx_hash.to_string(),
                        settings.chain.as_str(),
                        "receipt_error",
                    );
                    warn!(error = %err, tx_hash = %result.tx_hash, nonce = executable.nonce, "failed while waiting for transaction receipt");
                    Ok(CandidateProcessResult::new(
                        CandidateVerdict::SubmissionRejected,
                        None,
                        Some(executable.exact),
                        route_stats,
                        router_ms,
                        validator_ms,
                        total_started.elapsed().as_millis(),
                    ))
                }
            }
        }
        Err(err) => {
            nonce_manager.mark_dropped(executable.nonce);
            risk.mark_failed_submission();
            telemetry::record_candidate_rejected("submission", settings.chain.as_str());
            warn!(error = %err, nonce = executable.nonce, "submission failed");
            Ok(CandidateProcessResult::new(
                CandidateVerdict::SubmissionRejected,
                None,
                Some(executable.exact),
                route_stats,
                router_ms,
                validator_ms,
                total_started.elapsed().as_millis(),
            ))
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum CandidateVerdict {
    NoRoute,
    ValidationRejected,
    DepegRejected,
    RiskRejected,
    Simulated,
    Submitted,
    SubmissionRejected,
}

#[derive(Debug)]
struct CandidateProcessResult {
    verdict: CandidateVerdict,
    submission: Option<SubmissionResult>,
    exact: Option<ExactPlan>,
    route_stats: RouteSearchStats,
    router_ms: u128,
    validator_ms: u128,
    total_ms: u128,
}

impl CandidateProcessResult {
    fn new(
        verdict: CandidateVerdict,
        submission: Option<SubmissionResult>,
        exact: Option<ExactPlan>,
        route_stats: RouteSearchStats,
        router_ms: u128,
        validator_ms: u128,
        total_ms: u128,
    ) -> Self {
        Self {
            verdict,
            submission,
            exact,
            route_stats,
            router_ms,
            validator_ms,
            total_ms,
        }
    }
}

#[derive(Debug, Default)]
struct ProfitabilityRefreshStats {
    candidate_count: usize,
    no_route: usize,
    validation_rejected: usize,
    depeg_rejected: usize,
    risk_rejected: usize,
    simulated: usize,
    submitted: usize,
    submission_rejected: usize,
    total_router_ms: u128,
    total_validator_ms: u128,
    total_candidate_ms: u128,
    max_candidate_ms: u128,
    route_amount_range_missing: usize,
    route_evaluated_amounts: usize,
    route_no_output_quotes: usize,
    route_no_output_hop0: usize,
    route_no_output_hop1: usize,
    route_no_output_hop2: usize,
    route_no_output_hop3_plus: usize,
    route_no_output_v2: usize,
    route_no_output_aerodrome_v2: usize,
    route_no_output_v3: usize,
    route_no_output_curve: usize,
    route_no_output_balancer: usize,
    route_gross_nonpositive: usize,
    route_flash_fee_nonpositive: usize,
    route_profitable_plans: usize,
    route_fast_sizer_points: usize,
    route_used_fast_sizer: usize,
    route_used_wide_fallback: usize,
    best: Option<BestProfit>,
}

#[derive(Debug, Clone)]
struct BestProfit {
    net_profit_usd_e8: i128,
    gross_profit_usd_e8: i128,
    gas_cost_usd_e8: i128,
    gas_l2_execution_cost_wei: String,
    gas_l1_data_fee_wei: String,
    gas_total_cost_wei: String,
    actual_flash_fee_usd_e8: i128,
    input_value_usd_e8: u128,
    flash_loan_value_usd_e8: u128,
    profit_margin_bps: i64,
    gas_to_gross_bps: i64,
    path_len: usize,
    capital_source: String,
}

impl ProfitabilityRefreshStats {
    fn new(candidate_count: usize) -> Self {
        Self {
            candidate_count,
            ..Self::default()
        }
    }

    fn record(&mut self, result: &CandidateProcessResult) {
        match result.verdict {
            CandidateVerdict::NoRoute => self.no_route += 1,
            CandidateVerdict::ValidationRejected => self.validation_rejected += 1,
            CandidateVerdict::DepegRejected => self.depeg_rejected += 1,
            CandidateVerdict::RiskRejected => self.risk_rejected += 1,
            CandidateVerdict::Simulated => self.simulated += 1,
            CandidateVerdict::Submitted => self.submitted += 1,
            CandidateVerdict::SubmissionRejected => self.submission_rejected += 1,
        }
        self.total_router_ms = self.total_router_ms.saturating_add(result.router_ms);
        self.total_validator_ms = self.total_validator_ms.saturating_add(result.validator_ms);
        self.total_candidate_ms = self.total_candidate_ms.saturating_add(result.total_ms);
        self.max_candidate_ms = self.max_candidate_ms.max(result.total_ms);
        let route = &result.route_stats;
        if route.amount_range_missing {
            self.route_amount_range_missing += 1;
        }
        self.route_evaluated_amounts = self
            .route_evaluated_amounts
            .saturating_add(route.evaluated_amounts);
        self.route_no_output_quotes = self
            .route_no_output_quotes
            .saturating_add(route.no_output_quotes);
        self.route_no_output_hop0 = self
            .route_no_output_hop0
            .saturating_add(route.no_output_hop0);
        self.route_no_output_hop1 = self
            .route_no_output_hop1
            .saturating_add(route.no_output_hop1);
        self.route_no_output_hop2 = self
            .route_no_output_hop2
            .saturating_add(route.no_output_hop2);
        self.route_no_output_hop3_plus = self
            .route_no_output_hop3_plus
            .saturating_add(route.no_output_hop3_plus);
        self.route_no_output_v2 = self.route_no_output_v2.saturating_add(route.no_output_v2);
        self.route_no_output_aerodrome_v2 = self
            .route_no_output_aerodrome_v2
            .saturating_add(route.no_output_aerodrome_v2);
        self.route_no_output_v3 = self.route_no_output_v3.saturating_add(route.no_output_v3);
        self.route_no_output_curve = self
            .route_no_output_curve
            .saturating_add(route.no_output_curve);
        self.route_no_output_balancer = self
            .route_no_output_balancer
            .saturating_add(route.no_output_balancer);
        self.route_gross_nonpositive = self
            .route_gross_nonpositive
            .saturating_add(route.gross_nonpositive);
        self.route_flash_fee_nonpositive = self
            .route_flash_fee_nonpositive
            .saturating_add(route.flash_fee_nonpositive);
        self.route_profitable_plans = self
            .route_profitable_plans
            .saturating_add(route.profitable_plans);
        self.route_fast_sizer_points = self
            .route_fast_sizer_points
            .saturating_add(route.fast_sizer_points);
        if route.used_fast_sizer {
            self.route_used_fast_sizer += 1;
        }
        if route.used_wide_fallback {
            self.route_used_wide_fallback += 1;
        }

        if matches!(
            result.verdict,
            CandidateVerdict::Simulated | CandidateVerdict::Submitted
        ) {
            if let Some(exact) = &result.exact {
                self.record_best(exact);
            }
        }
    }

    fn record_best(&mut self, exact: &ExactPlan) {
        let next = BestProfit {
            net_profit_usd_e8: exact.net_profit_usd_e8,
            gross_profit_usd_e8: exact.gross_profit_usd_e8,
            gas_cost_usd_e8: exact.gas_cost_usd_e8,
            gas_l2_execution_cost_wei: exact.gas_l2_execution_cost_wei.to_string(),
            gas_l1_data_fee_wei: exact.gas_l1_data_fee_wei.to_string(),
            gas_total_cost_wei: exact.gas_cost_wei.to_string(),
            actual_flash_fee_usd_e8: exact.actual_flash_fee_usd_e8,
            input_value_usd_e8: exact.input_value_usd_e8,
            flash_loan_value_usd_e8: exact.flash_loan_value_usd_e8,
            profit_margin_bps: ratio_bps(exact.net_profit_usd_e8, exact.input_value_usd_e8),
            gas_to_gross_bps: ratio_bps(
                exact.gas_cost_usd_e8,
                exact.gross_profit_usd_e8.max(0) as u128,
            ),
            path_len: exact.hops.len(),
            capital_source: format!("{:?}", exact.capital_source),
        };
        if self
            .best
            .as_ref()
            .map(|best| next.net_profit_usd_e8 > best.net_profit_usd_e8)
            .unwrap_or(true)
        {
            self.best = Some(next);
        }
    }

    fn log(
        &self,
        settings: &Settings,
        snapshot_id: u64,
        changed_edge_count: usize,
        refresh_duration: Duration,
    ) {
        let avg_candidate_ms = avg_ms(self.total_candidate_ms, self.candidate_count);
        let avg_router_ms = avg_ms(self.total_router_ms, self.candidate_count);
        let avg_validator_ms = avg_ms(self.total_validator_ms, self.candidate_count);
        if let Some(best) = &self.best {
            info!(
                chain = %settings.chain,
                snapshot_id,
                candidate_count = self.candidate_count,
                changed_edge_count,
                no_route = self.no_route,
                validation_rejected = self.validation_rejected,
                depeg_rejected = self.depeg_rejected,
                risk_rejected = self.risk_rejected,
                simulated = self.simulated,
                submitted = self.submitted,
                submission_rejected = self.submission_rejected,
                best_net_profit_usd_e8 = best.net_profit_usd_e8,
                best_gross_profit_usd_e8 = best.gross_profit_usd_e8,
                best_gas_cost_usd_e8 = best.gas_cost_usd_e8,
                best_gas_l2_execution_cost_wei = %best.gas_l2_execution_cost_wei,
                best_gas_l1_data_fee_wei = %best.gas_l1_data_fee_wei,
                best_gas_total_cost_wei = %best.gas_total_cost_wei,
                best_actual_flash_fee_usd_e8 = best.actual_flash_fee_usd_e8,
                best_input_value_usd_e8 = best.input_value_usd_e8,
                best_flash_loan_value_usd_e8 = best.flash_loan_value_usd_e8,
                best_profit_margin_bps = best.profit_margin_bps,
                best_gas_to_gross_bps = best.gas_to_gross_bps,
                best_path_len = best.path_len,
                best_capital_source = %best.capital_source,
                avg_candidate_ms,
                avg_router_ms,
                avg_validator_ms,
                max_candidate_ms = self.max_candidate_ms,
                route_amount_range_missing = self.route_amount_range_missing,
                route_evaluated_amounts = self.route_evaluated_amounts,
                route_no_output_quotes = self.route_no_output_quotes,
                route_no_output_hop0 = self.route_no_output_hop0,
                route_no_output_hop1 = self.route_no_output_hop1,
                route_no_output_hop2 = self.route_no_output_hop2,
                route_no_output_hop3_plus = self.route_no_output_hop3_plus,
                route_no_output_v2 = self.route_no_output_v2,
                route_no_output_aerodrome_v2 = self.route_no_output_aerodrome_v2,
                route_no_output_v3 = self.route_no_output_v3,
                route_no_output_curve = self.route_no_output_curve,
                route_no_output_balancer = self.route_no_output_balancer,
                route_gross_nonpositive = self.route_gross_nonpositive,
                route_flash_fee_nonpositive = self.route_flash_fee_nonpositive,
                route_profitable_plans = self.route_profitable_plans,
                route_fast_sizer_points = self.route_fast_sizer_points,
                route_used_fast_sizer = self.route_used_fast_sizer,
                route_used_wide_fallback = self.route_used_wide_fallback,
                refresh_total_ms = refresh_duration.as_millis(),
                "profitability refresh summary"
            );
        } else {
            info!(
                chain = %settings.chain,
                snapshot_id,
                candidate_count = self.candidate_count,
                changed_edge_count,
                no_route = self.no_route,
                validation_rejected = self.validation_rejected,
                depeg_rejected = self.depeg_rejected,
                risk_rejected = self.risk_rejected,
                simulated = self.simulated,
                submitted = self.submitted,
                submission_rejected = self.submission_rejected,
                avg_candidate_ms,
                avg_router_ms,
                avg_validator_ms,
                max_candidate_ms = self.max_candidate_ms,
                route_amount_range_missing = self.route_amount_range_missing,
                route_evaluated_amounts = self.route_evaluated_amounts,
                route_no_output_quotes = self.route_no_output_quotes,
                route_no_output_hop0 = self.route_no_output_hop0,
                route_no_output_hop1 = self.route_no_output_hop1,
                route_no_output_hop2 = self.route_no_output_hop2,
                route_no_output_hop3_plus = self.route_no_output_hop3_plus,
                route_no_output_v2 = self.route_no_output_v2,
                route_no_output_aerodrome_v2 = self.route_no_output_aerodrome_v2,
                route_no_output_v3 = self.route_no_output_v3,
                route_no_output_curve = self.route_no_output_curve,
                route_no_output_balancer = self.route_no_output_balancer,
                route_gross_nonpositive = self.route_gross_nonpositive,
                route_flash_fee_nonpositive = self.route_flash_fee_nonpositive,
                route_profitable_plans = self.route_profitable_plans,
                route_fast_sizer_points = self.route_fast_sizer_points,
                route_used_fast_sizer = self.route_used_fast_sizer,
                route_used_wide_fallback = self.route_used_wide_fallback,
                refresh_total_ms = refresh_duration.as_millis(),
                "profitability refresh summary"
            );
        }
    }
}

fn avg_ms(total: u128, count: usize) -> u128 {
    if count == 0 {
        0
    } else {
        total / count as u128
    }
}

fn millis_to_secs(ms: u128) -> f64 {
    ms as f64 / 1_000.0
}

fn ratio_bps(numerator: i128, denominator: u128) -> i64 {
    if denominator == 0 {
        return 0;
    }
    let scaled = numerator.saturating_mul(10_000);
    let ratio = scaled / denominator as i128;
    i128_to_i64_saturating(ratio)
}

fn u128_to_u64_saturating(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn format_candidate_route(candidate: &CandidatePath) -> String {
    let mut parts = Vec::with_capacity(candidate.path.len() + 1);
    parts.push(candidate.start_symbol.clone());
    for hop in &candidate.path {
        parts.push(format!("{}", hop.to));
    }
    parts.join(" -> ")
}

fn format_candidate_dex_route(candidate: &CandidatePath) -> String {
    candidate
        .path
        .iter()
        .map(|hop| format!("{}:{:?}", hop.dex_name, hop.amm_kind))
        .collect::<Vec<_>>()
        .join(" -> ")
}

fn validate_runtime_prerequisites(settings: &Settings, simulate_only: bool) -> Result<()> {
    if settings.operator_private_key.is_none() {
        anyhow::bail!("OPERATOR_PRIVATE_KEY is required");
    }
    if settings.contracts.executor_address.is_none() {
        anyhow::bail!("chain executor address env is required");
    }
    if settings.tokens.is_empty() {
        anyhow::bail!("at least one configured token address is required");
    }
    if !settings.dexes.iter().any(|dex| dex.enabled) {
        anyhow::bail!("at least one enabled DEX is required");
    }
    if !simulate_only
        && !settings.execution.allow_public_fallback
        && settings.rpc.protected_rpc_url.is_none()
    {
        anyhow::bail!(
            "live trading with ALLOW_PUBLIC_FALLBACK=false requires a protected/private RPC URL"
        );
    }
    if settings.risk.stable_depeg_cutoff_e6 > 1_000_000 {
        anyhow::bail!("STABLE_DEPEG_CUTOFF_E6 cannot exceed 1000000");
    }
    Ok(())
}

fn initial_edge_refs(snapshot: &GraphSnapshot, distance_cache: &DistanceCache) -> Vec<EdgeRef> {
    let mut by_pair = std::collections::HashMap::<(usize, usize), InitialEdgeCandidate>::new();

    for (from, edges) in snapshot.adjacency.iter().enumerate() {
        if !distance_cache
            .reachable_from_anchor
            .get(from)
            .copied()
            .unwrap_or(false)
        {
            continue;
        }

        for (edge_idx, edge) in edges.iter().enumerate() {
            if !distance_cache
                .reachable_to_anchor
                .get(edge.to)
                .copied()
                .unwrap_or(false)
            {
                continue;
            }
            if edge.spot_rate_q128.is_zero() || edge.liquidity.safe_capacity_in == 0 {
                continue;
            }

            let edge_ref = EdgeRef { from, edge_idx };
            let candidate = InitialEdgeCandidate::from_edge(snapshot, edge_ref, edge);
            by_pair
                .entry((edge.from, edge.to))
                .and_modify(|current| {
                    if compare_initial_edge_candidate(&candidate, current).is_lt() {
                        *current = candidate.clone();
                    }
                })
                .or_insert(candidate);
        }
    }

    let mut refs = by_pair.into_values().collect::<Vec<_>>();
    refs.sort_by(compare_initial_edge_candidate);
    if let Some(limit) = initial_refresh_max_edges() {
        refs.truncate(limit);
    }
    refs.into_iter()
        .map(|candidate| candidate.edge_ref)
        .collect()
}

fn total_edge_count(snapshot: &GraphSnapshot) -> usize {
    snapshot.adjacency.iter().map(Vec::len).sum()
}

fn initial_refresh_max_edges() -> Option<usize> {
    match std::env::var("INITIAL_REFRESH_MAX_EDGES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
    {
        Some(0) => None,
        Some(value) => Some(value),
        None => None,
    }
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
}

#[derive(Debug, Clone)]
struct InitialEdgeCandidate {
    edge_ref: EdgeRef,
    priority: u8,
    capacity_usd_e8: u128,
    capacity_raw: u128,
    confidence_bps: u16,
    weight_log_q32: i64,
    pool_id: Address,
}

impl InitialEdgeCandidate {
    fn from_edge(snapshot: &GraphSnapshot, edge_ref: EdgeRef, edge: &crate::types::Edge) -> Self {
        let from_token = &snapshot.tokens[edge.from];
        let to_token = &snapshot.tokens[edge.to];
        let priority = if from_token.is_cycle_anchor || to_token.is_cycle_anchor {
            0
        } else if from_token.manual_price_usd_e8.is_some() && to_token.manual_price_usd_e8.is_some()
        {
            1
        } else if from_token.manual_price_usd_e8.is_some() || to_token.manual_price_usd_e8.is_some()
        {
            2
        } else {
            3
        };
        let capacity_usd_e8 =
            amount_to_usd_e8(edge.liquidity.safe_capacity_in, from_token).unwrap_or(0);
        let capacity_usd_e8 = capacity_usd_e8.max(u128::from(edge.liquidity.estimated_usd_e8));

        Self {
            edge_ref,
            priority,
            capacity_usd_e8,
            capacity_raw: edge.liquidity.safe_capacity_in,
            confidence_bps: edge.pool_health.confidence_bps,
            weight_log_q32: edge.weight_log_q32,
            pool_id: edge.pool_id,
        }
    }
}

fn compare_initial_edge_candidate(
    left: &InitialEdgeCandidate,
    right: &InitialEdgeCandidate,
) -> std::cmp::Ordering {
    left.priority
        .cmp(&right.priority)
        .then_with(|| right.capacity_usd_e8.cmp(&left.capacity_usd_e8))
        .then_with(|| right.confidence_bps.cmp(&left.confidence_bps))
        .then_with(|| left.weight_log_q32.cmp(&right.weight_log_q32))
        .then_with(|| right.capacity_raw.cmp(&left.capacity_raw))
        .then_with(|| left.pool_id.cmp(&right.pool_id))
}

fn collect_changed_specs(snapshot: &GraphSnapshot, batch: &RefreshBatch) -> Vec<DiscoveredPool> {
    if batch.triggers.iter().any(|trigger| trigger.full_refresh) {
        let mut seen = HashSet::<Address>::new();
        return batch
            .triggers
            .iter()
            .filter(|trigger| trigger.full_refresh)
            .filter_map(|trigger| trigger.pool_id)
            .filter(|pool_id| seen.insert(*pool_id))
            .filter_map(|pool_id| snapshot.pool(pool_id).map(pool_to_spec))
            .collect::<Vec<_>>();
    }

    let mut seen = HashSet::<Address>::new();
    let mut out = Vec::new();
    for trigger in &batch.triggers {
        if trigger.patch.is_some() {
            continue;
        }
        let Some(pool_id) = trigger.pool_id else {
            continue;
        };
        if !seen.insert(pool_id) {
            continue;
        }
        if let Some(pool) = snapshot.pool(pool_id) {
            out.push(pool_to_spec(pool));
        }
    }
    out
}

fn collect_patches(batch: &RefreshBatch) -> Vec<crate::types::PoolStatePatch> {
    batch
        .triggers
        .iter()
        .filter(|trigger| !trigger.full_refresh)
        .filter_map(|trigger| trigger.patch.clone())
        .collect()
}

fn pool_to_spec(pool: &crate::types::PoolState) -> DiscoveredPool {
    DiscoveredPool {
        address: pool.pool_id,
        dex_name: pool.dex_name.clone(),
        amm_kind: pool.kind,
        factory: pool.factory,
        registry: pool.registry,
        vault: pool.vault,
        quoter: pool.quoter,
        balancer_pool_id: match &pool.state {
            PoolSpecificState::BalancerWeighted(state) => Some(state.pool_id),
            _ => None,
        },
        priority: false,
    }
}

async fn current_block_ref(rpc: &RpcClients) -> Result<BlockRef> {
    let (number, hash, parent_hash) = rpc.best_read().get_block_by_number("latest").await?;
    Ok(BlockRef {
        number,
        hash,
        parent_hash,
        finality: FinalityLevel::Sealed,
    })
}

fn handle_reorg_event(
    settings: &Settings,
    store: &GraphStore,
    event: &ReorgEvent,
    poll_from_block: &mut u64,
) {
    match event {
        ReorgEvent::Detected {
            common_ancestor_block,
            depth,
            ..
        } => {
            telemetry::record_reorg_detected(
                *depth,
                *common_ancestor_block,
                *common_ancestor_block,
                settings.chain.as_str(),
            );
            warn!(
                common_ancestor_block,
                depth,
                "chain reorg detected; rolling graph snapshot back to the common ancestor window"
            );
            if let Some(snapshot) = store.rollback_to_block(*common_ancestor_block) {
                let rollback_block = snapshot
                    .block_ref
                    .as_ref()
                    .map(|block| block.number)
                    .unwrap_or(*common_ancestor_block);
                *poll_from_block =
                    rollback_block.saturating_sub(settings.risk.event_backfill_blocks);
            } else {
                *poll_from_block =
                    common_ancestor_block.saturating_sub(settings.risk.event_backfill_blocks);
            }
        }
        ReorgEvent::Rollback {
            from_block,
            to_block,
        }
        | ReorgEvent::Replay {
            from_block,
            to_block,
        } => {
            telemetry::record_reorg_rollback(*from_block, *to_block, settings.chain.as_str());
            if let Some(snapshot) = store.rollback_to_block(*to_block) {
                let rollback_block = snapshot
                    .block_ref
                    .as_ref()
                    .map(|block| block.number)
                    .unwrap_or(*to_block);
                *poll_from_block =
                    rollback_block.saturating_sub(settings.risk.event_backfill_blocks);
            } else {
                *poll_from_block = to_block.saturating_sub(settings.risk.event_backfill_blocks);
            }
        }
    }
}

fn i128_to_i64_saturating(value: i128) -> i64 {
    value.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}
