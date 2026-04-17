use std::{collections::HashSet, sync::Arc, time::Duration};

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
    risk::{depeg_guard::DepegGuard, limits::RiskManager},
    router::Router,
    rpc::RpcClients,
    types::{
        BlockRef, CandidatePath, EdgeRef, FinalityLevel, PoolSpecificState, RefreshBatch,
        SubmissionResult,
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
    let initial_changed_edges = initial_edge_refs(initial_snapshot.as_ref());
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
    let mut candidates = detector.detect(snapshot.as_ref(), &changed_edges, &distance_cache);
    for _ in &candidates {
        telemetry::record_candidate_detected(snapshot.snapshot_id, settings.chain.as_str());
    }
    if candidates.is_empty() {
        debug!(snapshot_id = snapshot.snapshot_id, "no candidates detected");
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

    for candidate in candidates {
        if let Some(result) = process_candidate(
            settings.clone(),
            snapshot.as_ref(),
            candidate,
            router,
            validator,
            submitter,
            risk,
            depeg_guard,
            nonce_manager,
            simulate_only,
        )
        .await?
        {
            info!(tx_hash = %result.tx_hash, channel = %result.channel, "submission accepted");
        }
    }

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
) -> Result<Option<SubmissionResult>> {
    let Some(plan) = router.search_best_plan(snapshot, &candidate).await? else {
        return Ok(None);
    };

    let Some(mut executable) = validator.prepare(plan, snapshot).await? else {
        telemetry::record_candidate_rejected("validation", settings.chain.as_str());
        return Ok(None);
    };
    telemetry::record_candidate_validated(executable.exact.snapshot_id, settings.chain.as_str());
    telemetry::record_opportunity_found(
        executable.exact.snapshot_id,
        i128_to_i64_saturating(executable.exact.net_profit_usd_e8),
        settings.chain.as_str(),
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
                return Ok(None);
            }
        }
    }

    if let Err(err) = risk.pre_trade_check(&executable) {
        debug!(error = %err, path_len = candidate.path.len(), "risk pre-trade check rejected plan");
        telemetry::record_candidate_rejected("risk", settings.chain.as_str());
        return Ok(None);
    }

    if simulate_only {
        info!(
            chain = %settings.chain,
            snapshot_id = executable.exact.snapshot_id,
            anchor_symbol = %candidate.start_symbol,
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
            net_profit_usd_e8 = executable.exact.net_profit_usd_e8,
            contract_min_profit_raw = executable.exact.contract_min_profit_raw,
            capital_source = ?executable.exact.capital_source,
            "validated opportunity (simulation only)"
        );
        return Ok(None);
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
                    Ok(Some(result))
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
                    Ok(None)
                }
                Ok(ReceiptOutcome::TimedOut) => {
                    risk.mark_failed_submission();
                    telemetry::record_transaction_failed(
                        &result.tx_hash.to_string(),
                        settings.chain.as_str(),
                        "receipt_timeout",
                    );
                    warn!(tx_hash = %result.tx_hash, nonce = executable.nonce, "submitted transaction receipt wait timed out");
                    Ok(None)
                }
                Err(err) => {
                    risk.mark_failed_submission();
                    telemetry::record_transaction_failed(
                        &result.tx_hash.to_string(),
                        settings.chain.as_str(),
                        "receipt_error",
                    );
                    warn!(error = %err, tx_hash = %result.tx_hash, nonce = executable.nonce, "failed while waiting for transaction receipt");
                    Ok(None)
                }
            }
        }
        Err(err) => {
            nonce_manager.mark_dropped(executable.nonce);
            risk.mark_failed_submission();
            telemetry::record_candidate_rejected("submission", settings.chain.as_str());
            warn!(error = %err, nonce = executable.nonce, "submission failed");
            Ok(None)
        }
    }
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

fn all_edge_refs(snapshot: &GraphSnapshot) -> Vec<EdgeRef> {
    snapshot
        .adjacency
        .iter()
        .enumerate()
        .flat_map(|(from, edges)| (0..edges.len()).map(move |edge_idx| EdgeRef { from, edge_idx }))
        .collect()
}

fn initial_edge_refs(snapshot: &GraphSnapshot) -> Vec<EdgeRef> {
    let mut refs = all_edge_refs(snapshot);
    refs.sort_by(|left, right| {
        let left_edge = snapshot.edge(*left);
        let right_edge = snapshot.edge(*right);
        match (left_edge, right_edge) {
            (Some(left_edge), Some(right_edge)) => left_edge
                .weight_log_q32
                .cmp(&right_edge.weight_log_q32)
                .then_with(|| {
                    right_edge
                        .pool_health
                        .confidence_bps
                        .cmp(&left_edge.pool_health.confidence_bps)
                })
                .then_with(|| {
                    right_edge
                        .liquidity
                        .safe_capacity_in
                        .cmp(&left_edge.liquidity.safe_capacity_in)
                }),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
    });
    if let Some(limit) = initial_refresh_max_edges() {
        refs.truncate(limit);
    }
    refs
}

fn total_edge_count(snapshot: &GraphSnapshot) -> usize {
    snapshot.adjacency.iter().map(Vec::len).sum()
}

fn initial_refresh_max_edges() -> Option<usize> {
    std::env::var("INITIAL_REFRESH_MAX_EDGES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .and_then(|value| (value > 0).then_some(value))
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
