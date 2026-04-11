use std::{
    collections::HashSet,
    sync::Arc,
    time::Duration,
};

use alloy::primitives::Address;
use anyhow::{Context, Result};
use tokio::time::sleep;
use tracing::{debug, info, warn};

use crate::{
    config::Settings,
    detector::Detector,
    discovery::{factory_scanner::DiscoveredPool, DiscoveryManager},
    execution::{nonce_manager::NonceManager, submitter::Submitter, validator::Validator},
    graph::{DistanceCache, GraphSnapshot, GraphStore},
    risk::{depeg_guard::DepegGuard, limits::RiskManager},
    router::Router,
    rpc::RpcClients,
    types::{
        BlockRef, CandidatePath, EdgeRef, FinalityLevel, PoolSpecificState, RefreshBatch,
        SubmissionResult,
    },
};

pub async fn run_chain(settings: Arc<Settings>, once: bool, simulate_only_flag: bool) -> Result<()> {
    let simulate_only = simulate_only_flag || settings.simulation_only;
    validate_runtime_prerequisites(&settings)?;

    std::fs::create_dir_all("state").ok();

    let rpc = Arc::new(RpcClients::from_settings(&settings.rpc)?);
    let discovery = DiscoveryManager::new(settings.clone(), rpc.clone());
    let detector = Detector::new(settings.clone());
    let router = Router::new(settings.clone(), rpc.clone());
    let validator = Validator::new(settings.clone(), rpc.clone());
    let submitter = Submitter::new(settings.clone());
    let risk = RiskManager::new(&settings);
    let depeg_guard = DepegGuard::new(&settings);
    let nonce_manager = NonceManager::new();

    let operator = validator
        .operator_address()
        .context("failed to derive operator address from OPERATOR_PRIVATE_KEY")?;
    nonce_manager
        .sync(&rpc.best_read(), operator)
        .await
        .context("failed to sync operator nonce")?;

    info!(chain = %settings.chain, simulate_only, "bootstrapping discovery");
    let bootstrap = discovery.bootstrap().await.context("bootstrap discovery failed")?;
    info!(
        chain = %settings.chain,
        token_count = bootstrap.tokens.len(),
        pool_count = bootstrap.pools.len(),
        snapshot_id = bootstrap.snapshot.snapshot_id,
        "bootstrap complete"
    );

    let store = GraphStore::new(bootstrap.snapshot, 8);
    let initial_snapshot = store.load();
    let initial_changed_edges = all_edge_refs(initial_snapshot.as_ref());
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
        .map(|block| block.number.saturating_sub(settings.risk.event_backfill_blocks))
        .unwrap_or(0);

    loop {
        let snapshot = store.load();
        let poll_result = event_stream.poll_once(snapshot.as_ref(), poll_from_block).await;
        let (latest_block, batch) = match poll_result {
            Ok(result) => result,
            Err(err) => {
                warn!(error = %err, from_block = poll_from_block, "event polling failed");
                sleep(Duration::from_millis(settings.risk.poll_interval_ms)).await;
                continue;
            }
        };
        poll_from_block = latest_block;

        if batch.triggers.is_empty() {
            sleep(Duration::from_millis(settings.risk.poll_interval_ms)).await;
            continue;
        }

        let changed_specs = collect_changed_specs(snapshot.as_ref(), &batch);
        if changed_specs.is_empty() {
            debug!(trigger_count = batch.triggers.len(), "no matching pool specs for refresh batch");
            sleep(Duration::from_millis(settings.risk.poll_interval_ms)).await;
            continue;
        }

        info!(
            trigger_count = batch.triggers.len(),
            refresh_pool_count = changed_specs.len(),
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

    if !depeg_guard.stable_routes_allowed() {
        warn!("stable depeg guard disabled route processing");
        return Ok(());
    }

    let distance_cache = DistanceCache::recompute(snapshot.as_ref());
    let mut candidates = detector.detect(snapshot.as_ref(), &changed_edges, &distance_cache);
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

    for candidate in candidates.into_iter().take(32) {
        if let Some(result) = process_candidate(
            settings.clone(),
            snapshot.as_ref(),
            candidate,
            router,
            validator,
            submitter,
            risk,
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
    nonce_manager: &NonceManager,
    simulate_only: bool,
) -> Result<Option<SubmissionResult>> {
    let Some(plan) = router.search_best_plan(snapshot, &candidate).await? else {
        return Ok(None);
    };

    let Some(mut executable) = validator.prepare(plan).await? else {
        return Ok(None);
    };

    if let Err(err) = risk.pre_trade_check(&executable) {
        debug!(error = %err, path_len = candidate.path.len(), "risk pre-trade check rejected plan");
        return Ok(None);
    }

    if simulate_only {
        info!(
            chain = %settings.chain,
            snapshot_id = executable.exact.snapshot_id,
            token = %candidate.start_symbol,
            path_len = candidate.path.len(),
            input_amount = executable.exact.input_amount,
            output_amount = executable.exact.output_amount,
            expected_profit = executable.exact.expected_profit,
            gas_limit = executable.exact.gas_limit,
            capital_source = ?executable.exact.capital_source,
            "validated opportunity (simulation only)"
        );
        return Ok(None);
    }

    executable.nonce = nonce_manager.reserve();
    risk.mark_submitted();

    match submitter.submit(&executable).await {
        Ok(result) => {
            nonce_manager.mark_submitted(executable.nonce);
            risk.on_submission_result(&result);
            // Receipt tracking is intentionally left external; release the local slot immediately.
            risk.mark_finalized(0);
            Ok(Some(result))
        }
        Err(err) => {
            nonce_manager.mark_dropped(executable.nonce);
            risk.mark_failed_submission();
            warn!(error = %err, nonce = executable.nonce, "submission failed");
            Ok(None)
        }
    }
}

fn validate_runtime_prerequisites(settings: &Settings) -> Result<()> {
    if settings.operator_private_key.is_none() {
        anyhow::bail!("OPERATOR_PRIVATE_KEY is required");
    }
    if settings.contracts.executor_address.is_none() {
        anyhow::bail!("chain executor address env is required");
    }
    Ok(())
}

fn all_edge_refs(snapshot: &GraphSnapshot) -> Vec<EdgeRef> {
    snapshot
        .adjacency
        .iter()
        .enumerate()
        .flat_map(|(from, edges)| {
            (0..edges.len()).map(move |edge_idx| EdgeRef { from, edge_idx })
        })
        .collect()
}

fn collect_changed_specs(snapshot: &GraphSnapshot, batch: &RefreshBatch) -> Vec<DiscoveredPool> {
    if batch.triggers.iter().any(|trigger| trigger.full_refresh) {
        return snapshot
            .pools
            .values()
            .map(pool_to_spec)
            .collect::<Vec<_>>();
    }

    let mut seen = HashSet::<Address>::new();
    let mut out = Vec::new();
    for trigger in &batch.triggers {
        let Some(pool_id) = trigger.pool_id else { continue };
        if !seen.insert(pool_id) {
            continue;
        }
        if let Some(pool) = snapshot.pool(pool_id) {
            out.push(pool_to_spec(pool));
        }
    }
    out
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
