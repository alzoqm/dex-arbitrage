use std::{collections::HashMap, fs, sync::Arc, time::Duration};

use alloy::{
    primitives::{aliases::I24, keccak256, Address, Uint, B256, U256},
    sol_types::SolCall,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::{
    abi::{
        IAerodromeCLFactory, IAerodromeV2Factory, ICurveRegistry, ITraderJoeLBFactory,
        IUniswapV2Factory, IUniswapV3Factory,
    },
    cache::{load_cache, load_legacy_cache, repair_cache, AtomicCacheWriter, CacheLoadResult},
    config::{DexConfig, Settings},
    monitoring::metrics as telemetry,
    rpc::{adapt_log_chunk_size, should_reduce_chunk_size, RpcClients},
    types::{AmmKind, BlockRef, DiscoveryKind, FinalityLevel},
};

use super::multicall;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredPool {
    pub address: Address,
    pub dex_name: String,
    pub amm_kind: AmmKind,
    pub factory: Option<Address>,
    pub registry: Option<Address>,
    pub vault: Option<Address>,
    pub quoter: Option<Address>,
    pub balancer_pool_id: Option<B256>,
    #[serde(default)]
    pub priority: bool,
}

#[derive(Debug)]
pub struct FactoryScanner {
    settings: Arc<Settings>,
    rpc: Arc<RpcClients>,
}

impl FactoryScanner {
    pub fn new(settings: Arc<Settings>, rpc: Arc<RpcClients>) -> Self {
        Self { settings, rpc }
    }

    pub async fn scan_all(&self) -> Result<Vec<DiscoveredPool>> {
        let latest = self.latest_block().await?;
        let mut cache = DiscoveryCache::load(&self.settings)
            .unwrap_or_else(|_| DiscoveryCache::new(self.settings.chain_id));
        let save_after_each_dex = env_bool("DISCOVERY_SAVE_AFTER_EACH_DEX", false);
        let mut pools = Vec::new();
        for dex in self.settings.dexes.iter().filter(|dex| dex.enabled) {
            info!(
                dex = %dex.name,
                discovery = ?dex.discovery_kind,
                latest,
                "starting dex discovery"
            );
            let cached = cache.dexes.remove(&dex.name);
            let (mut part, next_cursor) = match dex.discovery_kind {
                DiscoveryKind::FactoryAllPairs => {
                    self.scan_v2_factory(dex, cached.as_ref(), latest).await?
                }
                DiscoveryKind::SolidlyFactoryAllPools => {
                    self.scan_solidly_factory(dex, cached.as_ref(), latest)
                        .await?
                }
                DiscoveryKind::SlipstreamFactoryAllPools => {
                    self.scan_slipstream_factory(dex, cached.as_ref(), latest)
                        .await?
                }
                DiscoveryKind::PoolCreatedLogs => {
                    self.scan_v3_factory_logs(dex, cached.as_ref(), latest)
                        .await?
                }
                DiscoveryKind::TraderJoeLbConfiguredPairs => {
                    self.scan_trader_joe_lb_configured_pairs(dex, cached.as_ref(), latest)
                        .await?
                }
                DiscoveryKind::TraderJoeLbFactoryAllPairs => {
                    self.scan_trader_joe_lb_factory_all_pairs(dex, cached.as_ref(), latest)
                        .await?
                }
                DiscoveryKind::CurveRegistry => {
                    self.scan_curve_registry(dex, cached.as_ref(), latest)
                        .await?
                }
                DiscoveryKind::BalancerVaultLogs => {
                    self.scan_balancer_vault_logs(dex, cached.as_ref(), latest)
                        .await?
                }
                DiscoveryKind::StaticList => (Vec::new(), ScanCursor::default()),
            };
            cache.dexes.insert(
                dex.name.clone(),
                CachedDexScan::from_scan(dex, latest, next_cursor, &part),
            );
            if save_after_each_dex {
                cache.save(&self.settings).ok();
            }
            info!(
                dex = %dex.name,
                pools = part.len(),
                "dex discovery complete"
            );
            pools.append(&mut part);
        }
        if !save_after_each_dex {
            cache.save(&self.settings).ok();
        }
        Ok(pools)
    }

    pub async fn latest_block(&self) -> Result<u64> {
        self.rpc.best_read().block_number().await
    }

    pub async fn current_block_ref(&self) -> Result<BlockRef> {
        let (number, hash, parent_hash) =
            self.rpc.best_read().get_block_by_number("latest").await?;
        Ok(BlockRef {
            number,
            hash,
            parent_hash,
            finality: FinalityLevel::Sealed,
        })
    }

    pub fn spec_for_pool(
        &self,
        snapshot: &crate::graph::GraphSnapshot,
        pool_id: Address,
    ) -> Option<DiscoveredPool> {
        let pool = snapshot.pool(pool_id)?;
        Some(DiscoveredPool {
            address: pool.pool_id,
            dex_name: pool.dex_name.clone(),
            amm_kind: pool.kind,
            factory: pool.factory,
            registry: pool.registry,
            vault: pool.vault,
            quoter: pool.quoter,
            balancer_pool_id: match &pool.state {
                crate::types::PoolSpecificState::BalancerWeighted(state) => Some(state.pool_id),
                _ => None,
            },
            priority: false,
        })
    }

    async fn scan_v2_factory(
        &self,
        dex: &DexConfig,
        cached: Option<&CachedDexScan>,
        latest: u64,
    ) -> Result<(Vec<DiscoveredPool>, ScanCursor)> {
        let Some(factory) = dex.factory else {
            return Ok((Vec::new(), ScanCursor::default()));
        };
        let data = IUniswapV2Factory::allPairsLengthCall {}.abi_encode();
        let raw = self
            .rpc
            .best_read()
            .eth_call(factory, None, data.into(), "latest")
            .await?;
        let ret = IUniswapV2Factory::allPairsLengthCall::abi_decode_returns(&raw)
            .context("decode allPairsLength failed")?;
        let len = u256_to_u64(ret);
        info!(
            dex = %dex.name,
            factory = %factory,
            total_pairs = len,
            "v2 factory pair count loaded"
        );

        if let Some(cached) = cached.filter(|cached| cached.matches(dex, latest)) {
            if cached.cursor.count >= len {
                info!(
                    dex = %dex.name,
                    cached_pairs = cached.pools.len(),
                    total_pairs = len,
                    "v2 discovery cache is complete"
                );
                return Ok((
                    cached.pools.clone(),
                    ScanCursor {
                        block: latest,
                        count: len,
                    },
                ));
            }
        }

        let mut pools = cached_pools(cached, dex);
        let start = cached
            .filter(|cached| cached.matches(dex, latest))
            .map(|cached| cached.cursor.count.min(len))
            .unwrap_or(0);
        let new_pairs = match self.scan_v2_pairs_multicall(factory, start, len).await {
            Ok(pairs) => pairs,
            Err(err) => {
                warn!(dex = %dex.name, error = %err, "multicall allPairs scan failed; falling back to sequential eth_call");
                self.scan_v2_pairs_sequential(factory, start, len).await?
            }
        };
        for ret in new_pairs {
            pools.push(DiscoveredPool {
                address: ret,
                dex_name: dex.name.clone(),
                amm_kind: dex.amm_kind,
                factory: dex.factory,
                registry: dex.registry,
                vault: dex.vault,
                quoter: dex.quoter,
                balancer_pool_id: None,
                priority: false,
            });
        }
        dedup_pools(&mut pools);
        Ok((
            pools,
            ScanCursor {
                block: latest,
                count: len,
            },
        ))
    }

    async fn scan_solidly_factory(
        &self,
        dex: &DexConfig,
        cached: Option<&CachedDexScan>,
        latest: u64,
    ) -> Result<(Vec<DiscoveredPool>, ScanCursor)> {
        let Some(factory) = dex.factory else {
            return Ok((Vec::new(), ScanCursor::default()));
        };
        let raw = self
            .rpc
            .best_read()
            .eth_call(
                factory,
                None,
                IAerodromeV2Factory::allPoolsLengthCall {}
                    .abi_encode()
                    .into(),
                "latest",
            )
            .await?;
        let ret = IAerodromeV2Factory::allPoolsLengthCall::abi_decode_returns(&raw)
            .context("decode allPoolsLength failed")?;
        let len = u256_to_u64(ret);
        info!(
            dex = %dex.name,
            factory = %factory,
            total_pools = len,
            "solidly factory pool count loaded"
        );

        if let Some(cached) = cached.filter(|cached| cached.matches(dex, latest)) {
            if cached.cursor.count >= len {
                info!(
                    dex = %dex.name,
                    cached_pools = cached.pools.len(),
                    total_pools = len,
                    "solidly discovery cache is complete"
                );
                return Ok((
                    cached.pools.clone(),
                    ScanCursor {
                        block: latest,
                        count: len,
                    },
                ));
            }
        }

        let mut pools = cached_pools(cached, dex);
        let start = cached
            .filter(|cached| cached.matches(dex, latest))
            .map(|cached| cached.cursor.count.min(len))
            .unwrap_or(0);
        let new_pools = match self.scan_solidly_pools_multicall(factory, start, len).await {
            Ok(pools) => pools,
            Err(err) => {
                warn!(dex = %dex.name, error = %err, "multicall allPools scan failed; falling back to sequential eth_call");
                self.scan_solidly_pools_sequential(factory, start, len)
                    .await?
            }
        };
        for address in new_pools {
            pools.push(DiscoveredPool {
                address,
                dex_name: dex.name.clone(),
                amm_kind: dex.amm_kind,
                factory: dex.factory,
                registry: dex.registry,
                vault: dex.vault,
                quoter: dex.quoter,
                balancer_pool_id: None,
                priority: false,
            });
        }
        if should_refresh_configured_seeds(cached) {
            let mut seeded = self.seed_configured_solidly_pools(dex).await?;
            pools.append(&mut seeded);
        }
        dedup_pools(&mut pools);
        Ok((
            pools,
            ScanCursor {
                block: latest,
                count: len,
            },
        ))
    }

    async fn seed_configured_solidly_pools(&self, dex: &DexConfig) -> Result<Vec<DiscoveredPool>> {
        let Some(factory) = dex.factory else {
            return Ok(Vec::new());
        };
        let tokens = self
            .settings
            .tokens
            .iter()
            .map(|token| token.address)
            .collect::<Vec<_>>();
        if tokens.len() < 2 {
            return Ok(Vec::new());
        }

        let mut pools = Vec::new();
        for i in 0..tokens.len() {
            for j in (i + 1)..tokens.len() {
                for stable in [false, true] {
                    let raw = self
                        .rpc
                        .best_read()
                        .eth_call(
                            factory,
                            None,
                            IAerodromeV2Factory::getPoolCall {
                                tokenA: tokens[i],
                                tokenB: tokens[j],
                                stable,
                            }
                            .abi_encode()
                            .into(),
                            "latest",
                        )
                        .await?;
                    let pool = IAerodromeV2Factory::getPoolCall::abi_decode_returns(&raw)?;
                    if pool == Address::ZERO {
                        continue;
                    }
                    pools.push(DiscoveredPool {
                        address: pool,
                        dex_name: dex.name.clone(),
                        amm_kind: dex.amm_kind,
                        factory: dex.factory,
                        registry: dex.registry,
                        vault: dex.vault,
                        quoter: dex.quoter,
                        balancer_pool_id: None,
                        priority: true,
                    });
                }
            }
        }
        if !pools.is_empty() {
            info!(
                dex = %dex.name,
                seeded_pool_count = pools.len(),
                "seeded configured-token solidly pools from factory"
            );
        }
        Ok(pools)
    }

    async fn scan_slipstream_factory(
        &self,
        dex: &DexConfig,
        cached: Option<&CachedDexScan>,
        latest: u64,
    ) -> Result<(Vec<DiscoveredPool>, ScanCursor)> {
        let Some(factory) = dex.factory else {
            return Ok((Vec::new(), ScanCursor::default()));
        };
        let raw = self
            .rpc
            .best_read()
            .eth_call(
                factory,
                None,
                IAerodromeCLFactory::allPoolsLengthCall {}
                    .abi_encode()
                    .into(),
                "latest",
            )
            .await?;
        let ret = IAerodromeCLFactory::allPoolsLengthCall::abi_decode_returns(&raw)
            .context("decode slipstream allPoolsLength failed")?;
        let len = u256_to_u64(ret);
        info!(
            dex = %dex.name,
            factory = %factory,
            total_pools = len,
            "slipstream factory pool count loaded"
        );

        if let Some(cached) = cached.filter(|cached| cached.matches(dex, latest)) {
            if cached.cursor.count >= len {
                info!(
                    dex = %dex.name,
                    cached_pools = cached.pools.len(),
                    total_pools = len,
                    "slipstream discovery cache is complete"
                );
                return Ok((
                    cached.pools.clone(),
                    ScanCursor {
                        block: latest,
                        count: len,
                    },
                ));
            }
        }

        let mut pools = cached_pools(cached, dex);
        let start = cached
            .filter(|cached| cached.matches(dex, latest))
            .map(|cached| cached.cursor.count.min(len))
            .unwrap_or(0);
        let new_pools = match self
            .scan_slipstream_pools_multicall(factory, start, len)
            .await
        {
            Ok(pools) => pools,
            Err(err) => {
                warn!(dex = %dex.name, error = %err, "multicall slipstream allPools scan failed; falling back to sequential eth_call");
                self.scan_slipstream_pools_sequential(factory, start, len)
                    .await?
            }
        };
        for address in new_pools {
            pools.push(DiscoveredPool {
                address,
                dex_name: dex.name.clone(),
                amm_kind: dex.amm_kind,
                factory: dex.factory,
                registry: dex.registry,
                vault: dex.vault,
                quoter: dex.quoter,
                balancer_pool_id: None,
                priority: true,
            });
        }
        if should_refresh_configured_seeds(cached) {
            let mut seeded = self.seed_configured_slipstream_pools(dex).await?;
            pools.append(&mut seeded);
        }
        dedup_pools(&mut pools);
        Ok((
            pools,
            ScanCursor {
                block: latest,
                count: len,
            },
        ))
    }

    async fn seed_configured_slipstream_pools(
        &self,
        dex: &DexConfig,
    ) -> Result<Vec<DiscoveredPool>> {
        let Some(factory) = dex.factory else {
            return Ok(Vec::new());
        };
        let tokens = self
            .settings
            .tokens
            .iter()
            .map(|token| token.address)
            .collect::<Vec<_>>();
        if tokens.len() < 2 {
            return Ok(Vec::new());
        }

        let tick_spacings = self.slipstream_tick_spacings(factory).await?;
        if tick_spacings.is_empty() {
            return Ok(Vec::new());
        }

        let mut pools = Vec::new();
        for i in 0..tokens.len() {
            for j in (i + 1)..tokens.len() {
                for &tick_spacing in &tick_spacings {
                    let raw = self
                        .rpc
                        .best_read()
                        .eth_call(
                            factory,
                            None,
                            IAerodromeCLFactory::getPoolCall {
                                tokenA: tokens[i],
                                tokenB: tokens[j],
                                tickSpacing: tick_spacing,
                            }
                            .abi_encode()
                            .into(),
                            "latest",
                        )
                        .await?;
                    let pool = IAerodromeCLFactory::getPoolCall::abi_decode_returns(&raw)?;
                    if pool == Address::ZERO {
                        continue;
                    }
                    pools.push(DiscoveredPool {
                        address: pool,
                        dex_name: dex.name.clone(),
                        amm_kind: dex.amm_kind,
                        factory: dex.factory,
                        registry: dex.registry,
                        vault: dex.vault,
                        quoter: dex.quoter,
                        balancer_pool_id: None,
                        priority: true,
                    });
                }
            }
        }
        if !pools.is_empty() {
            info!(
                dex = %dex.name,
                seeded_pool_count = pools.len(),
                "seeded configured-token slipstream pools from factory"
            );
        }
        Ok(pools)
    }

    async fn slipstream_tick_spacings(&self, factory: Address) -> Result<Vec<I24>> {
        let raw = self
            .rpc
            .best_read()
            .eth_call(
                factory,
                None,
                IAerodromeCLFactory::tickSpacingsCall {}.abi_encode().into(),
                "latest",
            )
            .await?;
        let tick_spacings = IAerodromeCLFactory::tickSpacingsCall::abi_decode_returns(&raw)?;
        if tick_spacings.is_empty() {
            Ok(default_slipstream_tick_spacings())
        } else {
            Ok(tick_spacings)
        }
    }

    async fn scan_v3_factory_logs(
        &self,
        dex: &DexConfig,
        cached: Option<&CachedDexScan>,
        latest: u64,
    ) -> Result<(Vec<DiscoveredPool>, ScanCursor)> {
        let Some(factory) = dex.factory else {
            return Ok((Vec::new(), ScanCursor::default()));
        };
        let topic = signature_hash("PoolCreated(address,address,uint24,int24,address)");
        let mut pools = cached_pools(cached, dex);
        let from_block = cached
            .filter(|cached| cached.matches(dex, latest))
            .map(|cached| cached.cursor.block.saturating_add(1))
            .unwrap_or(dex.start_block);
        let logs = self
            .get_logs_chunked(from_block, latest, &[factory], &[topic])
            .await?;

        for log in logs {
            let data = log.data.as_ref();
            if data.len() < 64 {
                continue;
            }
            let pool_addr = decode_address_from_word(&data[32..64]);
            pools.push(DiscoveredPool {
                address: pool_addr,
                dex_name: dex.name.clone(),
                amm_kind: dex.amm_kind,
                factory: dex.factory,
                registry: dex.registry,
                vault: dex.vault,
                quoter: dex.quoter,
                balancer_pool_id: None,
                priority: false,
            });
        }
        if should_refresh_configured_seeds(cached) {
            let mut seeded = self.seed_configured_v3_pools(dex).await?;
            pools.append(&mut seeded);
        }
        dedup_pools(&mut pools);
        Ok((
            pools,
            ScanCursor {
                block: latest,
                count: 0,
            },
        ))
    }

    async fn seed_configured_v3_pools(&self, dex: &DexConfig) -> Result<Vec<DiscoveredPool>> {
        let Some(factory) = dex.factory else {
            return Ok(Vec::new());
        };
        let tokens = self
            .settings
            .tokens
            .iter()
            .map(|token| token.address)
            .collect::<Vec<_>>();
        if tokens.len() < 2 {
            return Ok(Vec::new());
        }

        let fees = configured_v3_seed_fees();
        let mut pools = Vec::new();
        for i in 0..tokens.len() {
            for j in (i + 1)..tokens.len() {
                for fee in &fees {
                    let raw = self
                        .rpc
                        .best_read()
                        .eth_call(
                            factory,
                            None,
                            IUniswapV3Factory::getPoolCall {
                                tokenA: tokens[i],
                                tokenB: tokens[j],
                                fee: Uint::<24, 1>::from(*fee),
                            }
                            .abi_encode()
                            .into(),
                            "latest",
                        )
                        .await?;
                    let pool = IUniswapV3Factory::getPoolCall::abi_decode_returns(&raw)?;
                    if pool == Address::ZERO {
                        continue;
                    }
                    pools.push(DiscoveredPool {
                        address: pool,
                        dex_name: dex.name.clone(),
                        amm_kind: dex.amm_kind,
                        factory: dex.factory,
                        registry: dex.registry,
                        vault: dex.vault,
                        quoter: dex.quoter,
                        balancer_pool_id: None,
                        priority: true,
                    });
                }
            }
        }
        if !pools.is_empty() {
            info!(
                dex = %dex.name,
                seeded_pool_count = pools.len(),
                "seeded configured-token v3 pools from factory"
            );
        }
        Ok(pools)
    }

    async fn scan_trader_joe_lb_configured_pairs(
        &self,
        dex: &DexConfig,
        cached: Option<&CachedDexScan>,
        latest: u64,
    ) -> Result<(Vec<DiscoveredPool>, ScanCursor)> {
        if let Some(cached) = cached.filter(|cached| cached.matches(dex, latest)) {
            if cached.cursor.block == latest {
                return Ok((
                    cached.pools.clone(),
                    ScanCursor {
                        block: latest,
                        count: cached.pools.len() as u64,
                    },
                ));
            }
        }
        let mut pools = self.seed_configured_trader_joe_lb_pairs(dex).await?;
        dedup_pools(&mut pools);
        Ok((
            pools,
            ScanCursor {
                block: latest,
                count: 0,
            },
        ))
    }

    async fn scan_trader_joe_lb_factory_all_pairs(
        &self,
        dex: &DexConfig,
        cached: Option<&CachedDexScan>,
        latest: u64,
    ) -> Result<(Vec<DiscoveredPool>, ScanCursor)> {
        let Some(factory) = dex.factory else {
            return Ok((Vec::new(), ScanCursor::default()));
        };
        let raw = self
            .rpc
            .best_read()
            .eth_call(
                factory,
                None,
                ITraderJoeLBFactory::getNumberOfLBPairsCall {}
                    .abi_encode()
                    .into(),
                "latest",
            )
            .await?;
        let len = ITraderJoeLBFactory::getNumberOfLBPairsCall::abi_decode_returns(&raw)?;
        let len = u256_to_u64(len);
        info!(
            dex = %dex.name,
            factory = %factory,
            total_pools = len,
            "Trader Joe LB factory pair count loaded"
        );

        if let Some(cached) = cached.filter(|cached| cached.matches(dex, latest)) {
            if cached.cursor.count >= len {
                info!(
                    dex = %dex.name,
                    cached_pools = cached.pools.len(),
                    total_pools = len,
                    "Trader Joe LB discovery cache is complete"
                );
                return Ok((
                    cached.pools.clone(),
                    ScanCursor {
                        block: latest,
                        count: len,
                    },
                ));
            }
        }

        let mut pools = cached_pools(cached, dex);
        let start = cached
            .filter(|cached| cached.matches(dex, latest))
            .map(|cached| cached.cursor.count.min(len))
            .unwrap_or(0);
        let new_pools = match self
            .scan_trader_joe_lb_pairs_multicall(factory, start, len)
            .await
        {
            Ok(pools) => pools,
            Err(err) => {
                warn!(dex = %dex.name, error = %err, "multicall Trader Joe LB pair scan failed; falling back to sequential eth_call");
                self.scan_trader_joe_lb_pairs_sequential(factory, start, len)
                    .await?
            }
        };
        for address in new_pools {
            pools.push(DiscoveredPool {
                address,
                dex_name: dex.name.clone(),
                amm_kind: dex.amm_kind,
                factory: dex.factory,
                registry: dex.registry,
                vault: dex.vault,
                quoter: dex.quoter,
                balancer_pool_id: None,
                priority: false,
            });
        }
        dedup_pools(&mut pools);
        Ok((
            pools,
            ScanCursor {
                block: latest,
                count: len,
            },
        ))
    }

    async fn scan_trader_joe_lb_pairs_multicall(
        &self,
        factory: Address,
        start: u64,
        len: u64,
    ) -> Result<Vec<Address>> {
        if start >= len {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let chunk_size = multicall_chunk_size();
        for chunk_start in (start..len).step_by(chunk_size as usize) {
            let end = (chunk_start + chunk_size).min(len);
            let calls = (chunk_start..end)
                .map(|idx| {
                    (
                        factory,
                        ITraderJoeLBFactory::getLBPairAtIndexCall {
                            id: U256::from(idx),
                        }
                        .abi_encode(),
                    )
                })
                .collect::<Vec<_>>();
            let results = multicall::aggregate3(&self.rpc, calls).await?;
            for raw in results.into_iter().flatten() {
                let pair = ITraderJoeLBFactory::getLBPairAtIndexCall::abi_decode_returns(&raw)?;
                if pair != Address::ZERO {
                    out.push(pair);
                }
            }
        }
        Ok(out)
    }

    async fn scan_trader_joe_lb_pairs_sequential(
        &self,
        factory: Address,
        start: u64,
        len: u64,
    ) -> Result<Vec<Address>> {
        let mut out = Vec::new();
        for idx in start..len {
            let raw = self
                .rpc
                .best_read()
                .eth_call(
                    factory,
                    None,
                    ITraderJoeLBFactory::getLBPairAtIndexCall {
                        id: U256::from(idx),
                    }
                    .abi_encode()
                    .into(),
                    "latest",
                )
                .await?;
            let pair = ITraderJoeLBFactory::getLBPairAtIndexCall::abi_decode_returns(&raw)?;
            if pair != Address::ZERO {
                out.push(pair);
            }
        }
        Ok(out)
    }

    async fn seed_configured_trader_joe_lb_pairs(
        &self,
        dex: &DexConfig,
    ) -> Result<Vec<DiscoveredPool>> {
        let Some(factory) = dex.factory else {
            return Ok(Vec::new());
        };
        let tokens = self
            .settings
            .tokens
            .iter()
            .map(|token| token.address)
            .collect::<Vec<_>>();
        if tokens.len() < 2 {
            return Ok(Vec::new());
        }

        let mut pools = Vec::new();
        let bin_steps = trader_joe_lb_bin_steps(&self.settings.chain.env_key("TRADERJOE_LB_BIN_STEPS"));
        for i in 0..tokens.len() {
            for j in (i + 1)..tokens.len() {
                for &bin_step in &bin_steps {
                    let raw = self
                        .rpc
                        .best_read()
                        .eth_call(
                            factory,
                            None,
                            ITraderJoeLBFactory::getLBPairInformationCall {
                                tokenX: tokens[i],
                                tokenY: tokens[j],
                                binStep: U256::from(bin_step),
                            }
                            .abi_encode()
                            .into(),
                            "latest",
                        )
                        .await?;
                    let info =
                        ITraderJoeLBFactory::getLBPairInformationCall::abi_decode_returns(&raw)?;
                    if info.LBPair == Address::ZERO || info.ignoredForRouting {
                        continue;
                    }
                    pools.push(DiscoveredPool {
                        address: info.LBPair,
                        dex_name: dex.name.clone(),
                        amm_kind: dex.amm_kind,
                        factory: dex.factory,
                        registry: dex.registry,
                        vault: dex.vault,
                        quoter: dex.quoter,
                        balancer_pool_id: None,
                        priority: true,
                    });
                }
            }
        }
        if !pools.is_empty() {
            info!(
                dex = %dex.name,
                seeded_pool_count = pools.len(),
                "seeded configured-token Trader Joe LB pairs from factory"
            );
        }
        Ok(pools)
    }

    async fn scan_curve_registry(
        &self,
        dex: &DexConfig,
        cached: Option<&CachedDexScan>,
        latest: u64,
    ) -> Result<(Vec<DiscoveredPool>, ScanCursor)> {
        let Some(registry) = dex.registry else {
            return Ok((Vec::new(), ScanCursor::default()));
        };
        let data = ICurveRegistry::pool_countCall {}.abi_encode();
        let raw = self
            .rpc
            .best_read()
            .eth_call(registry, None, data.into(), "latest")
            .await?;
        let ret = ICurveRegistry::pool_countCall::abi_decode_returns(&raw)
            .context("decode curve pool_count failed")?;
        let len = u256_to_u64(ret);
        if let Some(cached) = cached.filter(|cached| cached.matches(dex, latest)) {
            if cached.cursor.count >= len {
                info!(
                    dex = %dex.name,
                    cached_pools = cached.pools.len(),
                    total_pools = len,
                    "curve discovery cache is complete"
                );
                return Ok((
                    cached.pools.clone(),
                    ScanCursor {
                        block: latest,
                        count: len,
                    },
                ));
            }
        }

        let mut pools = cached_pools(cached, dex);
        let start = cached
            .filter(|cached| cached.matches(dex, latest))
            .map(|cached| cached.cursor.count.min(len))
            .unwrap_or(0);
        let new_pools = match self.scan_curve_pools_multicall(registry, start, len).await {
            Ok(pools) => pools,
            Err(err) => {
                warn!(dex = %dex.name, error = %err, "multicall curve pool_list scan failed; falling back to sequential eth_call");
                self.scan_curve_pools_sequential(registry, start, len)
                    .await?
            }
        };
        for ret in new_pools {
            pools.push(DiscoveredPool {
                address: ret,
                dex_name: dex.name.clone(),
                amm_kind: dex.amm_kind,
                factory: dex.factory,
                registry: dex.registry,
                vault: dex.vault,
                quoter: dex.quoter,
                balancer_pool_id: None,
                priority: false,
            });
        }
        dedup_pools(&mut pools);
        Ok((
            pools,
            ScanCursor {
                block: latest,
                count: len,
            },
        ))
    }

    async fn scan_balancer_vault_logs(
        &self,
        dex: &DexConfig,
        cached: Option<&CachedDexScan>,
        latest: u64,
    ) -> Result<(Vec<DiscoveredPool>, ScanCursor)> {
        let Some(vault) = dex.vault else {
            return Ok((Vec::new(), ScanCursor::default()));
        };
        let topic = signature_hash("PoolRegistered(bytes32,address,uint8)");
        let from_block = cached
            .filter(|cached| cached.matches(dex, latest))
            .map(|cached| cached.cursor.block.saturating_add(1))
            .unwrap_or(dex.start_block);
        let logs = self
            .get_logs_chunked(from_block, latest, &[vault], &[topic])
            .await?;
        let mut pools = cached_pools(cached, dex);
        for log in logs {
            if log.topics.len() < 3 {
                continue;
            }
            let pool_id = log.topics[1];
            let pool_addr = topic_to_address(log.topics[2]);
            pools.push(DiscoveredPool {
                address: pool_addr,
                dex_name: dex.name.clone(),
                amm_kind: dex.amm_kind,
                factory: dex.factory,
                registry: dex.registry,
                vault: dex.vault,
                quoter: dex.quoter,
                balancer_pool_id: Some(pool_id),
                priority: false,
            });
        }
        dedup_pools(&mut pools);
        Ok((
            pools,
            ScanCursor {
                block: latest,
                count: 0,
            },
        ))
    }

    async fn get_logs_chunked(
        &self,
        from_block: u64,
        to_block: u64,
        addresses: &[Address],
        topics: &[B256],
    ) -> Result<Vec<crate::rpc::RpcLog>> {
        if from_block > to_block {
            return Ok(Vec::new());
        }

        // Use chain-aware chunk size with adaptive splitting
        let provider_mode = std::env::var("RPC_PROVIDER_MODE")
            .ok()
            .unwrap_or_else(|| "payg".to_string());
        let mut chunk_blocks = std::env::var("DISCOVERY_LOG_CHUNK_BLOCKS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(50_000);

        // Apply chain-aware limits
        chunk_blocks = adapt_log_chunk_size(self.settings.chain, &provider_mode, chunk_blocks);
        let base_chunk_blocks = chunk_blocks;
        let min_chunk_blocks = log_min_chunk_blocks().min(base_chunk_blocks).max(1);
        let max_retries = log_max_retries();

        let mut all = Vec::new();
        let mut start = from_block;
        let mut retry_count = 0_usize;
        let mut chunk_count = 0_u64;

        while start <= to_block {
            let end = start.saturating_add(chunk_blocks - 1).min(to_block);
            match self
                .rpc
                .best_read()
                .get_logs(start, end, addresses, topics)
                .await
            {
                Ok(mut logs) => {
                    let logs_found = logs.len();
                    all.append(&mut logs);
                    retry_count = 0; // Reset retry count on success
                    chunk_count += 1;
                    if chunk_count % 100 == 0 || end == to_block {
                        info!(
                            from_block = start,
                            to_block = end,
                            next_block = end.saturating_add(1),
                            chunk_blocks,
                            logs_found,
                            total_logs = all.len(),
                            "log scan progress"
                        );
                    }
                    if end == u64::MAX {
                        break;
                    }
                    start = end.saturating_add(1);
                    if chunk_blocks < base_chunk_blocks {
                        chunk_blocks = chunk_blocks.saturating_mul(2).min(base_chunk_blocks);
                    }
                }
                Err(e) => {
                    let error_msg = e.to_string();
                    let can_reduce = chunk_blocks > min_chunk_blocks;
                    if retry_count < max_retries
                        && (should_reduce_chunk_size(&error_msg) || can_reduce)
                    {
                        let next_chunk_size = if can_reduce {
                            chunk_blocks.saturating_div(2).max(min_chunk_blocks)
                        } else {
                            chunk_blocks
                        };
                        warn!(
                            %error_msg,
                            from_block = start,
                            to_block = end,
                            chunk_size = chunk_blocks,
                            next_chunk_size,
                            retry = retry_count,
                            "log query failed, reducing chunk size and retrying"
                        );
                        chunk_blocks = next_chunk_size;
                        retry_count += 1;
                        let backoff_ms = (250_u64).saturating_mul(retry_count as u64).min(2_000);
                        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                        continue; // Retry with smaller chunk
                    } else {
                        // Either not a chunk size issue or max retries exceeded
                        return Err(e.context("failed to get logs after retries"));
                    }
                }
            }
        }
        Ok(all)
    }

    async fn scan_v2_pairs_multicall(
        &self,
        factory: Address,
        start: u64,
        len: u64,
    ) -> Result<Vec<Address>> {
        if multicall_disabled() {
            anyhow::bail!("multicall disabled");
        }
        let mut out = Vec::new();
        let chunk_size = multicall_chunk_size();
        let mut idx = start;
        let mut chunk_count = 0_u64;
        while idx < len {
            let end = idx.saturating_add(chunk_size).min(len);
            let calls = (idx..end)
                .map(|index| {
                    (
                        factory,
                        IUniswapV2Factory::allPairsCall {
                            index: U256::saturating_from(index),
                        }
                        .abi_encode(),
                    )
                })
                .collect::<Vec<_>>();
            let results = multicall::aggregate3(&self.rpc, calls).await?;
            for raw in results.into_iter().flatten() {
                out.push(
                    IUniswapV2Factory::allPairsCall::abi_decode_returns(&raw)
                        .context("decode multicall allPairs failed")?,
                );
            }
            idx = end;
            chunk_count += 1;
            if chunk_count % 100 == 0 || idx == len {
                info!(
                    factory = %factory,
                    scanned = idx,
                    total = len,
                    discovered = out.len(),
                    "v2 allPairs scan progress"
                );
            }
        }
        Ok(out)
    }

    async fn scan_v2_pairs_sequential(
        &self,
        factory: Address,
        start: u64,
        len: u64,
    ) -> Result<Vec<Address>> {
        let mut out = Vec::new();
        for idx in start..len {
            let data = IUniswapV2Factory::allPairsCall {
                index: U256::saturating_from(idx),
            }
            .abi_encode();
            let raw = self
                .rpc
                .best_read()
                .eth_call(factory, None, data.into(), "latest")
                .await?;
            out.push(
                IUniswapV2Factory::allPairsCall::abi_decode_returns(&raw)
                    .context("decode allPairs failed")?,
            );
        }
        Ok(out)
    }

    async fn scan_solidly_pools_multicall(
        &self,
        factory: Address,
        start: u64,
        len: u64,
    ) -> Result<Vec<Address>> {
        if multicall_disabled() {
            anyhow::bail!("multicall disabled");
        }
        let mut out = Vec::new();
        let chunk_size = multicall_chunk_size();
        let mut idx = start;
        let mut chunk_count = 0_u64;
        while idx < len {
            let end = idx.saturating_add(chunk_size).min(len);
            let calls = (idx..end)
                .map(|index| {
                    (
                        factory,
                        IAerodromeV2Factory::allPoolsCall {
                            index: U256::saturating_from(index),
                        }
                        .abi_encode(),
                    )
                })
                .collect::<Vec<_>>();
            let results = multicall::aggregate3(&self.rpc, calls).await?;
            for raw in results.into_iter().flatten() {
                out.push(
                    IAerodromeV2Factory::allPoolsCall::abi_decode_returns(&raw)
                        .context("decode multicall allPools failed")?,
                );
            }
            idx = end;
            chunk_count += 1;
            if chunk_count % 100 == 0 || idx == len {
                info!(
                    factory = %factory,
                    scanned = idx,
                    total = len,
                    discovered = out.len(),
                    "solidly allPools scan progress"
                );
            }
        }
        Ok(out)
    }

    async fn scan_solidly_pools_sequential(
        &self,
        factory: Address,
        start: u64,
        len: u64,
    ) -> Result<Vec<Address>> {
        let mut out = Vec::new();
        for idx in start..len {
            let raw = self
                .rpc
                .best_read()
                .eth_call(
                    factory,
                    None,
                    IAerodromeV2Factory::allPoolsCall {
                        index: U256::saturating_from(idx),
                    }
                    .abi_encode()
                    .into(),
                    "latest",
                )
                .await?;
            out.push(
                IAerodromeV2Factory::allPoolsCall::abi_decode_returns(&raw)
                    .context("decode allPools failed")?,
            );
        }
        Ok(out)
    }

    async fn scan_slipstream_pools_multicall(
        &self,
        factory: Address,
        start: u64,
        len: u64,
    ) -> Result<Vec<Address>> {
        if multicall_disabled() {
            anyhow::bail!("multicall disabled");
        }
        let mut out = Vec::new();
        let chunk_size = multicall_chunk_size();
        let mut idx = start;
        let mut chunk_count = 0_u64;
        while idx < len {
            let end = idx.saturating_add(chunk_size).min(len);
            let calls = (idx..end)
                .map(|index| {
                    (
                        factory,
                        IAerodromeCLFactory::allPoolsCall {
                            index: U256::saturating_from(index),
                        }
                        .abi_encode(),
                    )
                })
                .collect::<Vec<_>>();
            let results = multicall::aggregate3(&self.rpc, calls).await?;
            for raw in results.into_iter().flatten() {
                out.push(
                    IAerodromeCLFactory::allPoolsCall::abi_decode_returns(&raw)
                        .context("decode multicall slipstream allPools failed")?,
                );
            }
            idx = end;
            chunk_count += 1;
            if chunk_count % 100 == 0 || idx == len {
                info!(
                    factory = %factory,
                    scanned = idx,
                    total = len,
                    discovered = out.len(),
                    "slipstream allPools scan progress"
                );
            }
        }
        Ok(out)
    }

    async fn scan_slipstream_pools_sequential(
        &self,
        factory: Address,
        start: u64,
        len: u64,
    ) -> Result<Vec<Address>> {
        let mut out = Vec::new();
        for idx in start..len {
            let raw = self
                .rpc
                .best_read()
                .eth_call(
                    factory,
                    None,
                    IAerodromeCLFactory::allPoolsCall {
                        index: U256::saturating_from(idx),
                    }
                    .abi_encode()
                    .into(),
                    "latest",
                )
                .await?;
            out.push(
                IAerodromeCLFactory::allPoolsCall::abi_decode_returns(&raw)
                    .context("decode slipstream allPools failed")?,
            );
        }
        Ok(out)
    }

    async fn scan_curve_pools_multicall(
        &self,
        registry: Address,
        start: u64,
        len: u64,
    ) -> Result<Vec<Address>> {
        if multicall_disabled() {
            anyhow::bail!("multicall disabled");
        }
        let mut out = Vec::new();
        let chunk_size = multicall_chunk_size();
        let mut idx = start;
        while idx < len {
            let end = idx.saturating_add(chunk_size).min(len);
            let calls = (idx..end)
                .map(|index| {
                    (
                        registry,
                        ICurveRegistry::pool_listCall {
                            index: U256::saturating_from(index),
                        }
                        .abi_encode(),
                    )
                })
                .collect::<Vec<_>>();
            let results = multicall::aggregate3(&self.rpc, calls).await?;
            for raw in results.into_iter().flatten() {
                out.push(
                    ICurveRegistry::pool_listCall::abi_decode_returns(&raw)
                        .context("decode multicall curve pool_list failed")?,
                );
            }
            idx = end;
        }
        Ok(out)
    }

    async fn scan_curve_pools_sequential(
        &self,
        registry: Address,
        start: u64,
        len: u64,
    ) -> Result<Vec<Address>> {
        let mut out = Vec::new();
        for idx in start..len {
            let data = ICurveRegistry::pool_listCall {
                index: U256::saturating_from(idx),
            }
            .abi_encode();
            let raw = self
                .rpc
                .best_read()
                .eth_call(registry, None, data.into(), "latest")
                .await?;
            out.push(
                ICurveRegistry::pool_listCall::abi_decode_returns(&raw)
                    .context("decode curve pool_list failed")?,
            );
        }
        Ok(out)
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
struct ScanCursor {
    block: u64,
    count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedDexScan {
    amm: AmmKind,
    discovery: DiscoveryKind,
    factory: Option<Address>,
    registry: Option<Address>,
    vault: Option<Address>,
    quoter: Option<Address>,
    latest_block: u64,
    cursor: ScanCursor,
    pools: Vec<DiscoveredPool>,
}

impl CachedDexScan {
    fn from_scan(
        dex: &DexConfig,
        latest_block: u64,
        cursor: ScanCursor,
        pools: &[DiscoveredPool],
    ) -> Self {
        Self {
            amm: dex.amm_kind,
            discovery: dex.discovery_kind,
            factory: dex.factory,
            registry: dex.registry,
            vault: dex.vault,
            quoter: dex.quoter,
            latest_block,
            cursor,
            pools: pools.to_vec(),
        }
    }

    fn matches(&self, dex: &DexConfig, latest: u64) -> bool {
        self.amm == dex.amm_kind
            && self.discovery == dex.discovery_kind
            && self.factory == dex.factory
            && self.registry == dex.registry
            && self.vault == dex.vault
            && self.quoter == dex.quoter
            && self.latest_block <= latest
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DiscoveryCache {
    version: u32,
    chain_id: u64,
    dexes: HashMap<String, CachedDexScan>,
}

impl DiscoveryCache {
    fn new(chain_id: u64) -> Self {
        Self {
            version: 1,
            chain_id,
            dexes: HashMap::new(),
        }
    }

    fn load(settings: &Settings) -> Result<Self> {
        let path = settings.data_path("discovery_cache");
        let cache = match load_cache::<Self, _>(&path) {
            CacheLoadResult::Ok(cache) => {
                telemetry::record_cache_hit("discovery_cache", "binary");
                cache
            }
            CacheLoadResult::NotFound => {
                telemetry::record_cache_miss("discovery_cache", "binary");
                load_legacy_cache::<Self, _>(&path)?.context("discovery cache not found")?
            }
            CacheLoadResult::VersionMismatch { .. } | CacheLoadResult::Corrupt { .. } => {
                telemetry::record_cache_miss("discovery_cache", "binary");
                match load_legacy_cache::<Self, _>(&path)? {
                    Some(cache) => cache,
                    None => {
                        repair_cache(&path).ok();
                        telemetry::record_cache_flush("discovery_cache", "repair");
                        anyhow::bail!("discovery cache is corrupt");
                    }
                }
            }
        };
        cache.validate(settings)?;
        Ok(cache)
    }

    fn save(&self, settings: &Settings) -> Result<()> {
        fs::create_dir_all("state").ok();
        AtomicCacheWriter::new(settings.data_path("discovery_cache"), true).write(self)?;
        Ok(())
    }

    fn validate(&self, settings: &Settings) -> Result<()> {
        if self.version != 1 || self.chain_id != settings.chain_id {
            anyhow::bail!("discovery cache version or chain id mismatch");
        }
        Ok(())
    }
}

fn cached_pools(cached: Option<&CachedDexScan>, dex: &DexConfig) -> Vec<DiscoveredPool> {
    cached
        .filter(|cached| {
            cached.amm == dex.amm_kind
                && cached.discovery == dex.discovery_kind
                && cached.factory == dex.factory
                && cached.registry == dex.registry
                && cached.vault == dex.vault
                && cached.quoter == dex.quoter
        })
        .map(|cached| cached.pools.clone())
        .unwrap_or_default()
}

fn dedup_pools(pools: &mut Vec<DiscoveredPool>) {
    pools.sort_by(|a, b| b.priority.cmp(&a.priority));
    let mut seen = std::collections::HashSet::<(String, Address)>::new();
    pools.retain(|pool| seen.insert((pool.dex_name.clone(), pool.address)));
}

fn should_refresh_configured_seeds(cached: Option<&CachedDexScan>) -> bool {
    cached.is_none() || env_bool("DISCOVERY_REFRESH_CONFIGURED_SEEDS", false)
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

fn configured_v3_seed_fees() -> Vec<u32> {
    let mut fees = std::env::var("V3_FACTORY_SEED_FEES")
        .ok()
        .map(|value| {
            value
                .split(',')
                .filter_map(|part| part.trim().parse::<u32>().ok())
                .filter(|fee| *fee > 0 && *fee < 1_000_000)
                .collect::<Vec<_>>()
        })
        .filter(|fees| !fees.is_empty())
        .unwrap_or_else(|| vec![100, 500, 3_000, 10_000]);
    fees.sort_unstable();
    fees.dedup();
    fees
}

fn trader_joe_lb_bin_steps(env_key: &str) -> Vec<u16> {
    let mut steps = std::env::var(env_key)
        .ok()
        .or_else(|| std::env::var("TRADERJOE_LB_BIN_STEPS").ok())
        .unwrap_or_else(|| "1,2,5,10,15,20,25,50,100".to_string())
        .split(',')
        .filter_map(|value| value.trim().parse::<u16>().ok())
        .filter(|value| *value > 0)
        .collect::<Vec<_>>();
    steps.sort_unstable();
    steps.dedup();
    steps
}

fn default_slipstream_tick_spacings() -> Vec<I24> {
    [1_i32, 10, 50, 100, 200, 500, 2000]
        .into_iter()
        .filter_map(|value| I24::try_from(value).ok())
        .collect()
}

fn signature_hash(signature: &str) -> B256 {
    keccak256(signature.as_bytes())
}

fn topic_to_address(topic: B256) -> Address {
    let bytes = topic.as_slice();
    Address::from_slice(&bytes[12..32])
}

fn decode_address_from_word(word: &[u8]) -> Address {
    Address::from_slice(&word[12..32])
}

fn u256_to_u64(value: alloy::primitives::U256) -> u64 {
    value.to_string().parse::<u64>().unwrap_or(0)
}

fn multicall_chunk_size() -> u64 {
    std::env::var("MULTICALL_CHUNK_SIZE")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(500)
}

fn multicall_disabled() -> bool {
    std::env::var("DISABLE_MULTICALL")
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

fn log_min_chunk_blocks() -> u64 {
    std::env::var("DISCOVERY_LOG_MIN_CHUNK_BLOCKS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(100)
}

fn log_max_retries() -> usize {
    std::env::var("DISCOVERY_LOG_MAX_RETRIES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(12)
}
