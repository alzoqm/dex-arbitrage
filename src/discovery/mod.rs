pub mod admission;
pub mod event_stream;
pub mod factory_scanner;
pub mod multicall;
pub mod pool_fetcher;

use std::{
    collections::{HashMap, HashSet},
    fs,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use alloy::{
    primitives::{Address, Bytes, U256},
    sol_types::SolCall,
};
use anyhow::{Context, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::{
    abi::{IAavePool, IERC20},
    cache::{load_cache, load_legacy_cache, repair_cache, AtomicCacheWriter, CacheLoadResult},
    config::Settings,
    graph::GraphSnapshot,
    monitoring::metrics as telemetry,
    rpc::RpcClients,
    types::{
        AmmKind, BlockRef, PoolHealth, PoolSpecificState, PoolState, PoolStatePatch, TokenBehavior,
        TokenInfo,
    },
};

use self::{
    admission::AdmissionEngine,
    event_stream::EventStream,
    factory_scanner::{DiscoveredPool, FactoryScanner},
    pool_fetcher::{PoolFetcher, SkippedPoolFetch},
};

#[derive(Debug, Clone)]
pub struct DiscoveryOutput {
    pub tokens: Vec<TokenInfo>,
    pub pools: HashMap<Address, PoolState>,
    pub snapshot: GraphSnapshot,
}

#[derive(Debug)]
pub struct DiscoveryManager {
    settings: Arc<Settings>,
    rpc: Arc<RpcClients>,
    scanner: FactoryScanner,
    fetcher: PoolFetcher,
    admission: AdmissionEngine,
    aave_cache: Mutex<Option<AaveReserveCache>>,
    token_metadata_cache: Mutex<HashMap<Address, (String, u8)>>,
}

#[derive(Debug, Clone)]
struct AaveReserveCache {
    fetched_at: Instant,
    reserves: HashMap<Address, ()>,
}

impl DiscoveryManager {
    pub fn new(settings: Arc<Settings>, rpc: Arc<RpcClients>) -> Self {
        let token_metadata_cache = TokenMetadataCache::load(&settings)
            .ok()
            .map(|cache| cache.into_map())
            .unwrap_or_default();
        Self {
            scanner: FactoryScanner::new(settings.clone(), rpc.clone()),
            fetcher: PoolFetcher::new(settings.clone(), rpc.clone()),
            admission: AdmissionEngine::new(settings.clone()),
            settings,
            rpc,
            aave_cache: Mutex::new(None),
            token_metadata_cache: Mutex::new(token_metadata_cache),
        }
    }

    pub async fn bootstrap(&self) -> Result<DiscoveryOutput> {
        let flash_reserves = self.aave_flash_reserves().await?;
        let discovered = self.scanner.scan_all().await?;
        let latest_at_bootstrap = self.scanner.latest_block().await.unwrap_or(0);
        let fetch_block_number = (latest_at_bootstrap > 0).then_some(latest_at_bootstrap);
        let mut pools = HashMap::new();
        let (mut cached_pools, mut skipped_pool_cache, pool_state_cache_loaded, cache_anchor_block) =
            if pool_state_cache_disabled() {
                (HashMap::new(), HashMap::new(), false, None)
            } else {
                match PoolStateCache::load(&self.settings) {
                    Ok(cache) => {
                        let cache_anchor_block = cache.anchor_block();
                        info!(
                            pools = cache.pools.len(),
                            skipped_pools = cache.skipped_pools.len(),
                            cache_anchor_block,
                            "pool state cache loaded"
                        );
                        let pools = cache
                            .pools
                            .into_iter()
                            .map(|pool| (pool.pool_id, pool))
                            .collect::<HashMap<_, _>>();
                        let skipped = cache
                            .skipped_pools
                            .into_iter()
                            .filter(skipped_pool_cache_entry_active)
                            .map(|entry| (entry.spec.address, entry))
                            .collect::<HashMap<_, _>>();
                        (pools, skipped, true, cache_anchor_block)
                    }
                    Err(err) => {
                        warn!(
                            error = %err,
                            "pool state cache load failed; falling back to full fetch"
                        );
                        (HashMap::new(), HashMap::new(), false, None)
                    }
                }
            };
        let fetch_unseen_pools = fetch_unseen_pools_on_bootstrap() || !pool_state_cache_loaded;
        let save_intermediate_checkpoints = fetch_unseen_pools || !pool_state_cache_loaded;
        let replay_cached_pools = bootstrap_replay_cached_pool_states()
            && pool_state_cache_loaded
            && cache_anchor_block.is_some();
        let cached_pool_count = cached_pools.len();
        let active_skipped_pool_count = skipped_pool_cache.len();

        let mut v2_to_fetch = Vec::new();
        let mut aerodrome_v2_to_fetch = Vec::new();
        let mut v3_to_fetch = Vec::new();
        let mut other_to_fetch = Vec::new();
        let mut skipped_cache_hits = 0usize;
        let mut stale_cached_to_refresh = 0usize;
        let mut stale_cached_to_replay = 0usize;
        let mut unseen_pool_skips = 0usize;
        for spec in discovered {
            let cached = cached_pools
                .remove(&spec.address)
                .filter(|pool| pool_matches_spec(pool, &spec));
            if let Some(pool) = cached {
                if self.admission.admit(&pool) {
                    pools.insert(pool.pool_id, pool);
                } else if replay_cached_pools
                    && !pool.health.stale
                    && self.admission.should_refresh_cached(&pool)
                {
                    stale_cached_to_replay += 1;
                    pools.insert(pool.pool_id, pool);
                } else if self.admission.should_refresh_cached(&pool) {
                    stale_cached_to_refresh += 1;
                    push_fetch_spec(
                        &mut v2_to_fetch,
                        &mut aerodrome_v2_to_fetch,
                        &mut v3_to_fetch,
                        &mut other_to_fetch,
                        spec,
                    );
                }
                continue;
            }

            if let Some(entry) = skipped_pool_cache.get(&spec.address) {
                if skipped_pool_matches_spec(entry, &spec)
                    && skipped_pool_cache_applies_to_spec(entry, &spec)
                {
                    skipped_cache_hits += 1;
                    continue;
                }
                skipped_pool_cache.remove(&spec.address);
            }

            if !fetch_unseen_pools && is_high_cardinality_pool_kind(spec.amm_kind) && !spec.priority
            {
                unseen_pool_skips += 1;
                continue;
            }

            push_fetch_spec(
                &mut v2_to_fetch,
                &mut aerodrome_v2_to_fetch,
                &mut v3_to_fetch,
                &mut other_to_fetch,
                spec,
            );
        }

        if pool_state_cache_loaded && !fetch_unseen_pools && unseen_pool_skips > 0 {
            info!(
                unseen_pool_skips,
                cached_pool_count,
                active_skipped_pool_count,
                "skipping unseen discovered pools during cached bootstrap"
            );
        }

        info!(
            cached = pools.len(),
            stale_cached_to_refresh,
            stale_cached_to_replay,
            cache_anchor_block,
            v2_to_fetch = v2_to_fetch.len(),
            aerodrome_v2_to_fetch = aerodrome_v2_to_fetch.len(),
            v3_to_fetch = v3_to_fetch.len(),
            other_to_fetch = other_to_fetch.len(),
            skipped_cache_hits,
            unseen_pool_skips,
            fetch_unseen_pools,
            "pool state fetch plan built"
        );

        let v2_batch = self
            .fetcher
            .fetch_v2_batch(&v2_to_fetch, fetch_block_number)
            .await?;
        for skipped in v2_batch.skipped {
            insert_skipped_pool(&mut skipped_pool_cache, skipped);
        }
        for pool in v2_batch.pools {
            if self.admission.admit(&pool) {
                skipped_pool_cache.remove(&pool.pool_id);
                pools.insert(pool.pool_id, pool);
            }
        }
        if !v2_to_fetch.is_empty() && save_intermediate_checkpoints {
            self.save_pool_state_checkpoint(&pools, &skipped_pool_cache, "v2");
        }

        let aerodrome_v2_batch = self
            .fetcher
            .fetch_aerodrome_v2_batch(&aerodrome_v2_to_fetch, fetch_block_number)
            .await?;
        for skipped in aerodrome_v2_batch.skipped {
            insert_skipped_pool(&mut skipped_pool_cache, skipped);
        }
        for pool in aerodrome_v2_batch.pools {
            if self.admission.admit(&pool) {
                skipped_pool_cache.remove(&pool.pool_id);
                pools.insert(pool.pool_id, pool);
            }
        }
        if !aerodrome_v2_to_fetch.is_empty() && save_intermediate_checkpoints {
            self.save_pool_state_checkpoint(&pools, &skipped_pool_cache, "aerodrome_v2");
        }

        let v3_batch = self
            .fetcher
            .fetch_v3_batch(&v3_to_fetch, fetch_block_number)
            .await?;
        for skipped in v3_batch.skipped {
            insert_skipped_pool(&mut skipped_pool_cache, skipped);
        }
        for pool in v3_batch.pools {
            if self.admission.admit(&pool) {
                skipped_pool_cache.remove(&pool.pool_id);
                pools.insert(pool.pool_id, pool);
            }
        }
        if !v3_to_fetch.is_empty() && save_intermediate_checkpoints {
            self.save_pool_state_checkpoint(&pools, &skipped_pool_cache, "v3");
        }

        let mut skipped_other = 0usize;
        for (idx, spec) in other_to_fetch.into_iter().enumerate() {
            match self
                .fetcher
                .fetch_pool_with_retries(&spec, fetch_block_number)
                .await
            {
                Ok(Some(pool)) => {
                    if self.admission.admit(&pool) {
                        skipped_pool_cache.remove(&pool.pool_id);
                        pools.insert(pool.pool_id, pool);
                    }
                }
                Ok(None) => {
                    skipped_other += 1;
                    insert_skipped_pool_reason(
                        &mut skipped_pool_cache,
                        spec,
                        "non_v2_v3_fetch_returned_none",
                    );
                }
                Err(err) => {
                    skipped_other += 1;
                    warn!(
                        pool = %spec.address,
                        dex = %spec.dex_name,
                        kind = ?spec.amm_kind,
                        error = %err,
                        "skipping non-v2/v3 pool after retries"
                    );
                    insert_skipped_pool_reason(
                        &mut skipped_pool_cache,
                        spec,
                        "non_v2_v3_fetch_failed",
                    );
                }
            }
            if (idx + 1) % 500 == 0 {
                info!(
                    fetched = idx + 1,
                    admitted = pools.len(),
                    skipped = skipped_other,
                    "non-v2 pool state fetch progress"
                );
                self.save_pool_state_checkpoint(&pools, &skipped_pool_cache, "non_v2_v3");
            }
        }
        if skipped_other > 0 || !pools.is_empty() {
            self.save_pool_state_checkpoint(&pools, &skipped_pool_cache, "post_fetch");
        }

        if replay_cached_pools {
            if let Some(anchor_block) = cache_anchor_block {
                self.replay_cached_pool_state_events(
                    &mut pools,
                    &mut skipped_pool_cache,
                    anchor_block,
                    latest_at_bootstrap,
                )
                .await?;
            }
        }

        let mut tokens = self.discover_tokens(&pools, &flash_reserves).await?;
        derive_missing_prices_from_pools(&mut tokens, &pools);

        let block_ref = if latest_at_bootstrap > 0 {
            self.scanner.current_block_ref().await.ok()
        } else {
            None
        };

        let snapshot = GraphSnapshot::build(0, block_ref, tokens.clone(), pools.clone());
        PoolStateCache::from_parts(
            &self.settings,
            pools.values().cloned().collect(),
            skipped_pool_cache.values().cloned().collect(),
        )
        .save(&self.settings)
        .ok();
        Ok(DiscoveryOutput {
            tokens,
            pools,
            snapshot,
        })
    }

    fn save_pool_state_checkpoint(
        &self,
        pools: &HashMap<Address, PoolState>,
        skipped_pool_cache: &HashMap<Address, SkippedPoolCacheEntry>,
        phase: &str,
    ) {
        let cache = PoolStateCache::from_parts(
            &self.settings,
            pools.values().cloned().collect(),
            skipped_pool_cache.values().cloned().collect(),
        );
        match cache.save(&self.settings) {
            Ok(()) => info!(
                phase,
                pools = pools.len(),
                skipped_pools = skipped_pool_cache.len(),
                "pool state checkpoint saved"
            ),
            Err(err) => warn!(
                phase,
                pools = pools.len(),
                error = %err,
                "pool state checkpoint save failed"
            ),
        }
    }

    async fn replay_cached_pool_state_events(
        &self,
        pools: &mut HashMap<Address, PoolState>,
        skipped_pool_cache: &mut HashMap<Address, SkippedPoolCacheEntry>,
        anchor_block: u64,
        latest_block: u64,
    ) -> Result<()> {
        if latest_block == 0 || latest_block <= anchor_block {
            mark_replayed_pools_current(pools, latest_block.max(anchor_block));
            return Ok(());
        }

        let replay_from = anchor_block.saturating_sub(self.settings.risk.event_backfill_blocks);
        let temp_snapshot = GraphSnapshot::build(0, None, Vec::new(), pools.clone());
        let event_stream = EventStream::new(self.settings.clone(), self.rpc.clone());
        let batch = event_stream
            .backfill_once(&temp_snapshot, replay_from, latest_block)
            .await
            .context("cached pool event replay failed")?;

        let patches = collect_replay_patches(&batch);
        let full_refresh_specs = collect_replay_full_refresh_specs(pools, &batch);
        let mut failed_full_refresh = HashSet::new();

        for patch in patches {
            if let Some(pool) = pools.get_mut(&patch.pool_id()) {
                apply_pool_patch(pool, patch, Some(latest_block));
            }
        }

        if !full_refresh_specs.is_empty() {
            info!(
                from_block = replay_from,
                to_block = latest_block,
                full_refresh_count = full_refresh_specs.len(),
                "refreshing pools touched by non-patchable replay logs"
            );
            self.fetch_replay_full_refresh_pools(
                pools,
                skipped_pool_cache,
                full_refresh_specs,
                latest_block,
                &mut failed_full_refresh,
            )
            .await?;
        }

        mark_replayed_pools_current(pools, latest_block);
        for pool_id in failed_full_refresh {
            if let Some(pool) = pools.get_mut(&pool_id) {
                pool.health.stale = true;
            }
        }
        self.save_pool_state_checkpoint(pools, skipped_pool_cache, "event_replay");
        Ok(())
    }

    async fn fetch_replay_full_refresh_pools(
        &self,
        pools: &mut HashMap<Address, PoolState>,
        skipped_pool_cache: &mut HashMap<Address, SkippedPoolCacheEntry>,
        specs: Vec<DiscoveredPool>,
        latest_block: u64,
        failed_full_refresh: &mut HashSet<Address>,
    ) -> Result<()> {
        let mut v2_to_fetch = Vec::new();
        let mut aerodrome_v2_to_fetch = Vec::new();
        let mut v3_to_fetch = Vec::new();
        let mut other_to_fetch = Vec::new();
        for spec in specs {
            push_fetch_spec(
                &mut v2_to_fetch,
                &mut aerodrome_v2_to_fetch,
                &mut v3_to_fetch,
                &mut other_to_fetch,
                spec,
            );
        }

        let block_number = Some(latest_block);
        let v2_batch = self
            .fetcher
            .fetch_v2_batch(&v2_to_fetch, block_number)
            .await?;
        for skipped in v2_batch.skipped {
            failed_full_refresh.insert(skipped.spec.address);
            insert_skipped_pool(skipped_pool_cache, skipped);
        }
        for pool in v2_batch.pools {
            skipped_pool_cache.remove(&pool.pool_id);
            failed_full_refresh.remove(&pool.pool_id);
            pools.insert(pool.pool_id, pool);
        }

        let aerodrome_v2_batch = self
            .fetcher
            .fetch_aerodrome_v2_batch(&aerodrome_v2_to_fetch, block_number)
            .await?;
        for skipped in aerodrome_v2_batch.skipped {
            failed_full_refresh.insert(skipped.spec.address);
            insert_skipped_pool(skipped_pool_cache, skipped);
        }
        for pool in aerodrome_v2_batch.pools {
            skipped_pool_cache.remove(&pool.pool_id);
            failed_full_refresh.remove(&pool.pool_id);
            pools.insert(pool.pool_id, pool);
        }

        let v3_batch = self
            .fetcher
            .fetch_v3_batch(&v3_to_fetch, block_number)
            .await?;
        for skipped in v3_batch.skipped {
            failed_full_refresh.insert(skipped.spec.address);
            insert_skipped_pool(skipped_pool_cache, skipped);
        }
        for pool in v3_batch.pools {
            skipped_pool_cache.remove(&pool.pool_id);
            failed_full_refresh.remove(&pool.pool_id);
            pools.insert(pool.pool_id, pool);
        }

        for spec in other_to_fetch {
            match self
                .fetcher
                .fetch_pool_with_retries(&spec, block_number)
                .await
            {
                Ok(Some(pool)) if self.admission.admit(&pool) => {
                    skipped_pool_cache.remove(&pool.pool_id);
                    failed_full_refresh.remove(&pool.pool_id);
                    pools.insert(pool.pool_id, pool);
                }
                Ok(_) => {
                    failed_full_refresh.insert(spec.address);
                    insert_skipped_pool_reason(
                        skipped_pool_cache,
                        spec,
                        "replay_full_refresh_returned_none",
                    );
                }
                Err(err) => {
                    failed_full_refresh.insert(spec.address);
                    warn!(
                        pool = %spec.address,
                        dex = %spec.dex_name,
                        kind = ?spec.amm_kind,
                        error = %err,
                        "skipping replay full refresh pool after retries"
                    );
                    insert_skipped_pool_reason(
                        skipped_pool_cache,
                        spec,
                        "replay_full_refresh_failed",
                    );
                }
            }
        }

        Ok(())
    }

    pub async fn refresh_pools(
        &self,
        current_snapshot_id: u64,
        tokens: Vec<TokenInfo>,
        current_pools: HashMap<Address, PoolState>,
        changed_specs: Vec<DiscoveredPool>,
        patches: Vec<PoolStatePatch>,
        block_ref: Option<BlockRef>,
    ) -> Result<(GraphSnapshot, Vec<Address>)> {
        let mut pools = current_pools;
        let mut changed_pool_ids = Vec::new();
        let mut tokens = tokens;
        let block_number = block_ref.as_ref().map(|block| block.number);

        for patch in patches {
            if let Some(pool) = pools.get_mut(&patch.pool_id()) {
                if apply_pool_patch(pool, patch, block_number) {
                    changed_pool_ids.push(pool.pool_id);
                }
            }
        }

        for spec in changed_specs {
            if let Some(pool) = self.fetcher.fetch_pool(&spec, block_number).await? {
                if self.admission.admit(&pool) {
                    changed_pool_ids.push(pool.pool_id);
                    pools.insert(pool.pool_id, pool);
                }
            }
        }

        let flash_reserves = self.aave_flash_reserves().await?;
        self.extend_tokens_with_discovered(&mut tokens, pools.values(), &flash_reserves)
            .await?;
        apply_aave_flash_reserves(&mut tokens, &flash_reserves);
        derive_missing_prices_from_pools(&mut tokens, &pools);
        let snapshot = GraphSnapshot::build(current_snapshot_id + 1, block_ref, tokens, pools);
        Ok((snapshot, changed_pool_ids))
    }

    pub fn event_stream(&self, rpc: Arc<RpcClients>) -> EventStream {
        EventStream::new(self.settings.clone(), rpc)
    }

    fn configured_tokens(&self) -> Vec<TokenInfo> {
        self.settings
            .tokens
            .iter()
            .map(|token| TokenInfo {
                address: token.address,
                symbol: token.symbol.clone(),
                decimals: token.decimals,
                is_stable: token.is_stable || is_stable_symbol(&token.symbol),
                is_cycle_anchor: token.is_cycle_anchor,
                flash_loan_enabled: token.flash_loan_enabled,
                allow_self_funded: token.allow_self_funded,
                behavior: TokenBehavior::default(),
                manual_price_usd_e8: token
                    .manual_price_usd_e8
                    .or_else(|| stable_price_for_symbol(&token.symbol)),
                max_position_usd_e8: token.max_position_usd_e8,
                max_flash_loan_usd_e8: token.max_flash_loan_usd_e8,
            })
            .collect()
    }

    async fn discover_tokens(
        &self,
        pools: &HashMap<Address, PoolState>,
        flash_reserves: &HashMap<Address, ()>,
    ) -> Result<Vec<TokenInfo>> {
        let mut tokens = self.configured_tokens();
        self.extend_tokens_with_discovered(&mut tokens, pools.values(), flash_reserves)
            .await?;
        apply_aave_flash_reserves(&mut tokens, flash_reserves);
        Ok(tokens)
    }

    async fn extend_tokens_with_discovered<'a, I>(
        &self,
        tokens: &mut Vec<TokenInfo>,
        pools: I,
        flash_reserves: &HashMap<Address, ()>,
    ) -> Result<()>
    where
        I: IntoIterator<Item = &'a PoolState>,
    {
        let mut known = tokens
            .iter()
            .map(|token| token.address)
            .collect::<std::collections::HashSet<_>>();
        let metadata_cache_len = self.token_metadata_cache.lock().len();
        let mut discovered_addresses = Vec::new();

        for pool in pools {
            for address in pool.token_addresses.iter().copied() {
                if !known.insert(address) {
                    continue;
                }
                discovered_addresses.push(address);
            }
        }

        for address in flash_reserves.keys().copied() {
            if !known.insert(address) {
                continue;
            }
            discovered_addresses.push(address);
        }

        let metadata = self.fetch_token_metadata_batch(&discovered_addresses).await;
        for address in discovered_addresses {
            let (symbol, decimals) = metadata
                .get(&address)
                .cloned()
                .unwrap_or_else(|| (address.to_string(), 18));
            if !self.settings.policy.symbol_allowed(&symbol) {
                continue;
            }
            let flash_loan_enabled = flash_reserves.contains_key(&address);
            let is_stable = is_stable_symbol(&symbol);
            tokens.push(TokenInfo {
                address,
                symbol,
                decimals,
                is_stable,
                is_cycle_anchor: flash_loan_enabled,
                flash_loan_enabled,
                allow_self_funded: false,
                behavior: TokenBehavior::default(),
                manual_price_usd_e8: is_stable.then_some(100_000_000),
                max_position_usd_e8: None,
                max_flash_loan_usd_e8: None,
            });
        }

        if self.token_metadata_cache.lock().len() != metadata_cache_len {
            self.save_token_metadata_cache();
        }

        Ok(())
    }

    async fn fetch_token_metadata_batch(
        &self,
        addresses: &[Address],
    ) -> HashMap<Address, (String, u8)> {
        let mut out = HashMap::new();
        if addresses.is_empty() {
            return out;
        }

        let mut missing = Vec::new();
        {
            let cache = self.token_metadata_cache.lock();
            for address in addresses.iter().copied() {
                if let Some(metadata) = cache.get(&address).cloned() {
                    out.insert(address, metadata);
                } else {
                    missing.push(address);
                }
            }
        }

        if missing.is_empty() {
            return out;
        }

        let chunk_size = token_metadata_multicall_chunk_size();
        info!(
            total = missing.len(),
            chunk_size, "starting token metadata batch fetch"
        );

        let mut fetched = 0usize;
        let mut fallback = 0usize;
        for (chunk_index, chunk) in missing.chunks(chunk_size).enumerate() {
            let mut calls = Vec::with_capacity(chunk.len() * 2);
            for address in chunk {
                calls.push((*address, IERC20::symbolCall {}.abi_encode()));
                calls.push((*address, IERC20::decimalsCall {}.abi_encode()));
            }

            match multicall::aggregate3(&self.rpc, calls).await {
                Ok(results) => {
                    let mut cache = self.token_metadata_cache.lock();
                    for (address, raw) in chunk.iter().copied().zip(results.chunks(2)) {
                        let symbol = raw
                            .first()
                            .and_then(|value| value.as_ref())
                            .and_then(decode_token_symbol_return)
                            .unwrap_or_else(|| address.to_string());
                        let decimals = raw
                            .get(1)
                            .and_then(|value| value.as_ref())
                            .and_then(|raw| IERC20::decimalsCall::abi_decode_returns(raw).ok())
                            .unwrap_or(18);
                        let metadata = (symbol, decimals);
                        cache.insert(address, metadata.clone());
                        out.insert(address, metadata);
                        fetched += 1;
                    }
                }
                Err(err) => {
                    warn!(
                        chunk_index,
                        count = chunk.len(),
                        error = %err,
                        "token metadata multicall failed; falling back to per-token fetch"
                    );
                    for address in chunk.iter().copied() {
                        let metadata = self.fetch_token_metadata(address).await;
                        out.insert(address, metadata);
                        fallback += 1;
                    }
                }
            }

            if (chunk_index + 1) % 25 == 0 || fetched + fallback == missing.len() {
                info!(
                    fetched = fetched + fallback,
                    total = missing.len(),
                    multicall = fetched,
                    fallback,
                    "token metadata batch fetch progress"
                );
            }
        }

        info!(
            total = missing.len(),
            multicall = fetched,
            fallback,
            "token metadata batch fetch complete"
        );
        out
    }

    async fn aave_flash_reserves(&self) -> Result<HashMap<Address, ()>> {
        let ttl = Duration::from_secs(
            std::env::var("AAVE_RESERVE_CACHE_SECS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|value| *value > 0)
                .unwrap_or(3_600),
        );
        if let Some(cached) = self.aave_cache.lock().clone() {
            if cached.fetched_at.elapsed() <= ttl {
                return Ok(cached.reserves);
            }
        }

        let Some(aave_pool) = self.settings.contracts.aave_pool else {
            return Ok(HashMap::new());
        };

        let raw = self
            .rpc
            .best_read()
            .eth_call(
                aave_pool,
                None,
                IAavePool::getReservesListCall {}.abi_encode().into(),
                "latest",
            )
            .await?;
        let reserves = IAavePool::getReservesListCall::abi_decode_returns(&raw)?;
        let mut out = HashMap::new();
        let calls = reserves
            .iter()
            .copied()
            .map(|asset| {
                (
                    aave_pool,
                    IAavePool::getConfigurationCall { asset }.abi_encode(),
                )
            })
            .collect::<Vec<_>>();
        match multicall::aggregate3(&self.rpc, calls).await {
            Ok(results) => {
                for (asset, raw) in reserves.into_iter().zip(results.into_iter()) {
                    let Some(raw) = raw else {
                        continue;
                    };
                    let config = match IAavePool::getConfigurationCall::abi_decode_returns(&raw) {
                        Ok(config) => config,
                        Err(err) => {
                            warn!(
                                asset = %asset,
                                error = %err,
                                "skipping undecodable Aave reserve configuration"
                            );
                            continue;
                        }
                    };
                    if reserve_accepts_flash_loan(config.data) {
                        out.insert(asset, ());
                    }
                }
            }
            Err(err) => {
                warn!(
                    error = %err,
                    "Aave reserve multicall failed; falling back to per-reserve calls"
                );
                for asset in reserves {
                    let raw = match self
                        .rpc
                        .best_read()
                        .eth_call(
                            aave_pool,
                            None,
                            IAavePool::getConfigurationCall { asset }
                                .abi_encode()
                                .into(),
                            "latest",
                        )
                        .await
                    {
                        Ok(raw) => raw,
                        Err(err) => {
                            warn!(
                                asset = %asset,
                                error = %err,
                                "skipping Aave reserve after configuration eth_call failure"
                            );
                            continue;
                        }
                    };
                    let config = match IAavePool::getConfigurationCall::abi_decode_returns(&raw) {
                        Ok(config) => config,
                        Err(err) => {
                            warn!(
                                asset = %asset,
                                error = %err,
                                "skipping undecodable Aave reserve configuration"
                            );
                            continue;
                        }
                    };
                    if reserve_accepts_flash_loan(config.data) {
                        out.insert(asset, ());
                    }
                }
            }
        }
        *self.aave_cache.lock() = Some(AaveReserveCache {
            fetched_at: Instant::now(),
            reserves: out.clone(),
        });
        Ok(out)
    }

    async fn fetch_token_metadata(&self, address: Address) -> (String, u8) {
        if let Some(cached) = self.token_metadata_cache.lock().get(&address).cloned() {
            return cached;
        }
        let symbol = self
            .fetch_token_symbol(address)
            .await
            .unwrap_or_else(|| address.to_string());
        let decimals = self.fetch_token_decimals(address).await.unwrap_or(18);
        let metadata = (symbol, decimals);
        self.token_metadata_cache
            .lock()
            .insert(address, metadata.clone());
        metadata
    }

    fn save_token_metadata_cache(&self) {
        TokenMetadataCache::from_map(
            self.settings.chain_id,
            self.token_metadata_cache.lock().clone(),
        )
        .save(&self.settings)
        .ok();
    }

    async fn fetch_token_symbol(&self, address: Address) -> Option<String> {
        let raw = self
            .rpc
            .best_read()
            .eth_call(
                address,
                None,
                IERC20::symbolCall {}.abi_encode().into(),
                "latest",
            )
            .await
            .ok()?;

        decode_token_symbol_return(&raw)
    }

    async fn fetch_token_decimals(&self, address: Address) -> Option<u8> {
        let raw = self
            .rpc
            .best_read()
            .eth_call(
                address,
                None,
                IERC20::decimalsCall {}.abi_encode().into(),
                "latest",
            )
            .await
            .ok()?;
        IERC20::decimalsCall::abi_decode_returns(&raw).ok()
    }
}

fn apply_pool_patch(
    pool: &mut PoolState,
    patch: PoolStatePatch,
    fallback_block_number: Option<u64>,
) -> bool {
    let block_number = match &patch {
        PoolStatePatch::UniswapV2Sync { block_number, .. }
        | PoolStatePatch::UniswapV3Swap { block_number, .. }
        | PoolStatePatch::BalancerSwap { block_number, .. }
        | PoolStatePatch::BalancerBalanceChanged { block_number, .. } => {
            block_number.or(fallback_block_number)
        }
    };

    let applied = match (patch, &mut pool.state) {
        (
            PoolStatePatch::UniswapV2Sync {
                reserve0, reserve1, ..
            },
            PoolSpecificState::UniswapV2Like(state),
        ) => {
            state.reserve0 = reserve0;
            state.reserve1 = reserve1;
            true
        }
        (
            PoolStatePatch::UniswapV2Sync {
                reserve0, reserve1, ..
            },
            PoolSpecificState::AerodromeV2Like(state),
        ) => {
            state.reserve0 = reserve0;
            state.reserve1 = reserve1;
            true
        }
        (
            PoolStatePatch::UniswapV3Swap {
                sqrt_price_x96,
                liquidity,
                tick,
                ..
            },
            PoolSpecificState::UniswapV3Like(state),
        ) => {
            state.sqrt_price_x96 = sqrt_price_x96;
            state.liquidity = liquidity;
            state.tick = tick;
            true
        }
        (
            PoolStatePatch::BalancerSwap {
                token_in,
                token_out,
                amount_in,
                amount_out,
                ..
            },
            PoolSpecificState::BalancerWeighted(state),
        ) => {
            let mut changed = false;
            if let Some(idx) = pool
                .token_addresses
                .iter()
                .position(|token| *token == token_in)
            {
                state.balances[idx] = state.balances[idx].saturating_add(amount_in);
                changed = true;
            }
            if let Some(idx) = pool
                .token_addresses
                .iter()
                .position(|token| *token == token_out)
            {
                state.balances[idx] = state.balances[idx].saturating_sub(amount_out);
                changed = true;
            }
            changed
        }
        (
            PoolStatePatch::BalancerBalanceChanged { tokens, deltas, .. },
            PoolSpecificState::BalancerWeighted(state),
        ) => {
            let mut changed = false;
            for (token, delta) in tokens.into_iter().zip(deltas) {
                if let Some(idx) = pool
                    .token_addresses
                    .iter()
                    .position(|known| *known == token)
                {
                    state.balances[idx] = apply_i128_delta(state.balances[idx], delta);
                    changed = true;
                }
            }
            changed
        }
        _ => false,
    };

    if applied {
        pool.last_updated_block = block_number.unwrap_or(pool.last_updated_block);
        pool.health = patched_health(
            &pool.health,
            block_number.unwrap_or(pool.health.last_successful_refresh_block),
        );
    }
    applied
}

fn apply_i128_delta(value: u128, delta: i128) -> u128 {
    if delta >= 0 {
        value.saturating_add(delta as u128)
    } else {
        value.saturating_sub(delta.unsigned_abs())
    }
}

fn patched_health(previous: &PoolHealth, block_number: u64) -> PoolHealth {
    PoolHealth {
        stale: false,
        paused: previous.paused,
        quarantined: previous.quarantined,
        confidence_bps: previous.confidence_bps,
        last_successful_refresh_block: block_number,
        last_refresh_at: SystemTime::now(),
        recent_revert_count: previous.recent_revert_count,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PoolStateCache {
    version: u32,
    chain_id: u64,
    #[serde(default)]
    saved_at_block: Option<u64>,
    #[serde(default)]
    saved_at_unix_secs: Option<u64>,
    pools: Vec<PoolState>,
    #[serde(default)]
    skipped_pools: Vec<SkippedPoolCacheEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TokenMetadataCache {
    version: u32,
    chain_id: u64,
    entries: Vec<TokenMetadataCacheEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TokenMetadataCacheEntry {
    address: Address,
    symbol: String,
    decimals: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SkippedPoolCacheEntry {
    spec: DiscoveredPool,
    reason: String,
    skipped_at_unix_secs: u64,
}

impl TokenMetadataCache {
    fn from_map(chain_id: u64, map: HashMap<Address, (String, u8)>) -> Self {
        Self {
            version: 1,
            chain_id,
            entries: map
                .into_iter()
                .map(|(address, (symbol, decimals))| TokenMetadataCacheEntry {
                    address,
                    symbol,
                    decimals,
                })
                .collect(),
        }
    }

    fn into_map(self) -> HashMap<Address, (String, u8)> {
        self.entries
            .into_iter()
            .map(|entry| (entry.address, (entry.symbol, entry.decimals)))
            .collect()
    }

    fn load(settings: &Settings) -> Result<Self> {
        let path = settings.data_path("token_metadata_cache");
        let cache = match load_cache::<Self, _>(&path) {
            CacheLoadResult::Ok(cache) => {
                telemetry::record_cache_hit("token_metadata_cache", "binary");
                cache
            }
            CacheLoadResult::NotFound => {
                telemetry::record_cache_miss("token_metadata_cache", "binary");
                load_legacy_cache::<Self, _>(&path)?.context("token metadata cache not found")?
            }
            CacheLoadResult::VersionMismatch { .. } | CacheLoadResult::Corrupt { .. } => {
                telemetry::record_cache_miss("token_metadata_cache", "binary");
                match load_legacy_cache::<Self, _>(&path)? {
                    Some(cache) => cache,
                    None => {
                        repair_cache(&path).ok();
                        telemetry::record_cache_flush("token_metadata_cache", "repair");
                        anyhow::bail!("token metadata cache is corrupt");
                    }
                }
            }
        };
        cache.validate(settings)?;
        Ok(cache)
    }

    fn save(&self, settings: &Settings) -> Result<()> {
        fs::create_dir_all("state").ok();
        AtomicCacheWriter::new(settings.data_path("token_metadata_cache"), true).write(self)?;
        Ok(())
    }

    fn validate(&self, settings: &Settings) -> Result<()> {
        if self.version != 1 || self.chain_id != settings.chain_id {
            anyhow::bail!("token metadata cache version or chain id mismatch");
        }
        Ok(())
    }
}

impl PoolStateCache {
    fn from_parts(
        settings: &Settings,
        pools: Vec<PoolState>,
        skipped_pools: Vec<SkippedPoolCacheEntry>,
    ) -> Self {
        let saved_at_block = pools
            .iter()
            .filter_map(|pool| (pool.last_updated_block > 0).then_some(pool.last_updated_block))
            .min();
        Self {
            version: 1,
            chain_id: settings.chain_id,
            saved_at_block,
            saved_at_unix_secs: Some(now_unix_secs()),
            pools,
            skipped_pools,
        }
    }

    fn anchor_block(&self) -> Option<u64> {
        let pool_anchor = self
            .pools
            .iter()
            .filter_map(|pool| (pool.last_updated_block > 0).then_some(pool.last_updated_block))
            .min();
        match (self.saved_at_block, pool_anchor) {
            (Some(saved), Some(pool_anchor)) => Some(saved.min(pool_anchor)),
            (Some(saved), None) => Some(saved),
            (None, Some(pool_anchor)) => Some(pool_anchor),
            (None, None) => None,
        }
    }

    fn load(settings: &Settings) -> Result<Self> {
        let path = settings.data_path("pool_state_cache");
        let cache = match load_cache::<Self, _>(&path) {
            CacheLoadResult::Ok(cache) => {
                telemetry::record_cache_hit("pool_state_cache", "binary");
                cache
            }
            CacheLoadResult::NotFound => {
                telemetry::record_cache_miss("pool_state_cache", "binary");
                load_legacy_cache::<Self, _>(&path)?.context("pool state cache not found")?
            }
            CacheLoadResult::VersionMismatch { .. } | CacheLoadResult::Corrupt { .. } => {
                telemetry::record_cache_miss("pool_state_cache", "binary");
                match load_legacy_cache::<Self, _>(&path)? {
                    Some(cache) => cache,
                    None => {
                        repair_cache(&path).ok();
                        telemetry::record_cache_flush("pool_state_cache", "repair");
                        anyhow::bail!("pool state cache is corrupt");
                    }
                }
            }
        };
        cache.validate(settings)?;
        Ok(cache)
    }

    fn save(&self, settings: &Settings) -> Result<()> {
        if pool_state_cache_disabled() {
            return Ok(());
        }
        fs::create_dir_all("state").ok();
        AtomicCacheWriter::new(settings.data_path("pool_state_cache"), true).write(self)?;
        Ok(())
    }

    fn validate(&self, settings: &Settings) -> Result<()> {
        if self.version != 1 || self.chain_id != settings.chain_id {
            anyhow::bail!("pool state cache version or chain id mismatch");
        }
        Ok(())
    }
}

fn pool_matches_spec(pool: &PoolState, spec: &DiscoveredPool) -> bool {
    pool.pool_id == spec.address
        && pool.dex_name == spec.dex_name
        && pool.kind == spec.amm_kind
        && pool.factory == spec.factory
        && pool.registry == spec.registry
        && pool.vault == spec.vault
        && pool.quoter == spec.quoter
}

fn skipped_pool_matches_spec(entry: &SkippedPoolCacheEntry, spec: &DiscoveredPool) -> bool {
    discovered_pool_matches_spec(&entry.spec, spec)
}

fn skipped_pool_cache_applies_to_spec(
    entry: &SkippedPoolCacheEntry,
    spec: &DiscoveredPool,
) -> bool {
    if !spec.priority {
        return true;
    }

    matches!(
        entry.reason.as_str(),
        "v2_zero_liquidity"
            | "aerodrome_v2_zero_liquidity"
            | "v3_zero_liquidity"
            | "curve_fetch_returned_none"
            | "balancer_fetch_returned_none"
    )
}

fn discovered_pool_matches_spec(left: &DiscoveredPool, right: &DiscoveredPool) -> bool {
    left.address == right.address
        && left.dex_name == right.dex_name
        && left.amm_kind == right.amm_kind
        && left.factory == right.factory
        && left.registry == right.registry
        && left.vault == right.vault
        && left.quoter == right.quoter
        && left.balancer_pool_id == right.balancer_pool_id
}

fn insert_skipped_pool(
    skipped_pool_cache: &mut HashMap<Address, SkippedPoolCacheEntry>,
    skipped: SkippedPoolFetch,
) {
    insert_skipped_pool_reason(skipped_pool_cache, skipped.spec, skipped.reason);
}

fn insert_skipped_pool_reason(
    skipped_pool_cache: &mut HashMap<Address, SkippedPoolCacheEntry>,
    spec: DiscoveredPool,
    reason: impl Into<String>,
) {
    skipped_pool_cache.insert(
        spec.address,
        SkippedPoolCacheEntry {
            spec,
            reason: reason.into(),
            skipped_at_unix_secs: now_unix_secs(),
        },
    );
}

fn push_fetch_spec(
    v2_to_fetch: &mut Vec<DiscoveredPool>,
    aerodrome_v2_to_fetch: &mut Vec<DiscoveredPool>,
    v3_to_fetch: &mut Vec<DiscoveredPool>,
    other_to_fetch: &mut Vec<DiscoveredPool>,
    spec: DiscoveredPool,
) {
    match spec.amm_kind {
        AmmKind::UniswapV2Like => v2_to_fetch.push(spec),
        AmmKind::AerodromeV2Like => aerodrome_v2_to_fetch.push(spec),
        AmmKind::UniswapV3Like => v3_to_fetch.push(spec),
        _ => other_to_fetch.push(spec),
    }
}

fn collect_replay_patches(batch: &crate::types::RefreshBatch) -> Vec<PoolStatePatch> {
    batch
        .triggers
        .iter()
        .filter(|trigger| !trigger.full_refresh)
        .filter_map(|trigger| trigger.patch.clone())
        .collect()
}

fn collect_replay_full_refresh_specs(
    pools: &HashMap<Address, PoolState>,
    batch: &crate::types::RefreshBatch,
) -> Vec<DiscoveredPool> {
    let mut seen = HashSet::<Address>::new();
    batch
        .triggers
        .iter()
        .filter(|trigger| trigger.full_refresh)
        .filter_map(|trigger| trigger.pool_id)
        .filter(|pool_id| seen.insert(*pool_id))
        .filter_map(|pool_id| pools.get(&pool_id).map(pool_to_discovered_spec))
        .collect()
}

fn pool_to_discovered_spec(pool: &PoolState) -> DiscoveredPool {
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

fn mark_replayed_pools_current(pools: &mut HashMap<Address, PoolState>, block_number: u64) {
    for pool in pools.values_mut() {
        if pool.health.paused || pool.health.quarantined {
            continue;
        }
        pool.health = replayed_health(&pool.health, block_number);
        pool.last_updated_block = pool.last_updated_block.max(block_number);
    }
}

fn replayed_health(previous: &PoolHealth, block_number: u64) -> PoolHealth {
    PoolHealth {
        stale: false,
        paused: previous.paused,
        quarantined: previous.quarantined,
        confidence_bps: previous.confidence_bps,
        last_successful_refresh_block: block_number,
        last_refresh_at: SystemTime::now(),
        recent_revert_count: previous.recent_revert_count,
    }
}

fn is_high_cardinality_pool_kind(kind: AmmKind) -> bool {
    matches!(
        kind,
        AmmKind::UniswapV2Like | AmmKind::AerodromeV2Like | AmmKind::UniswapV3Like
    )
}

fn skipped_pool_cache_entry_active(entry: &SkippedPoolCacheEntry) -> bool {
    if skipped_pool_cache_disabled() {
        return false;
    }
    let ttl = skipped_pool_cache_ttl();
    now_unix_secs().saturating_sub(entry.skipped_at_unix_secs) <= ttl.as_secs()
}

fn pool_state_cache_disabled() -> bool {
    std::env::var("DISABLE_POOL_STATE_CACHE")
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

fn fetch_unseen_pools_on_bootstrap() -> bool {
    std::env::var("DISCOVERY_FETCH_UNSEEN_POOLS")
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

fn bootstrap_replay_cached_pool_states() -> bool {
    std::env::var("BOOTSTRAP_REPLAY_CACHED_POOL_STATES")
        .map(|value| !matches!(value.as_str(), "0" | "false" | "FALSE" | "no" | "NO"))
        .unwrap_or(true)
}

fn skipped_pool_cache_disabled() -> bool {
    std::env::var("DISABLE_SKIPPED_POOL_CACHE")
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

fn skipped_pool_cache_ttl() -> Duration {
    Duration::from_secs(
        std::env::var("SKIPPED_POOL_CACHE_TTL_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(86_400),
    )
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn derive_missing_prices_from_pools(tokens: &mut [TokenInfo], pools: &HashMap<Address, PoolState>) {
    const MAX_PRICE_DERIVATION_ROUNDS: usize = 5;

    let token_index = tokens
        .iter()
        .enumerate()
        .map(|(idx, token)| (token.address, idx))
        .collect::<HashMap<_, _>>();
    let decimals = tokens
        .iter()
        .map(|token| (token.address, token.decimals))
        .collect::<HashMap<_, _>>();
    let mut prices = tokens
        .iter()
        .filter_map(|token| {
            token
                .manual_price_usd_e8
                .map(|price| (token.address, price))
        })
        .collect::<HashMap<_, _>>();

    for _ in 0..MAX_PRICE_DERIVATION_ROUNDS {
        let mut estimates = HashMap::<Address, Vec<u64>>::new();

        for pool in pools.values() {
            match &pool.state {
                PoolSpecificState::UniswapV2Like(state) if pool.token_addresses.len() == 2 => {
                    collect_balance_price_estimates(
                        &pool.token_addresses,
                        &[state.reserve0, state.reserve1],
                        None,
                        &decimals,
                        &prices,
                        &mut estimates,
                    );
                }
                PoolSpecificState::AerodromeV2Like(state) if pool.token_addresses.len() == 2 => {
                    collect_balance_price_estimates(
                        &pool.token_addresses,
                        &[state.reserve0, state.reserve1],
                        None,
                        &decimals,
                        &prices,
                        &mut estimates,
                    );
                }
                PoolSpecificState::CurvePlain(state) => {
                    collect_balance_price_estimates(
                        &pool.token_addresses,
                        &state.balances,
                        None,
                        &decimals,
                        &prices,
                        &mut estimates,
                    );
                }
                PoolSpecificState::BalancerWeighted(state) => {
                    collect_balance_price_estimates(
                        &pool.token_addresses,
                        &state.balances,
                        Some(&state.normalized_weights_1e18),
                        &decimals,
                        &prices,
                        &mut estimates,
                    );
                }
                PoolSpecificState::UniswapV3Like(state) if pool.token_addresses.len() == 2 => {
                    collect_v3_price_estimates(
                        &pool.token_addresses,
                        state.sqrt_price_x96,
                        &decimals,
                        &prices,
                        &mut estimates,
                    );
                }
                _ => {}
            }
        }

        let mut changed = false;
        for (address, mut values) in estimates {
            if prices.contains_key(&address) || values.is_empty() {
                continue;
            }
            values.sort_unstable();
            let price = values[values.len() / 2];
            if let Some(idx) = token_index.get(&address).copied() {
                tokens[idx].manual_price_usd_e8 = Some(price);
                prices.insert(address, price);
                changed = true;
            }
        }

        if !changed {
            break;
        }
    }
}

fn reserve_accepts_flash_loan(config_data: U256) -> bool {
    reserve_bit(config_data, 56) && !reserve_bit(config_data, 60) && reserve_bit(config_data, 63)
}

fn apply_aave_flash_reserves(tokens: &mut [TokenInfo], flash_reserves: &HashMap<Address, ()>) {
    for token in tokens {
        if flash_reserves.contains_key(&token.address) {
            token.is_cycle_anchor = true;
            token.flash_loan_enabled = true;
        }
    }
}

fn reserve_bit(config_data: U256, bit: usize) -> bool {
    ((config_data >> bit) & U256::from(1u8)) == U256::from(1u8)
}

fn decode_token_symbol_return(raw: &Bytes) -> Option<String> {
    if let Ok(symbol) = IERC20::symbolCall::abi_decode_returns(raw) {
        let trimmed = symbol.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    decode_bytes32_string(raw.as_ref())
}

fn decode_bytes32_string(bytes: &[u8]) -> Option<String> {
    if bytes.len() != 32 {
        return None;
    }
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    if end == 0 {
        return None;
    }
    std::str::from_utf8(&bytes[..end])
        .ok()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn token_metadata_multicall_chunk_size() -> usize {
    std::env::var("TOKEN_METADATA_MULTICALL_CHUNK_SIZE")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(250)
}

fn stable_price_for_symbol(symbol: &str) -> Option<u64> {
    is_stable_symbol(symbol).then_some(100_000_000)
}

fn is_stable_symbol(symbol: &str) -> bool {
    matches!(
        symbol.to_ascii_uppercase().as_str(),
        "USDC" | "USDBC" | "USDT" | "DAI" | "USDS" | "SUSDS" | "LUSD" | "FRAX" | "PYUSD"
    )
}

fn collect_balance_price_estimates(
    addresses: &[Address],
    balances: &[u128],
    weights_1e18: Option<&[u128]>,
    decimals: &HashMap<Address, u8>,
    prices: &HashMap<Address, u64>,
    estimates: &mut HashMap<Address, Vec<u64>>,
) {
    const MIN_PRICE_BALANCE_RAW: u128 = 10_000;

    for (known_idx, &known) in addresses.iter().enumerate() {
        let Some(&known_price) = prices.get(&known) else {
            continue;
        };
        let Some(&known_balance) = balances.get(known_idx) else {
            continue;
        };
        if known_balance <= MIN_PRICE_BALANCE_RAW {
            continue;
        }
        let Some(&known_decimals) = decimals.get(&known) else {
            continue;
        };
        let known_weight = weights_1e18
            .and_then(|weights| weights.get(known_idx).copied())
            .unwrap_or(1);

        for (unknown_idx, &unknown) in addresses.iter().enumerate() {
            if known_idx == unknown_idx {
                continue;
            }
            if prices.contains_key(&unknown) {
                continue;
            }
            let Some(&unknown_balance) = balances.get(unknown_idx) else {
                continue;
            };
            if unknown_balance <= MIN_PRICE_BALANCE_RAW {
                continue;
            }
            let Some(&unknown_decimals) = decimals.get(&unknown) else {
                continue;
            };
            let unknown_weight = weights_1e18
                .and_then(|weights| weights.get(unknown_idx).copied())
                .unwrap_or(1);

            if let Some(price) = derive_price_from_balances(
                known_price,
                known_balance,
                known_decimals,
                known_weight,
                unknown_balance,
                unknown_decimals,
                unknown_weight,
            ) {
                estimates.entry(unknown).or_default().push(price);
            }
        }
    }
}

fn collect_v3_price_estimates(
    addresses: &[Address],
    sqrt_price_x96: U256,
    decimals: &HashMap<Address, u8>,
    prices: &HashMap<Address, u64>,
    estimates: &mut HashMap<Address, Vec<u64>>,
) {
    let token0 = addresses[0];
    let token1 = addresses[1];
    let Some(&decimals0) = decimals.get(&token0) else {
        return;
    };
    let Some(&decimals1) = decimals.get(&token1) else {
        return;
    };
    let Some(raw_sqrt) = u256_to_f64(sqrt_price_x96) else {
        return;
    };
    let sqrt = raw_sqrt / 2f64.powi(96);
    let raw_ratio_1_per_0 = sqrt * sqrt;
    if !raw_ratio_1_per_0.is_finite() || raw_ratio_1_per_0 <= 0.0 {
        return;
    }
    let human_ratio_1_per_0 =
        raw_ratio_1_per_0 * 10f64.powi(i32::from(decimals0) - i32::from(decimals1));
    if !human_ratio_1_per_0.is_finite() || human_ratio_1_per_0 <= 0.0 {
        return;
    }

    if let Some(&price0) = prices.get(&token0) {
        let price1 = (price0 as f64 / human_ratio_1_per_0).round();
        push_price_estimate(estimates, token1, price1);
    }
    if let Some(&price1) = prices.get(&token1) {
        let price0 = (price1 as f64 * human_ratio_1_per_0).round();
        push_price_estimate(estimates, token0, price0);
    }
}

fn push_price_estimate(estimates: &mut HashMap<Address, Vec<u64>>, address: Address, price: f64) {
    if price.is_finite() && price > 0.0 && price <= u64::MAX as f64 {
        estimates.entry(address).or_default().push(price as u64);
    }
}

fn u256_to_f64(value: U256) -> Option<f64> {
    value.to_string().parse::<f64>().ok()
}

fn derive_price_from_balances(
    known_price_usd_e8: u64,
    known_balance: u128,
    known_decimals: u8,
    known_weight_1e18: u128,
    unknown_balance: u128,
    unknown_decimals: u8,
    unknown_weight_1e18: u128,
) -> Option<u64> {
    if known_price_usd_e8 == 0
        || known_balance == 0
        || unknown_balance == 0
        || known_weight_1e18 == 0
        || unknown_weight_1e18 == 0
    {
        return None;
    }

    let numerator = U256::from(known_price_usd_e8)
        .checked_mul(U256::from(known_balance))?
        .checked_mul(U256::from(unknown_weight_1e18))?
        .checked_mul(U256::from(10u64).pow(U256::from(unknown_decimals)))?;
    let denominator = U256::from(unknown_balance)
        .checked_mul(U256::from(known_weight_1e18))?
        .checked_mul(U256::from(10u64).pow(U256::from(known_decimals)))?;
    if denominator.is_zero() {
        return None;
    }

    let value = numerator.checked_div(denominator)?;
    if value.is_zero() || value > U256::from(u64::MAX) {
        return None;
    }
    Some(value.to())
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use crate::types::{AmmKind, PoolAdmissionStatus, V2PoolState};

    use super::*;

    #[test]
    fn reserve_config_requires_active_unpaused_flash_enabled() {
        let active = U256::from(1u8) << 56;
        let paused = U256::from(1u8) << 60;
        let flash = U256::from(1u8) << 63;

        assert!(reserve_accepts_flash_loan(active | flash));
        assert!(!reserve_accepts_flash_loan(flash));
        assert!(!reserve_accepts_flash_loan(active));
        assert!(!reserve_accepts_flash_loan(active | paused | flash));
    }

    #[test]
    fn bytes32_symbol_decoder_trims_zero_padding() {
        let mut raw = [0u8; 32];
        raw[..4].copy_from_slice(b"USDC");
        assert_eq!(decode_bytes32_string(&raw), Some("USDC".to_string()));
    }

    #[test]
    fn balance_price_derivation_accounts_for_decimals() {
        let price = derive_price_from_balances(
            100_000_000,
            1_000_000,
            6,
            1,
            500_000_000_000_000_000,
            18,
            1,
        )
        .unwrap();

        assert_eq!(price, 200_000_000);
    }

    #[test]
    fn aave_reserves_are_added_as_cycle_anchors_without_resetting_configured_anchors() {
        let reserve = Address::from_slice(&[1u8; 20]);
        let non_reserve = Address::from_slice(&[2u8; 20]);
        let mut tokens = vec![
            TokenInfo {
                address: reserve,
                symbol: "USDC".to_string(),
                decimals: 6,
                is_stable: true,
                is_cycle_anchor: false,
                flash_loan_enabled: false,
                allow_self_funded: true,
                behavior: TokenBehavior::default(),
                manual_price_usd_e8: Some(100_000_000),
                max_position_usd_e8: None,
                max_flash_loan_usd_e8: None,
            },
            TokenInfo {
                address: non_reserve,
                symbol: "WETH".to_string(),
                decimals: 18,
                is_stable: false,
                is_cycle_anchor: true,
                flash_loan_enabled: true,
                allow_self_funded: false,
                behavior: TokenBehavior::default(),
                manual_price_usd_e8: Some(300_000_000_000),
                max_position_usd_e8: None,
                max_flash_loan_usd_e8: None,
            },
        ];
        let flash_reserves = HashMap::from([(reserve, ())]);

        apply_aave_flash_reserves(&mut tokens, &flash_reserves);

        assert!(tokens[0].is_cycle_anchor);
        assert!(tokens[0].flash_loan_enabled);
        assert!(tokens[1].is_cycle_anchor);
        assert!(tokens[1].flash_loan_enabled);
    }

    #[test]
    fn patched_health_preserves_previous_health_flags_and_confidence() {
        let previous = PoolHealth {
            stale: true,
            paused: true,
            quarantined: true,
            confidence_bps: 7_500,
            last_successful_refresh_block: 12,
            last_refresh_at: SystemTime::now(),
            recent_revert_count: 4,
        };

        let patched = patched_health(&previous, 99);

        assert!(!patched.stale);
        assert!(patched.paused);
        assert!(patched.quarantined);
        assert_eq!(patched.confidence_bps, previous.confidence_bps);
        assert_eq!(patched.last_successful_refresh_block, 99);
        assert_eq!(patched.recent_revert_count, previous.recent_revert_count);
    }

    #[test]
    fn apply_pool_patch_keeps_health_flags_when_refreshing_pool_state() {
        let pool_id = Address::from_slice(&[9u8; 20]);
        let token0 = Address::from_slice(&[1u8; 20]);
        let token1 = Address::from_slice(&[2u8; 20]);
        let mut pool = PoolState {
            pool_id,
            dex_name: "uniswap".to_string(),
            kind: AmmKind::UniswapV2Like,
            token_addresses: vec![token0, token1],
            token_symbols: vec!["A".to_string(), "B".to_string()],
            factory: None,
            registry: None,
            vault: None,
            quoter: None,
            admission_status: PoolAdmissionStatus::Allowed,
            health: PoolHealth {
                stale: true,
                paused: true,
                quarantined: true,
                confidence_bps: 6_250,
                last_successful_refresh_block: 8,
                last_refresh_at: SystemTime::now(),
                recent_revert_count: 2,
            },
            state: PoolSpecificState::UniswapV2Like(V2PoolState {
                reserve0: 10,
                reserve1: 20,
                fee_ppm: 3_000,
            }),
            last_updated_block: 8,
            extras: HashMap::new(),
        };

        let applied = apply_pool_patch(
            &mut pool,
            PoolStatePatch::UniswapV2Sync {
                pool_id,
                reserve0: 111,
                reserve1: 222,
                block_number: Some(99),
            },
            None,
        );

        assert!(applied);
        match pool.state {
            PoolSpecificState::UniswapV2Like(ref state) => {
                assert_eq!(state.reserve0, 111);
                assert_eq!(state.reserve1, 222);
            }
            _ => panic!("expected v2 state"),
        }
        assert_eq!(pool.last_updated_block, 99);
        assert!(!pool.health.stale);
        assert!(pool.health.paused);
        assert!(pool.health.quarantined);
        assert_eq!(pool.health.confidence_bps, 6_250);
        assert_eq!(pool.health.last_successful_refresh_block, 99);
        assert_eq!(pool.health.recent_revert_count, 2);
    }
}
