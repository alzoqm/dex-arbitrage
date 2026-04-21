use std::{collections::HashMap, sync::Arc, time::SystemTime};

use alloy::{
    primitives::{Address, Bytes, U256},
    sol_types::SolCall,
};
use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::{
    abi::{
        IAerodromeCLPool, IAerodromeV2Factory, IAerodromeV2Pool, IBalancerPool, IBalancerVault,
        ICurvePool, ITraderJoeLBPair, IUniswapV2Pair, IUniswapV3Pool,
    },
    config::{Settings, TokenConfig},
    rpc::RpcClients,
    types::{
        AerodromeV2PoolState, AmmKind, BalancerPoolState, CurvePoolState, PoolAdmissionStatus,
        PoolHealth, PoolSpecificState, PoolState, TraderJoeLbPoolState, V2PoolState, V3PoolState,
    },
};

use super::{factory_scanner::DiscoveredPool, multicall};

#[derive(Debug)]
pub struct PoolFetcher {
    settings: Arc<Settings>,
    rpc: Arc<RpcClients>,
    token_map: HashMap<Address, TokenConfig>,
}

#[derive(Debug, Clone)]
pub struct PoolFetchBatch {
    pub pools: Vec<PoolState>,
    pub skipped: Vec<SkippedPoolFetch>,
}

#[derive(Debug, Clone)]
pub struct SkippedPoolFetch {
    pub spec: DiscoveredPool,
    pub reason: &'static str,
}

impl PoolFetcher {
    pub fn new(settings: Arc<Settings>, rpc: Arc<RpcClients>) -> Self {
        Self {
            token_map: settings.token_map(),
            settings,
            rpc,
        }
    }

    pub async fn fetch_pool(
        &self,
        spec: &DiscoveredPool,
        block_number: Option<u64>,
    ) -> Result<Option<PoolState>> {
        match spec.amm_kind {
            AmmKind::UniswapV2Like => self.fetch_v2(spec, block_number).await,
            AmmKind::AerodromeV2Like => self.fetch_aerodrome_v2(spec, block_number).await,
            AmmKind::UniswapV3Like => self.fetch_v3(spec, block_number).await,
            AmmKind::TraderJoeLb => self.fetch_trader_joe_lb(spec, block_number).await,
            AmmKind::CurvePlain => self.fetch_curve(spec, block_number).await,
            AmmKind::BalancerWeighted => self.fetch_balancer(spec, block_number).await,
        }
    }

    pub async fn fetch_pool_with_retries(
        &self,
        spec: &DiscoveredPool,
        block_number: Option<u64>,
    ) -> Result<Option<PoolState>> {
        let max_retries = pool_fetch_max_retries_for_kind(spec.amm_kind);
        let base_delay_ms = pool_fetch_retry_base_delay_ms();
        let mut attempt = 0usize;

        loop {
            match self.fetch_pool(spec, block_number).await {
                Ok(pool) => return Ok(pool),
                Err(err) if non_retryable_pool_fetch_error(&err) => {
                    warn!(
                        pool = %spec.address,
                        dex = %spec.dex_name,
                        kind = ?spec.amm_kind,
                        error = %err,
                        "pool fetch failed with non-retryable error"
                    );
                    return Err(err);
                }
                Err(err) if attempt < max_retries => {
                    let delay_ms = retry_delay_ms(base_delay_ms, attempt);
                    warn!(
                        pool = %spec.address,
                        dex = %spec.dex_name,
                        kind = ?spec.amm_kind,
                        attempt = attempt + 1,
                        max_retries,
                        delay_ms,
                        error = %err,
                        "pool fetch failed; retrying"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    attempt += 1;
                }
                Err(err) => return Err(err),
            }
        }
    }

    pub async fn fetch_v2_batch(
        &self,
        specs: &[DiscoveredPool],
        block_number: Option<u64>,
    ) -> Result<PoolFetchBatch> {
        if specs.is_empty() {
            return Ok(PoolFetchBatch {
                pools: Vec::new(),
                skipped: Vec::new(),
            });
        }

        let chunk_size = pool_fetch_multicall_chunk_size();
        let mut pools = Vec::new();
        let mut skipped_pools = Vec::new();
        let mut scanned = 0_usize;
        let mut skipped = 0_usize;

        info!(
            total = specs.len(),
            chunk_size, "starting v2 pool state batch fetch"
        );

        for (chunk_index, chunk) in specs.chunks(chunk_size).enumerate() {
            let mut calls = Vec::with_capacity(chunk.len() * 3);
            for spec in chunk {
                calls.push((spec.address, IUniswapV2Pair::token0Call {}.abi_encode()));
                calls.push((spec.address, IUniswapV2Pair::token1Call {}.abi_encode()));
                calls.push((
                    spec.address,
                    IUniswapV2Pair::getReservesCall {}.abi_encode(),
                ));
            }

            match multicall::aggregate3(&self.rpc, calls).await {
                Ok(results) => {
                    for (spec, raw) in chunk.iter().zip(results.chunks(3)) {
                        scanned += 1;
                        let Some(decoded) = decode_v2_batch_result(raw) else {
                            skipped += 1;
                            skipped_pools.push(SkippedPoolFetch {
                                spec: spec.clone(),
                                reason: "v2_decode_or_call_failed",
                            });
                            continue;
                        };
                        let (token0, token1, reserve0, reserve1) = decoded;
                        if reserve0 == 0 || reserve1 == 0 {
                            skipped += 1;
                            skipped_pools.push(SkippedPoolFetch {
                                spec: spec.clone(),
                                reason: "v2_zero_liquidity",
                            });
                            continue;
                        }
                        pools.push(self.build_v2_pool(
                            spec,
                            token0,
                            token1,
                            reserve0,
                            reserve1,
                            block_number,
                        ));
                    }
                }
                Err(err) => {
                    warn!(
                        chunk_index,
                        count = chunk.len(),
                        error = %err,
                        "batch v2 pool fetch failed; falling back to per-pool fetch"
                    );
                    for spec in chunk {
                        scanned += 1;
                        match self.fetch_v2(spec, block_number).await {
                            Ok(Some(pool)) => {
                                if v2_pool_has_liquidity(&pool) {
                                    pools.push(pool);
                                } else {
                                    skipped += 1;
                                    skipped_pools.push(SkippedPoolFetch {
                                        spec: spec.clone(),
                                        reason: "v2_zero_liquidity",
                                    });
                                }
                            }
                            Ok(None) => {
                                skipped += 1;
                                skipped_pools.push(SkippedPoolFetch {
                                    spec: spec.clone(),
                                    reason: "v2_fetch_returned_none",
                                });
                            }
                            Err(err) => {
                                skipped += 1;
                                skipped_pools.push(SkippedPoolFetch {
                                    spec: spec.clone(),
                                    reason: "v2_fetch_failed",
                                });
                                warn!(
                                    pool = %spec.address,
                                    error = %err,
                                    "skipping v2 pool after per-pool fetch failure"
                                );
                            }
                        }
                    }
                }
            }

            if (chunk_index + 1) % 50 == 0 || scanned == specs.len() {
                info!(
                    scanned,
                    total = specs.len(),
                    admitted_candidates = pools.len(),
                    skipped,
                    "v2 pool state batch fetch progress"
                );
            }
        }

        info!(
            scanned,
            total = specs.len(),
            admitted_candidates = pools.len(),
            skipped,
            "v2 pool state batch fetch complete"
        );
        Ok(PoolFetchBatch {
            pools,
            skipped: skipped_pools,
        })
    }

    async fn fetch_v2(
        &self,
        spec: &DiscoveredPool,
        block_number: Option<u64>,
    ) -> Result<Option<PoolState>> {
        let (token0, token1, reserve0, reserve1) = match self.fetch_v2_multicall(spec.address).await
        {
            Ok(values) => values,
            Err(_) => self.fetch_v2_sequential(spec.address).await?,
        };
        Ok(Some(self.build_v2_pool(
            spec,
            token0,
            token1,
            reserve0,
            reserve1,
            block_number,
        )))
    }

    fn build_v2_pool(
        &self,
        spec: &DiscoveredPool,
        token0: Address,
        token1: Address,
        reserve0: u128,
        reserve1: u128,
        block_number: Option<u64>,
    ) -> PoolState {
        let fee_ppm = self
            .settings
            .dexes
            .iter()
            .find(|dex| dex.name == spec.dex_name)
            .map(|dex| dex.fee_ppm)
            .unwrap_or(3_000);
        PoolState {
            pool_id: spec.address,
            dex_name: spec.dex_name.clone(),
            kind: spec.amm_kind,
            token_addresses: vec![token0, token1],
            token_symbols: vec![self.token_symbol(token0), self.token_symbol(token1)],
            factory: spec.factory,
            registry: spec.registry,
            vault: spec.vault,
            quoter: spec.quoter,
            admission_status: PoolAdmissionStatus::Allowed,
            health: fresh_health(10_000, false),
            state: PoolSpecificState::UniswapV2Like(V2PoolState {
                reserve0,
                reserve1,
                fee_ppm,
            }),
            last_updated_block: block_number.unwrap_or_default(),
            extras: HashMap::new(),
        }
    }

    async fn fetch_trader_joe_lb(
        &self,
        spec: &DiscoveredPool,
        block_number: Option<u64>,
    ) -> Result<Option<PoolState>> {
        let calls = vec![
            (
                spec.address,
                ITraderJoeLBPair::getTokenXCall {}.abi_encode(),
            ),
            (
                spec.address,
                ITraderJoeLBPair::getTokenYCall {}.abi_encode(),
            ),
            (
                spec.address,
                ITraderJoeLBPair::getBinStepCall {}.abi_encode(),
            ),
            (
                spec.address,
                ITraderJoeLBPair::getActiveIdCall {}.abi_encode(),
            ),
            (
                spec.address,
                ITraderJoeLBPair::getReservesCall {}.abi_encode(),
            ),
        ];
        let results = multicall::aggregate3(&self.rpc, calls).await?;
        let Some(token_x_raw) = results.first().and_then(|v| v.clone()) else {
            return Ok(None);
        };
        let Some(token_y_raw) = results.get(1).and_then(|v| v.clone()) else {
            return Ok(None);
        };
        let Some(bin_step_raw) = results.get(2).and_then(|v| v.clone()) else {
            return Ok(None);
        };
        let Some(active_id_raw) = results.get(3).and_then(|v| v.clone()) else {
            return Ok(None);
        };
        let Some(reserves_raw) = results.get(4).and_then(|v| v.clone()) else {
            return Ok(None);
        };
        let token_x = ITraderJoeLBPair::getTokenXCall::abi_decode_returns(&token_x_raw)?;
        let token_y = ITraderJoeLBPair::getTokenYCall::abi_decode_returns(&token_y_raw)?;
        let bin_step = ITraderJoeLBPair::getBinStepCall::abi_decode_returns(&bin_step_raw)?;
        let active_id = ITraderJoeLBPair::getActiveIdCall::abi_decode_returns(&active_id_raw)?;
        let reserves = ITraderJoeLBPair::getReservesCall::abi_decode_returns(&reserves_raw)?;
        if reserves.reserveX == 0 || reserves.reserveY == 0 {
            return Ok(None);
        }
        Ok(Some(self.build_trader_joe_lb_pool(
            spec,
            token_x,
            token_y,
            bin_step,
            u256_to_u128(U256::saturating_from(active_id)).min(u32::MAX as u128) as u32,
            reserves.reserveX,
            reserves.reserveY,
            block_number,
        )))
    }

    fn build_trader_joe_lb_pool(
        &self,
        spec: &DiscoveredPool,
        token_x: Address,
        token_y: Address,
        bin_step: u16,
        active_id: u32,
        reserve_x: u128,
        reserve_y: u128,
        block_number: Option<u64>,
    ) -> PoolState {
        PoolState {
            pool_id: spec.address,
            dex_name: spec.dex_name.clone(),
            kind: spec.amm_kind,
            token_addresses: vec![token_x, token_y],
            token_symbols: vec![self.token_symbol(token_x), self.token_symbol(token_y)],
            factory: spec.factory,
            registry: spec.registry,
            vault: spec.vault,
            quoter: spec.quoter,
            admission_status: PoolAdmissionStatus::Allowed,
            health: fresh_health(10_000, false),
            state: PoolSpecificState::TraderJoeLb(TraderJoeLbPoolState {
                reserve_x,
                reserve_y,
                bin_step,
                active_id,
            }),
            last_updated_block: block_number.unwrap_or_default(),
            extras: HashMap::new(),
        }
    }

    pub async fn fetch_trader_joe_lb_batch(
        &self,
        specs: &[DiscoveredPool],
        block_number: Option<u64>,
    ) -> Result<PoolFetchBatch> {
        if specs.is_empty() {
            return Ok(PoolFetchBatch {
                pools: Vec::new(),
                skipped: Vec::new(),
            });
        }

        let chunk_size = pool_fetch_multicall_chunk_size();
        let mut pools = Vec::new();
        let mut skipped_pools = Vec::new();
        let mut scanned = 0usize;
        let mut skipped = 0usize;
        info!(
            total = specs.len(),
            chunk_size, "starting Trader Joe LB pool state batch fetch"
        );

        for (chunk_index, chunk) in specs.chunks(chunk_size).enumerate() {
            let mut calls = Vec::with_capacity(chunk.len() * 5);
            for spec in chunk {
                calls.push((spec.address, ITraderJoeLBPair::getTokenXCall {}.abi_encode()));
                calls.push((spec.address, ITraderJoeLBPair::getTokenYCall {}.abi_encode()));
                calls.push((spec.address, ITraderJoeLBPair::getBinStepCall {}.abi_encode()));
                calls.push((spec.address, ITraderJoeLBPair::getActiveIdCall {}.abi_encode()));
                calls.push((spec.address, ITraderJoeLBPair::getReservesCall {}.abi_encode()));
            }
            match multicall::aggregate3(&self.rpc, calls).await {
                Ok(results) => {
                    for (spec, raw) in chunk.iter().zip(results.chunks(5)) {
                        scanned += 1;
                        let Some(pool) =
                            self.decode_trader_joe_lb_batch_result(spec, raw, block_number)?
                        else {
                            skipped += 1;
                            skipped_pools.push(SkippedPoolFetch {
                                spec: spec.clone(),
                                reason: "trader_joe_lb_decode_or_zero_liquidity",
                            });
                            continue;
                        };
                        pools.push(pool);
                    }
                }
                Err(err) => {
                    warn!(
                        chunk_index,
                        count = chunk.len(),
                        error = %err,
                        "batch Trader Joe LB fetch failed; falling back to per-pool fetch"
                    );
                    for spec in chunk {
                        scanned += 1;
                        match self.fetch_trader_joe_lb(spec, block_number).await {
                            Ok(Some(pool)) => pools.push(pool),
                            Ok(None) => {
                                skipped += 1;
                                skipped_pools.push(SkippedPoolFetch {
                                    spec: spec.clone(),
                                    reason: "trader_joe_lb_fetch_returned_none",
                                });
                            }
                            Err(err) => {
                                skipped += 1;
                                warn!(pool = %spec.address, error = %err, "skipping Trader Joe LB pool after per-pool fetch failure");
                                skipped_pools.push(SkippedPoolFetch {
                                    spec: spec.clone(),
                                    reason: "trader_joe_lb_fetch_failed",
                                });
                            }
                        }
                    }
                }
            }
            if (chunk_index + 1) % 10 == 0 || scanned == specs.len() {
                info!(
                    scanned,
                    total = specs.len(),
                    admitted_candidates = pools.len(),
                    skipped,
                    "Trader Joe LB pool state batch fetch progress"
                );
            }
        }

        info!(
            scanned,
            total = specs.len(),
            admitted_candidates = pools.len(),
            skipped,
            "Trader Joe LB pool state batch fetch complete"
        );
        Ok(PoolFetchBatch {
            pools,
            skipped: skipped_pools,
        })
    }

    fn decode_trader_joe_lb_batch_result(
        &self,
        spec: &DiscoveredPool,
        raw: &[Option<Bytes>],
        block_number: Option<u64>,
    ) -> Result<Option<PoolState>> {
        let Some(token_x_raw) = raw.first().and_then(|v| v.clone()) else {
            return Ok(None);
        };
        let Some(token_y_raw) = raw.get(1).and_then(|v| v.clone()) else {
            return Ok(None);
        };
        let Some(bin_step_raw) = raw.get(2).and_then(|v| v.clone()) else {
            return Ok(None);
        };
        let Some(active_id_raw) = raw.get(3).and_then(|v| v.clone()) else {
            return Ok(None);
        };
        let Some(reserves_raw) = raw.get(4).and_then(|v| v.clone()) else {
            return Ok(None);
        };
        let token_x = ITraderJoeLBPair::getTokenXCall::abi_decode_returns(&token_x_raw)?;
        let token_y = ITraderJoeLBPair::getTokenYCall::abi_decode_returns(&token_y_raw)?;
        let bin_step = ITraderJoeLBPair::getBinStepCall::abi_decode_returns(&bin_step_raw)?;
        let active_id = ITraderJoeLBPair::getActiveIdCall::abi_decode_returns(&active_id_raw)?;
        let reserves = ITraderJoeLBPair::getReservesCall::abi_decode_returns(&reserves_raw)?;
        if reserves.reserveX == 0 || reserves.reserveY == 0 {
            return Ok(None);
        }
        Ok(Some(self.build_trader_joe_lb_pool(
            spec,
            token_x,
            token_y,
            bin_step,
            u256_to_u128(U256::saturating_from(active_id)).min(u32::MAX as u128) as u32,
            reserves.reserveX,
            reserves.reserveY,
            block_number,
        )))
    }

    pub async fn fetch_aerodrome_v2_batch(
        &self,
        specs: &[DiscoveredPool],
        block_number: Option<u64>,
    ) -> Result<PoolFetchBatch> {
        if specs.is_empty() {
            return Ok(PoolFetchBatch {
                pools: Vec::new(),
                skipped: Vec::new(),
            });
        }

        let chunk_size = pool_fetch_multicall_chunk_size();
        let mut pools = Vec::new();
        let mut skipped_pools = Vec::new();
        let mut scanned = 0_usize;
        let mut skipped = 0_usize;

        info!(
            total = specs.len(),
            chunk_size, "starting aerodrome v2 pool state batch fetch"
        );

        for (chunk_index, chunk) in specs.chunks(chunk_size).enumerate() {
            let calls = chunk
                .iter()
                .map(|spec| (spec.address, IAerodromeV2Pool::metadataCall {}.abi_encode()))
                .collect::<Vec<_>>();
            let metadata = match multicall::aggregate3(&self.rpc, calls).await {
                Ok(results) => results,
                Err(err) => {
                    warn!(
                        chunk_index,
                        count = chunk.len(),
                        error = %err,
                        "batch aerodrome v2 metadata fetch failed; falling back to per-pool fetch"
                    );
                    for spec in chunk {
                        scanned += 1;
                        match self.fetch_aerodrome_v2(spec, block_number).await {
                            Ok(Some(pool)) => {
                                if aerodrome_v2_pool_has_liquidity(&pool) {
                                    pools.push(pool);
                                } else {
                                    skipped += 1;
                                    skipped_pools.push(SkippedPoolFetch {
                                        spec: spec.clone(),
                                        reason: "aerodrome_v2_zero_liquidity",
                                    });
                                }
                            }
                            Ok(None) => {
                                skipped += 1;
                                skipped_pools.push(SkippedPoolFetch {
                                    spec: spec.clone(),
                                    reason: "aerodrome_v2_fetch_returned_none",
                                });
                            }
                            Err(err) => {
                                skipped += 1;
                                skipped_pools.push(SkippedPoolFetch {
                                    spec: spec.clone(),
                                    reason: "aerodrome_v2_fetch_failed",
                                });
                                warn!(
                                    pool = %spec.address,
                                    error = %err,
                                    "skipping aerodrome v2 pool after per-pool fetch failure"
                                );
                            }
                        }
                    }
                    continue;
                }
            };

            let mut decoded = Vec::new();
            let mut fee_calls = Vec::new();
            for (spec, raw) in chunk.iter().zip(metadata.iter()) {
                scanned += 1;
                let Some(meta) = raw
                    .as_ref()
                    .and_then(|raw| decode_aerodrome_v2_metadata(raw).ok())
                else {
                    skipped += 1;
                    skipped_pools.push(SkippedPoolFetch {
                        spec: spec.clone(),
                        reason: "aerodrome_v2_decode_or_call_failed",
                    });
                    continue;
                };
                if meta.reserve0 == 0 || meta.reserve1 == 0 {
                    skipped += 1;
                    skipped_pools.push(SkippedPoolFetch {
                        spec: spec.clone(),
                        reason: "aerodrome_v2_zero_liquidity",
                    });
                    continue;
                }
                if let Some(factory) = spec.factory {
                    fee_calls.push((
                        factory,
                        IAerodromeV2Factory::getFeeCall {
                            pool: spec.address,
                            stable: meta.stable,
                        }
                        .abi_encode(),
                    ));
                }
                decoded.push((spec, meta));
            }

            let fee_results = if fee_calls.is_empty() {
                Vec::new()
            } else {
                match multicall::aggregate3(&self.rpc, fee_calls).await {
                    Ok(results) => results,
                    Err(err) => {
                        warn!(
                            chunk_index,
                            error = %err,
                            "batch aerodrome v2 fee fetch failed; using default stable/volatile fees"
                        );
                        Vec::new()
                    }
                }
            };
            let mut fee_iter = fee_results.into_iter();
            for (spec, meta) in decoded {
                let fee_ppm = if spec.factory.is_some() {
                    fee_iter
                        .next()
                        .flatten()
                        .and_then(|raw| decode_aerodrome_fee_ppm(&raw).ok())
                        .unwrap_or_else(|| default_aerodrome_fee_ppm(meta.stable))
                } else {
                    default_aerodrome_fee_ppm(meta.stable)
                };
                pools.push(self.build_aerodrome_v2_pool(spec, meta, fee_ppm, block_number));
            }

            if (chunk_index + 1) % 50 == 0 || scanned == specs.len() {
                info!(
                    scanned,
                    total = specs.len(),
                    admitted_candidates = pools.len(),
                    skipped,
                    "aerodrome v2 pool state batch fetch progress"
                );
            }
        }

        info!(
            scanned,
            total = specs.len(),
            admitted_candidates = pools.len(),
            skipped,
            "aerodrome v2 pool state batch fetch complete"
        );
        Ok(PoolFetchBatch {
            pools,
            skipped: skipped_pools,
        })
    }

    async fn fetch_aerodrome_v2(
        &self,
        spec: &DiscoveredPool,
        block_number: Option<u64>,
    ) -> Result<Option<PoolState>> {
        let raw = self
            .rpc
            .best_read()
            .eth_call(
                spec.address,
                None,
                IAerodromeV2Pool::metadataCall {}.abi_encode().into(),
                "latest",
            )
            .await?;
        let meta = decode_aerodrome_v2_metadata(&raw)?;
        let fee_ppm = match spec.factory {
            Some(factory) => {
                let raw = self
                    .rpc
                    .best_read()
                    .eth_call(
                        factory,
                        None,
                        IAerodromeV2Factory::getFeeCall {
                            pool: spec.address,
                            stable: meta.stable,
                        }
                        .abi_encode()
                        .into(),
                        "latest",
                    )
                    .await?;
                decode_aerodrome_fee_ppm(&raw)
                    .unwrap_or_else(|_| default_aerodrome_fee_ppm(meta.stable))
            }
            None => default_aerodrome_fee_ppm(meta.stable),
        };
        Ok(Some(self.build_aerodrome_v2_pool(
            spec,
            meta,
            fee_ppm,
            block_number,
        )))
    }

    fn build_aerodrome_v2_pool(
        &self,
        spec: &DiscoveredPool,
        meta: AerodromeV2Metadata,
        fee_ppm: u32,
        block_number: Option<u64>,
    ) -> PoolState {
        PoolState {
            pool_id: spec.address,
            dex_name: spec.dex_name.clone(),
            kind: spec.amm_kind,
            token_addresses: vec![meta.token0, meta.token1],
            token_symbols: vec![
                self.token_symbol(meta.token0),
                self.token_symbol(meta.token1),
            ],
            factory: spec.factory,
            registry: spec.registry,
            vault: spec.vault,
            quoter: spec.quoter,
            admission_status: PoolAdmissionStatus::Allowed,
            health: fresh_health(10_000, false),
            state: PoolSpecificState::AerodromeV2Like(AerodromeV2PoolState {
                reserve0: meta.reserve0,
                reserve1: meta.reserve1,
                decimals0: meta.decimals0,
                decimals1: meta.decimals1,
                stable: meta.stable,
                fee_ppm,
            }),
            last_updated_block: block_number.unwrap_or_default(),
            extras: HashMap::new(),
        }
    }

    pub async fn fetch_v3_batch(
        &self,
        specs: &[DiscoveredPool],
        block_number: Option<u64>,
    ) -> Result<PoolFetchBatch> {
        if specs.is_empty() {
            return Ok(PoolFetchBatch {
                pools: Vec::new(),
                skipped: Vec::new(),
            });
        }

        let chunk_size = pool_fetch_multicall_chunk_size();
        let mut pools = Vec::new();
        let mut skipped_pools = Vec::new();
        let mut scanned = 0_usize;
        let mut skipped = 0_usize;

        info!(
            total = specs.len(),
            chunk_size, "starting v3 pool state batch fetch"
        );

        for (chunk_index, chunk) in specs.chunks(chunk_size).enumerate() {
            let mut calls = Vec::with_capacity(chunk.len() * 6);
            for spec in chunk {
                calls.push((spec.address, IUniswapV3Pool::token0Call {}.abi_encode()));
                calls.push((spec.address, IUniswapV3Pool::token1Call {}.abi_encode()));
                calls.push((spec.address, IUniswapV3Pool::feeCall {}.abi_encode()));
                calls.push((
                    spec.address,
                    IUniswapV3Pool::tickSpacingCall {}.abi_encode(),
                ));
                calls.push((spec.address, IUniswapV3Pool::liquidityCall {}.abi_encode()));
                calls.push((spec.address, IUniswapV3Pool::slot0Call {}.abi_encode()));
            }

            match multicall::aggregate3(&self.rpc, calls).await {
                Ok(results) => {
                    for (spec, raw) in chunk.iter().zip(results.chunks(6)) {
                        scanned += 1;
                        let Some(decoded) =
                            decode_v3_batch_result(raw, is_slipstream_v3_spec(spec))
                        else {
                            skipped += 1;
                            skipped_pools.push(SkippedPoolFetch {
                                spec: spec.clone(),
                                reason: "v3_decode_or_call_failed",
                            });
                            continue;
                        };
                        let (token0, token1, fee, tick_spacing, liquidity, sqrt_price_x96, tick) =
                            decoded;
                        if liquidity == 0 {
                            skipped += 1;
                            skipped_pools.push(SkippedPoolFetch {
                                spec: spec.clone(),
                                reason: "v3_zero_liquidity",
                            });
                            continue;
                        }
                        pools.push(self.build_v3_pool(
                            spec,
                            token0,
                            token1,
                            fee,
                            tick_spacing,
                            liquidity,
                            sqrt_price_x96,
                            tick,
                            block_number,
                        ));
                    }
                }
                Err(err) => {
                    warn!(
                        chunk_index,
                        count = chunk.len(),
                        error = %err,
                        "batch v3 pool fetch failed; falling back to per-pool fetch"
                    );
                    for spec in chunk {
                        scanned += 1;
                        match self.fetch_v3(spec, block_number).await {
                            Ok(Some(pool)) => {
                                if v3_pool_has_liquidity(&pool) {
                                    pools.push(pool);
                                } else {
                                    skipped += 1;
                                    skipped_pools.push(SkippedPoolFetch {
                                        spec: spec.clone(),
                                        reason: "v3_zero_liquidity",
                                    });
                                }
                            }
                            Ok(None) => {
                                skipped += 1;
                                skipped_pools.push(SkippedPoolFetch {
                                    spec: spec.clone(),
                                    reason: "v3_fetch_returned_none",
                                });
                            }
                            Err(err) => {
                                skipped += 1;
                                skipped_pools.push(SkippedPoolFetch {
                                    spec: spec.clone(),
                                    reason: "v3_fetch_failed",
                                });
                                warn!(
                                    pool = %spec.address,
                                    error = %err,
                                    "skipping v3 pool after per-pool fetch failure"
                                );
                            }
                        }
                    }
                }
            }

            if (chunk_index + 1) % 50 == 0 || scanned == specs.len() {
                info!(
                    scanned,
                    total = specs.len(),
                    admitted_candidates = pools.len(),
                    skipped,
                    "v3 pool state batch fetch progress"
                );
            }
        }

        info!(
            scanned,
            total = specs.len(),
            admitted_candidates = pools.len(),
            skipped,
            "v3 pool state batch fetch complete"
        );
        Ok(PoolFetchBatch {
            pools,
            skipped: skipped_pools,
        })
    }

    async fn fetch_v3(
        &self,
        spec: &DiscoveredPool,
        block_number: Option<u64>,
    ) -> Result<Option<PoolState>> {
        let slipstream = is_slipstream_v3_spec(spec);
        let (token0, token1, fee, tick_spacing, liquidity, sqrt_price_x96, tick) =
            match self.fetch_v3_multicall(spec.address, slipstream).await {
                Ok(values) => values,
                Err(_) => self.fetch_v3_sequential(spec.address, slipstream).await?,
            };

        Ok(Some(self.build_v3_pool(
            spec,
            token0,
            token1,
            fee,
            tick_spacing,
            liquidity,
            sqrt_price_x96,
            tick,
            block_number,
        )))
    }

    #[allow(clippy::too_many_arguments)]
    fn build_v3_pool(
        &self,
        spec: &DiscoveredPool,
        token0: Address,
        token1: Address,
        fee: u32,
        tick_spacing: i32,
        liquidity: u128,
        sqrt_price_x96: U256,
        tick: i32,
        block_number: Option<u64>,
    ) -> PoolState {
        PoolState {
            pool_id: spec.address,
            dex_name: spec.dex_name.clone(),
            kind: spec.amm_kind,
            token_addresses: vec![token0, token1],
            token_symbols: vec![self.token_symbol(token0), self.token_symbol(token1)],
            factory: spec.factory,
            registry: spec.registry,
            vault: spec.vault,
            quoter: spec.quoter,
            admission_status: PoolAdmissionStatus::Allowed,
            health: fresh_health(10_000, false),
            state: PoolSpecificState::UniswapV3Like(V3PoolState {
                sqrt_price_x96,
                liquidity,
                tick,
                fee,
                tick_spacing,
            }),
            last_updated_block: block_number.unwrap_or_default(),
            extras: HashMap::new(),
        }
    }

    async fn fetch_curve(
        &self,
        spec: &DiscoveredPool,
        block_number: Option<u64>,
    ) -> Result<Option<PoolState>> {
        let amp_raw = self
            .rpc
            .best_read()
            .eth_call(
                spec.address,
                None,
                ICurvePool::ACall {}.abi_encode().into(),
                "latest",
            )
            .await?;
        let amp_ret = ICurvePool::ACall::abi_decode_returns(&amp_raw)?;
        let fee_raw = self
            .rpc
            .best_read()
            .eth_call(
                spec.address,
                None,
                ICurvePool::feeCall {}.abi_encode().into(),
                "latest",
            )
            .await?;
        let fee_ret = ICurvePool::feeCall::abi_decode_returns(&fee_raw)?;

        let mut token_addresses = Vec::new();
        let mut token_symbols = Vec::new();
        let mut balances = Vec::new();

        for i in 0..8u32 {
            let coin_call = ICurvePool::coinsCall {
                i: U256::saturating_from(i),
            }
            .abi_encode();
            let coin_raw = match self
                .rpc
                .best_read()
                .eth_call(spec.address, None, coin_call.into(), "latest")
                .await
            {
                Ok(raw) => raw,
                Err(_) => break,
            };
            let coin_ret = match ICurvePool::coinsCall::abi_decode_returns(&coin_raw) {
                Ok(ret) => ret,
                Err(_) => break,
            };
            let bal_call = ICurvePool::balancesCall {
                i: U256::saturating_from(i),
            }
            .abi_encode();
            let bal_raw = self
                .rpc
                .best_read()
                .eth_call(spec.address, None, bal_call.into(), "latest")
                .await?;
            let bal_ret = ICurvePool::balancesCall::abi_decode_returns(&bal_raw)?;
            token_symbols.push(self.token_symbol(coin_ret));
            token_addresses.push(coin_ret);
            balances.push(u256_to_u128(bal_ret));
        }

        if token_addresses.len() < 2 {
            return Ok(None);
        }

        Ok(Some(PoolState {
            pool_id: spec.address,
            dex_name: spec.dex_name.clone(),
            kind: spec.amm_kind,
            token_addresses,
            token_symbols,
            factory: spec.factory,
            registry: spec.registry,
            vault: spec.vault,
            quoter: spec.quoter,
            admission_status: PoolAdmissionStatus::Allowed,
            health: fresh_health(10_000, false),
            state: PoolSpecificState::CurvePlain(CurvePoolState {
                balances,
                amp: u256_to_u128(amp_ret),
                fee: u256_to_u128(fee_ret).min(u32::MAX as u128) as u32,
                supports_underlying: false,
            }),
            last_updated_block: block_number.unwrap_or_default(),
            extras: HashMap::new(),
        }))
    }

    async fn fetch_balancer(
        &self,
        spec: &DiscoveredPool,
        block_number: Option<u64>,
    ) -> Result<Option<PoolState>> {
        let pool_id = match spec.balancer_pool_id {
            Some(id) => id,
            None => {
                let raw = self
                    .rpc
                    .best_read()
                    .eth_call(
                        spec.address,
                        None,
                        IBalancerPool::getPoolIdCall {}.abi_encode().into(),
                        "latest",
                    )
                    .await?;
                IBalancerPool::getPoolIdCall::abi_decode_returns(&raw)?
            }
        };
        let vault = spec.vault.or_else(|| {
            self.settings
                .dexes
                .iter()
                .find(|d| d.name == spec.dex_name)
                .and_then(|d| d.vault)
        });
        let Some(vault) = vault else { return Ok(None) };
        let raw_tokens = self
            .rpc
            .best_read()
            .eth_call(
                vault,
                None,
                IBalancerVault::getPoolTokensCall { poolId: pool_id }
                    .abi_encode()
                    .into(),
                "latest",
            )
            .await?;
        let tokens_ret = IBalancerVault::getPoolTokensCall::abi_decode_returns(&raw_tokens)?;
        let raw_weights = self
            .rpc
            .best_read()
            .eth_call(
                spec.address,
                None,
                IBalancerPool::getNormalizedWeightsCall {}
                    .abi_encode()
                    .into(),
                "latest",
            )
            .await?;
        let weights_ret =
            IBalancerPool::getNormalizedWeightsCall::abi_decode_returns(&raw_weights)?;
        let raw_fee = self
            .rpc
            .best_read()
            .eth_call(
                spec.address,
                None,
                IBalancerPool::getSwapFeePercentageCall {}
                    .abi_encode()
                    .into(),
                "latest",
            )
            .await?;
        let fee_ret = IBalancerPool::getSwapFeePercentageCall::abi_decode_returns(&raw_fee)?;
        let pause_raw = self
            .rpc
            .best_read()
            .eth_call(
                spec.address,
                None,
                IBalancerPool::getPausedStateCall {}.abi_encode().into(),
                "latest",
            )
            .await
            .ok();
        let paused = pause_raw
            .and_then(|raw| IBalancerPool::getPausedStateCall::abi_decode_returns(&raw).ok())
            .map(|ret| ret.paused)
            .unwrap_or(false);

        Ok(Some(PoolState {
            pool_id: spec.address,
            dex_name: spec.dex_name.clone(),
            kind: spec.amm_kind,
            token_addresses: tokens_ret.tokens.clone(),
            token_symbols: tokens_ret
                .tokens
                .iter()
                .map(|token| self.token_symbol(*token))
                .collect(),
            factory: spec.factory,
            registry: spec.registry,
            vault: Some(vault),
            quoter: spec.quoter,
            admission_status: if paused {
                PoolAdmissionStatus::Quarantined
            } else {
                PoolAdmissionStatus::Allowed
            },
            health: fresh_health(if paused { 8_500 } else { 10_000 }, paused),
            state: PoolSpecificState::BalancerWeighted(BalancerPoolState {
                pool_id,
                balances: tokens_ret
                    .balances
                    .iter()
                    .cloned()
                    .map(u256_to_u128)
                    .collect(),
                normalized_weights_1e18: weights_ret.iter().cloned().map(u256_to_u128).collect(),
                swap_fee_ppm: u256_to_u128(fee_ret).min(u32::MAX as u128) as u32,
            }),
            last_updated_block: block_number.unwrap_or_default(),
            extras: HashMap::new(),
        }))
    }

    async fn fetch_v2_multicall(&self, pool: Address) -> Result<(Address, Address, u128, u128)> {
        let results = multicall::aggregate3(
            &self.rpc,
            vec![
                (pool, IUniswapV2Pair::token0Call {}.abi_encode()),
                (pool, IUniswapV2Pair::token1Call {}.abi_encode()),
                (pool, IUniswapV2Pair::getReservesCall {}.abi_encode()),
            ],
        )
        .await?;
        let token0 = IUniswapV2Pair::token0Call::abi_decode_returns(
            results
                .first()
                .and_then(|value| value.as_ref())
                .context("multicall token0 failed")?,
        )?;
        let token1 = IUniswapV2Pair::token1Call::abi_decode_returns(
            results
                .get(1)
                .and_then(|value| value.as_ref())
                .context("multicall token1 failed")?,
        )?;
        let reserves = IUniswapV2Pair::getReservesCall::abi_decode_returns(
            results
                .get(2)
                .and_then(|value| value.as_ref())
                .context("multicall getReserves failed")?,
        )?;
        Ok((
            token0,
            token1,
            u256_to_u128(U256::saturating_from(reserves.reserve0)),
            u256_to_u128(U256::saturating_from(reserves.reserve1)),
        ))
    }

    async fn fetch_v2_sequential(&self, pool: Address) -> Result<(Address, Address, u128, u128)> {
        let token0 = self
            .call_address(pool, IUniswapV2Pair::token0Call {}.abi_encode())
            .await?;
        let token1 = self
            .call_address(pool, IUniswapV2Pair::token1Call {}.abi_encode())
            .await?;
        let raw = self
            .rpc
            .best_read()
            .eth_call(
                pool,
                None,
                IUniswapV2Pair::getReservesCall {}.abi_encode().into(),
                "latest",
            )
            .await?;
        let reserves = IUniswapV2Pair::getReservesCall::abi_decode_returns(&raw)?;
        Ok((
            token0,
            token1,
            u256_to_u128(U256::saturating_from(reserves.reserve0)),
            u256_to_u128(U256::saturating_from(reserves.reserve1)),
        ))
    }

    async fn fetch_v3_multicall(
        &self,
        pool: Address,
        slipstream: bool,
    ) -> Result<(Address, Address, u32, i32, u128, U256, i32)> {
        let results = multicall::aggregate3(
            &self.rpc,
            vec![
                (pool, IUniswapV3Pool::token0Call {}.abi_encode()),
                (pool, IUniswapV3Pool::token1Call {}.abi_encode()),
                (pool, IUniswapV3Pool::feeCall {}.abi_encode()),
                (pool, IUniswapV3Pool::tickSpacingCall {}.abi_encode()),
                (pool, IUniswapV3Pool::liquidityCall {}.abi_encode()),
                (pool, IUniswapV3Pool::slot0Call {}.abi_encode()),
            ],
        )
        .await?;
        let token0 = IUniswapV3Pool::token0Call::abi_decode_returns(
            results
                .first()
                .and_then(|value| value.as_ref())
                .context("multicall v3 token0 failed")?,
        )?;
        let token1 = IUniswapV3Pool::token1Call::abi_decode_returns(
            results
                .get(1)
                .and_then(|value| value.as_ref())
                .context("multicall v3 token1 failed")?,
        )?;
        let fee = IUniswapV3Pool::feeCall::abi_decode_returns(
            results
                .get(2)
                .and_then(|value| value.as_ref())
                .context("multicall v3 fee failed")?,
        )?;
        let tick_spacing = IUniswapV3Pool::tickSpacingCall::abi_decode_returns(
            results
                .get(3)
                .and_then(|value| value.as_ref())
                .context("multicall v3 tickSpacing failed")?,
        )?;
        let liquidity = IUniswapV3Pool::liquidityCall::abi_decode_returns(
            results
                .get(4)
                .and_then(|value| value.as_ref())
                .context("multicall v3 liquidity failed")?,
        )?;
        let raw_slot0 = results
            .get(5)
            .and_then(|value| value.as_ref())
            .context("multicall v3 slot0 failed")?;
        let (sqrt_price_x96, tick) = decode_v3_slot0(raw_slot0, slipstream)?;
        Ok((
            token0,
            token1,
            u32::try_from(fee).unwrap_or(u32::MAX),
            i32::try_from(tick_spacing).unwrap_or(0),
            liquidity,
            sqrt_price_x96,
            tick,
        ))
    }

    async fn fetch_v3_sequential(
        &self,
        pool: Address,
        slipstream: bool,
    ) -> Result<(Address, Address, u32, i32, u128, U256, i32)> {
        let token0 = self
            .call_address(pool, IUniswapV3Pool::token0Call {}.abi_encode())
            .await?;
        let token1 = self
            .call_address(pool, IUniswapV3Pool::token1Call {}.abi_encode())
            .await?;
        let raw_fee = self
            .rpc
            .best_read()
            .eth_call(
                pool,
                None,
                IUniswapV3Pool::feeCall {}.abi_encode().into(),
                "latest",
            )
            .await?;
        let fee = IUniswapV3Pool::feeCall::abi_decode_returns(&raw_fee)?;
        let raw_spacing = self
            .rpc
            .best_read()
            .eth_call(
                pool,
                None,
                IUniswapV3Pool::tickSpacingCall {}.abi_encode().into(),
                "latest",
            )
            .await?;
        let tick_spacing = IUniswapV3Pool::tickSpacingCall::abi_decode_returns(&raw_spacing)?;
        let raw_liq = self
            .rpc
            .best_read()
            .eth_call(
                pool,
                None,
                IUniswapV3Pool::liquidityCall {}.abi_encode().into(),
                "latest",
            )
            .await?;
        let liquidity = IUniswapV3Pool::liquidityCall::abi_decode_returns(&raw_liq)?;
        let raw_slot0 = self
            .rpc
            .best_read()
            .eth_call(
                pool,
                None,
                IUniswapV3Pool::slot0Call {}.abi_encode().into(),
                "latest",
            )
            .await?;
        let (sqrt_price_x96, tick) = decode_v3_slot0(&raw_slot0, slipstream)?;
        Ok((
            token0,
            token1,
            u32::try_from(fee).unwrap_or(u32::MAX),
            i32::try_from(tick_spacing).unwrap_or(0),
            liquidity,
            sqrt_price_x96,
            tick,
        ))
    }

    async fn call_address(&self, to: Address, data: Vec<u8>) -> Result<Address> {
        let raw = self
            .rpc
            .best_read()
            .eth_call(to, None, data.into(), "latest")
            .await?;
        let bytes = raw.as_ref();
        if bytes.len() < 32 {
            anyhow::bail!("address return too short");
        }
        Ok(Address::from_slice(&bytes[12..32]))
    }

    fn token_symbol(&self, address: Address) -> String {
        self.token_map
            .get(&address)
            .map(|token| token.symbol.clone())
            .unwrap_or_else(|| address.to_string())
    }
}

fn fresh_health(confidence_bps: u16, paused: bool) -> PoolHealth {
    PoolHealth {
        stale: false,
        paused,
        quarantined: false,
        confidence_bps,
        last_successful_refresh_block: 0,
        last_refresh_at: SystemTime::now(),
        recent_revert_count: 0,
    }
}

fn u256_to_u128(value: alloy::primitives::U256) -> u128 {
    value.to_string().parse::<u128>().unwrap_or(u128::MAX)
}

#[derive(Debug, Clone, Copy)]
struct AerodromeV2Metadata {
    decimals0: u128,
    decimals1: u128,
    reserve0: u128,
    reserve1: u128,
    stable: bool,
    token0: Address,
    token1: Address,
}

fn decode_aerodrome_v2_metadata(raw: &Bytes) -> Result<AerodromeV2Metadata> {
    let ret = IAerodromeV2Pool::metadataCall::abi_decode_returns(raw)?;
    Ok(AerodromeV2Metadata {
        decimals0: u256_to_u128(ret.dec0),
        decimals1: u256_to_u128(ret.dec1),
        reserve0: u256_to_u128(ret.r0),
        reserve1: u256_to_u128(ret.r1),
        stable: ret.st,
        token0: ret.t0,
        token1: ret.t1,
    })
}

fn decode_aerodrome_fee_ppm(raw: &Bytes) -> Result<u32> {
    let fee_bps = IAerodromeV2Factory::getFeeCall::abi_decode_returns(raw)?;
    let fee_ppm = u256_to_u128(fee_bps).saturating_mul(100);
    Ok(u32::try_from(fee_ppm).unwrap_or(u32::MAX))
}

fn default_aerodrome_fee_ppm(stable: bool) -> u32 {
    if stable {
        500
    } else {
        3_000
    }
}

fn decode_v2_batch_result(raw: &[Option<Bytes>]) -> Option<(Address, Address, u128, u128)> {
    let token0 = IUniswapV2Pair::token0Call::abi_decode_returns(raw.first()?.as_ref()?).ok()?;
    let token1 = IUniswapV2Pair::token1Call::abi_decode_returns(raw.get(1)?.as_ref()?).ok()?;
    let reserves =
        IUniswapV2Pair::getReservesCall::abi_decode_returns(raw.get(2)?.as_ref()?).ok()?;
    Some((
        token0,
        token1,
        u256_to_u128(U256::saturating_from(reserves.reserve0)),
        u256_to_u128(U256::saturating_from(reserves.reserve1)),
    ))
}

fn decode_v3_batch_result(
    raw: &[Option<Bytes>],
    slipstream: bool,
) -> Option<(Address, Address, u32, i32, u128, U256, i32)> {
    let token0 = IUniswapV3Pool::token0Call::abi_decode_returns(raw.first()?.as_ref()?).ok()?;
    let token1 = IUniswapV3Pool::token1Call::abi_decode_returns(raw.get(1)?.as_ref()?).ok()?;
    let fee = IUniswapV3Pool::feeCall::abi_decode_returns(raw.get(2)?.as_ref()?).ok()?;
    let tick_spacing =
        IUniswapV3Pool::tickSpacingCall::abi_decode_returns(raw.get(3)?.as_ref()?).ok()?;
    let liquidity =
        IUniswapV3Pool::liquidityCall::abi_decode_returns(raw.get(4)?.as_ref()?).ok()?;
    let (sqrt_price_x96, tick) = decode_v3_slot0(raw.get(5)?.as_ref()?, slipstream).ok()?;
    Some((
        token0,
        token1,
        u32::try_from(fee).unwrap_or(u32::MAX),
        i32::try_from(tick_spacing).unwrap_or(0),
        liquidity,
        sqrt_price_x96,
        tick,
    ))
}

fn decode_v3_slot0(raw: &Bytes, slipstream: bool) -> Result<(U256, i32)> {
    if slipstream {
        let slot0 = IAerodromeCLPool::slot0Call::abi_decode_returns(raw)?;
        Ok((
            U256::saturating_from(slot0.sqrtPriceX96),
            i32::try_from(slot0.tick).unwrap_or(0),
        ))
    } else {
        let slot0 = IUniswapV3Pool::slot0Call::abi_decode_returns(raw)?;
        Ok((
            U256::saturating_from(slot0.sqrtPriceX96),
            i32::try_from(slot0.tick).unwrap_or(0),
        ))
    }
}

fn is_slipstream_v3_spec(spec: &DiscoveredPool) -> bool {
    spec.dex_name.starts_with("aerodrome_v3")
}

fn v2_pool_has_liquidity(pool: &PoolState) -> bool {
    match &pool.state {
        PoolSpecificState::UniswapV2Like(state) => state.reserve0 > 0 && state.reserve1 > 0,
        _ => true,
    }
}

fn aerodrome_v2_pool_has_liquidity(pool: &PoolState) -> bool {
    match &pool.state {
        PoolSpecificState::AerodromeV2Like(state) => state.reserve0 > 0 && state.reserve1 > 0,
        _ => true,
    }
}

fn v3_pool_has_liquidity(pool: &PoolState) -> bool {
    match &pool.state {
        PoolSpecificState::UniswapV3Like(state) => state.liquidity > 0,
        _ => true,
    }
}

fn pool_fetch_multicall_chunk_size() -> usize {
    std::env::var("POOL_FETCH_MULTICALL_CHUNK_SIZE")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(150)
}

fn pool_fetch_max_retries() -> usize {
    std::env::var("POOL_FETCH_MAX_RETRIES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(5)
}

fn pool_fetch_max_retries_for_kind(kind: AmmKind) -> usize {
    if matches!(
        kind,
        AmmKind::UniswapV2Like
            | AmmKind::AerodromeV2Like
            | AmmKind::UniswapV3Like
            | AmmKind::TraderJoeLb
    ) {
        return pool_fetch_max_retries();
    }
    std::env::var("POOL_FETCH_OTHER_MAX_RETRIES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0)
}

fn pool_fetch_retry_base_delay_ms() -> u64 {
    std::env::var("POOL_FETCH_RETRY_BASE_DELAY_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(250)
}

fn retry_delay_ms(base_delay_ms: u64, attempt: usize) -> u64 {
    let multiplier = 1_u64 << attempt.min(5);
    base_delay_ms.saturating_mul(multiplier).min(8_000)
}

fn non_retryable_pool_fetch_error(err: &anyhow::Error) -> bool {
    let message = format!("{err:#}").to_ascii_lowercase();
    message.contains("execution reverted")
        || message.contains("abi decode")
        || message.contains("decode")
}
