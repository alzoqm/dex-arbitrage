use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use alloy::{
    primitives::{aliases::I24, Address, Bytes, Uint, U256},
    sol_types::SolCall,
};
use anyhow::Result;
use parking_lot::Mutex;
use tokio::time::timeout;

use crate::{
    abi::{IAerodromeCLQuoter, IBalancerVault, ICurvePool, ITraderJoeLBPair, IV3QuoterV2},
    amm,
    graph::GraphSnapshot,
    monitoring::metrics as telemetry,
    rpc::RpcClients,
    types::{PoolSpecificState, PoolState},
};

use super::v3_limits::sqrt_price_limit_u160;

#[derive(Debug, Clone)]
pub struct ExactQuoter {
    rpc: Arc<RpcClients>,
    quote_cache: Arc<Mutex<QuoteCache>>,
    v3_revert_cache: Arc<Mutex<V3RevertCache>>,
    v3_direction_blocklist: Arc<Mutex<V3DirectionBlocklist>>,
    curve_underlying_support: Arc<Mutex<HashMap<alloy::primitives::Address, bool>>>,
    use_v3_rpc_quoter: bool,
    use_lb_rpc_quoter: bool,
    use_curve_rpc_quoter: bool,
    use_balancer_rpc_quoter: bool,
    quote_rpc_timeout: Duration,
    chain_label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct QuoteKey {
    snapshot_id: u64,
    pool_id: alloy::primitives::Address,
    token_in: alloy::primitives::Address,
    token_out: alloy::primitives::Address,
    amount_in: u128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct V3DirectionKey {
    snapshot_id: u64,
    pool_id: alloy::primitives::Address,
    token_in: alloy::primitives::Address,
    token_out: alloy::primitives::Address,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PoolDirectionKey {
    pool_id: alloy::primitives::Address,
    token_in: alloy::primitives::Address,
    token_out: alloy::primitives::Address,
}

static V3_DIRECTION_BLOCKLIST: OnceLock<Arc<Mutex<V3DirectionBlocklist>>> = OnceLock::new();

impl ExactQuoter {
    pub fn new(settings: Arc<crate::config::Settings>, rpc: Arc<RpcClients>) -> Self {
        Self::with_v3_rpc_quoter(
            settings.clone(),
            rpc,
            std::env::var("USE_V3_RPC_QUOTER")
                .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
                .unwrap_or(false),
        )
    }

    pub fn with_v3_rpc_quoter(
        settings: Arc<crate::config::Settings>,
        rpc: Arc<RpcClients>,
        use_v3_rpc_quoter: bool,
    ) -> Self {
        Self {
            rpc,
            quote_cache: Arc::new(Mutex::new(QuoteCache::from_env())),
            v3_revert_cache: Arc::new(Mutex::new(V3RevertCache::from_env())),
            v3_direction_blocklist: shared_v3_direction_blocklist(),
            curve_underlying_support: Arc::new(Mutex::new(HashMap::new())),
            use_v3_rpc_quoter,
            use_lb_rpc_quoter: use_v3_rpc_quoter
                || std::env::var("USE_LB_RPC_QUOTER")
                    .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
                    .unwrap_or(false),
            use_curve_rpc_quoter: std::env::var("USE_CURVE_RPC_QUOTER")
                .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
                .unwrap_or(false),
            use_balancer_rpc_quoter: std::env::var("USE_BALANCER_RPC_QUOTER")
                .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
                .unwrap_or(true),
            quote_rpc_timeout: Duration::from_millis(
                env_usize("EXACT_QUOTE_TIMEOUT_MS", 1_000) as u64
            ),
            chain_label: settings.chain.as_str().to_string(),
        }
    }

    pub async fn quote_pool(
        &self,
        snapshot: &GraphSnapshot,
        pool: &PoolState,
        token_in: alloy::primitives::Address,
        token_out: alloy::primitives::Address,
        amount_in: u128,
    ) -> Result<u128> {
        let cache_key = QuoteKey {
            snapshot_id: snapshot.snapshot_id,
            pool_id: pool.pool_id,
            token_in,
            token_out,
            amount_in,
        };
        if let Some(cached) = self.quote_cache.lock().get(&cache_key) {
            telemetry::record_quote_cache_hit(&pool.pool_id.to_string(), &self.chain_label);
            return Ok(cached);
        }
        telemetry::record_quote_cache_miss(&pool.pool_id.to_string(), &self.chain_label);

        let Some(i) = pool
            .token_addresses
            .iter()
            .position(|token| *token == token_in)
        else {
            self.insert_quote(cache_key, 0);
            return Ok(0);
        };
        let Some(j) = pool
            .token_addresses
            .iter()
            .position(|token| *token == token_out)
        else {
            self.insert_quote(cache_key, 0);
            return Ok(0);
        };
        if i == j {
            self.insert_quote(cache_key, 0);
            return Ok(0);
        }

        let out = match &pool.state {
            PoolSpecificState::UniswapV2Like(state) => {
                if pool.token_addresses.len() != 2 {
                    return Ok(0);
                }
                let zero_for_one = i == 0 && j == 1;
                amm::uniswap_v2::quote_exact_in(state, zero_for_one, amount_in).unwrap_or(0)
            }
            PoolSpecificState::AerodromeV2Like(state) => {
                if pool.token_addresses.len() != 2 {
                    return Ok(0);
                }
                let zero_for_one = i == 0 && j == 1;
                amm::aerodrome_v2::quote_exact_in(state, zero_for_one, amount_in).unwrap_or(0)
            }
            PoolSpecificState::UniswapV3Like(state) => {
                if pool.token_addresses.len() != 2 {
                    return Ok(0);
                }
                let pool_direction_key = PoolDirectionKey {
                    pool_id: pool.pool_id,
                    token_in,
                    token_out,
                };
                if self
                    .v3_direction_blocklist
                    .lock()
                    .blocked(&pool_direction_key)
                {
                    self.insert_quote(cache_key, 0);
                    return Ok(0);
                }
                if self.use_v3_rpc_quoter {
                    let direction_key = V3DirectionKey {
                        snapshot_id: snapshot.snapshot_id,
                        pool_id: pool.pool_id,
                        token_in,
                        token_out,
                    };
                    if self.v3_revert_cache.lock().blocked(&direction_key) {
                        0
                    } else if let Some(quoter) = pool.quoter {
                        let zero_for_one = i == 0 && j == 1;
                        let quoted = if is_slipstream_pool(pool) {
                            self.quote_slipstream_v3(
                                quoter,
                                token_in,
                                token_out,
                                amount_in,
                                state.tick_spacing,
                                state.sqrt_price_x96,
                                zero_for_one,
                            )
                            .await
                        } else {
                            self.quote_uniswap_v3(
                                quoter,
                                token_in,
                                token_out,
                                amount_in,
                                state.fee,
                                state.sqrt_price_x96,
                                zero_for_one,
                            )
                            .await
                        };
                        match quoted {
                            Ok(amount) => {
                                if amount > 0 {
                                    self.v3_revert_cache.lock().clear(&direction_key);
                                    self.v3_direction_blocklist
                                        .lock()
                                        .clear(&pool_direction_key);
                                } else {
                                    self.v3_direction_blocklist
                                        .lock()
                                        .record_failure(pool_direction_key);
                                }
                                amount
                            }
                            Err(err) if is_expected_quote_revert(&err) => {
                                self.v3_revert_cache.lock().record_failure(direction_key);
                                self.v3_direction_blocklist
                                    .lock()
                                    .record_failure(pool_direction_key);
                                0
                            }
                            Err(err) => return Err(err),
                        }
                    } else {
                        let zero_for_one = i == 0 && j == 1;
                        amm::uniswap_v3::fallback_quote(state, zero_for_one, amount_in)
                    }
                } else {
                    let zero_for_one = i == 0 && j == 1;
                    amm::uniswap_v3::fallback_quote(state, zero_for_one, amount_in)
                }
            }
            PoolSpecificState::TraderJoeLb(state) => {
                if pool.token_addresses.len() != 2 {
                    return Ok(0);
                }
                let swap_for_y = i == 0 && j == 1;
                if self.use_lb_rpc_quoter {
                    let Some(raw) = self
                        .eth_call_with_timeout(
                            pool.pool_id,
                            ITraderJoeLBPair::getSwapOutCall {
                                amountIn: amount_in,
                                swapForY: swap_for_y,
                            }
                            .abi_encode()
                            .into(),
                        )
                        .await?
                    else {
                        return Ok(0);
                    };
                    let ret = ITraderJoeLBPair::getSwapOutCall::abi_decode_returns(&raw)?;
                    if ret.amountInLeft > 0 {
                        0
                    } else {
                        ret.amountOut
                    }
                } else {
                    amm::trader_joe_lb::quote_exact_in(state, swap_for_y, amount_in).unwrap_or(0)
                }
            }
            PoolSpecificState::CurvePlain(state) => {
                if !self.use_curve_rpc_quoter {
                    let amount = amm::curve::fallback_quote(state, i, j, amount_in);
                    self.insert_quote(cache_key, amount);
                    return Ok(amount);
                }
                let try_underlying = self
                    .curve_underlying_support
                    .lock()
                    .get(&pool.pool_id)
                    .copied()
                    .unwrap_or(state.supports_underlying);
                let amount = if try_underlying {
                    let raw = self
                        .eth_call_with_timeout(
                            pool.pool_id,
                            ICurvePool::get_dy_underlyingCall {
                                i: i as i128,
                                j: j as i128,
                                dx: U256::saturating_from(amount_in),
                            }
                            .abi_encode()
                            .into(),
                        )
                        .await;
                    match raw {
                        Ok(Some(raw)) => {
                            let ret = ICurvePool::get_dy_underlyingCall::abi_decode_returns(&raw)?;
                            u256_to_u128(ret)
                        }
                        Ok(None) => 0,
                        Err(_) => {
                            self.curve_underlying_support
                                .lock()
                                .insert(pool.pool_id, false);
                            let Some(raw) = self
                                .eth_call_with_timeout(
                                    pool.pool_id,
                                    ICurvePool::get_dyCall {
                                        i: i as i128,
                                        j: j as i128,
                                        dx: U256::saturating_from(amount_in),
                                    }
                                    .abi_encode()
                                    .into(),
                                )
                                .await?
                            else {
                                return Ok(0);
                            };
                            let ret = ICurvePool::get_dyCall::abi_decode_returns(&raw)?;
                            u256_to_u128(ret)
                        }
                    }
                } else {
                    let Some(raw) = self
                        .eth_call_with_timeout(
                            pool.pool_id,
                            ICurvePool::get_dyCall {
                                i: i as i128,
                                j: j as i128,
                                dx: U256::saturating_from(amount_in),
                            }
                            .abi_encode()
                            .into(),
                        )
                        .await?
                    else {
                        return Ok(0);
                    };
                    let ret = ICurvePool::get_dyCall::abi_decode_returns(&raw)?;
                    u256_to_u128(ret)
                };
                amount
            }
            PoolSpecificState::BalancerWeighted(state) => {
                if self.use_balancer_rpc_quoter {
                    if let Some(vault) = pool.vault {
                        match self
                            .quote_balancer_vault(
                                vault,
                                state.pool_id,
                                token_in,
                                token_out,
                                amount_in,
                            )
                            .await
                        {
                            Ok(amount) => amount,
                            Err(err) if is_expected_quote_revert(&err) => 0,
                            Err(err) => return Err(err),
                        }
                    } else {
                        amm::balancer::fallback_quote(state, i, j, amount_in)
                    }
                } else {
                    amm::balancer::fallback_quote(state, i, j, amount_in)
                }
            }
        };

        self.insert_quote(cache_key, out);
        Ok(out)
    }

    async fn quote_uniswap_v3(
        &self,
        quoter: alloy::primitives::Address,
        token_in: alloy::primitives::Address,
        token_out: alloy::primitives::Address,
        amount_in: u128,
        fee: u32,
        sqrt_price_x96: U256,
        zero_for_one: bool,
    ) -> Result<u128> {
        let Some(raw) = self
            .eth_call_with_timeout(
                quoter,
                IV3QuoterV2::quoteExactInputSingleCall {
                    params: IV3QuoterV2::QuoteExactInputSingleParams {
                        tokenIn: token_in,
                        tokenOut: token_out,
                        amountIn: U256::saturating_from(amount_in),
                        fee: Uint::<24, 1>::saturating_from(fee),
                        sqrtPriceLimitX96: sqrt_price_limit_u160(sqrt_price_x96, zero_for_one),
                    },
                }
                .abi_encode()
                .into(),
            )
            .await?
        else {
            return Ok(0);
        };
        let ret = IV3QuoterV2::quoteExactInputSingleCall::abi_decode_returns(&raw)?;
        Ok(u256_to_u128(ret.amountOut))
    }

    async fn quote_slipstream_v3(
        &self,
        quoter: alloy::primitives::Address,
        token_in: alloy::primitives::Address,
        token_out: alloy::primitives::Address,
        amount_in: u128,
        tick_spacing: i32,
        sqrt_price_x96: U256,
        zero_for_one: bool,
    ) -> Result<u128> {
        let Ok(tick_spacing) = I24::try_from(tick_spacing) else {
            return Ok(0);
        };
        let Some(raw) = self
            .eth_call_with_timeout(
                quoter,
                IAerodromeCLQuoter::quoteExactInputSingleCall {
                    params: IAerodromeCLQuoter::QuoteExactInputSingleParams {
                        tokenIn: token_in,
                        tokenOut: token_out,
                        amountIn: U256::saturating_from(amount_in),
                        tickSpacing: tick_spacing,
                        sqrtPriceLimitX96: sqrt_price_limit_u160(sqrt_price_x96, zero_for_one),
                    },
                }
                .abi_encode()
                .into(),
            )
            .await?
        else {
            return Ok(0);
        };
        let ret = IAerodromeCLQuoter::quoteExactInputSingleCall::abi_decode_returns(&raw)?;
        Ok(u256_to_u128(ret.amountOut))
    }

    async fn quote_balancer_vault(
        &self,
        vault: Address,
        pool_id: alloy::primitives::B256,
        token_in: Address,
        token_out: Address,
        amount_in: u128,
    ) -> Result<u128> {
        let Some(raw) = self
            .eth_call_with_timeout(
                vault,
                IBalancerVault::queryBatchSwapCall {
                    kind: 0,
                    swaps: vec![IBalancerVault::BatchSwapStep {
                        poolId: pool_id,
                        assetInIndex: U256::ZERO,
                        assetOutIndex: U256::from(1u8),
                        amount: U256::saturating_from(amount_in),
                        userData: Bytes::new(),
                    }],
                    assets: vec![token_in, token_out],
                    funds: IBalancerVault::FundManagement {
                        sender: Address::ZERO,
                        fromInternalBalance: false,
                        recipient: Address::ZERO,
                        toInternalBalance: false,
                    },
                }
                .abi_encode()
                .into(),
            )
            .await?
        else {
            return Ok(0);
        };
        let ret = IBalancerVault::queryBatchSwapCall::abi_decode_returns(&raw)?;
        let Some(delta_out) = ret.get(1) else {
            return Ok(0);
        };
        Ok(negative_signed_abs_to_u128(delta_out))
    }

    async fn eth_call_with_timeout(&self, to: Address, data: Bytes) -> Result<Option<Bytes>> {
        match timeout(
            self.quote_rpc_timeout,
            self.rpc.best_read().eth_call(to, None, data, "latest"),
        )
        .await
        {
            Ok(raw) => raw.map(Some),
            Err(_) => Ok(None),
        }
    }

    fn insert_quote(&self, key: QuoteKey, value: u128) {
        self.quote_cache.lock().insert(key, value);
    }
}

#[derive(Debug)]
struct QuoteCache {
    max_segments: usize,
    max_entries_per_segment: usize,
    segments: HashMap<u64, QuoteSegment>,
}

#[derive(Debug, Default)]
struct QuoteSegment {
    entries: HashMap<QuoteKey, u128>,
    lru: VecDeque<QuoteKey>,
}

impl QuoteCache {
    fn from_env() -> Self {
        Self {
            max_segments: env_usize("QUOTE_CACHE_MAX_SEGMENTS", 4),
            max_entries_per_segment: env_usize("QUOTE_CACHE_MAX_ENTRIES_PER_SEGMENT", 20_000),
            segments: HashMap::new(),
        }
    }

    fn get(&mut self, key: &QuoteKey) -> Option<u128> {
        let segment = self.segments.get_mut(&key.snapshot_id)?;
        let value = segment.entries.get(key).copied()?;
        segment.touch(*key);
        Some(value)
    }

    fn insert(&mut self, key: QuoteKey, value: u128) {
        if !self.segments.contains_key(&key.snapshot_id) {
            self.evict_old_segments_for_new_segment();
        }
        let segment = self.segments.entry(key.snapshot_id).or_default();
        if !segment.entries.contains_key(&key) {
            segment.lru.push_back(key);
        } else {
            segment.touch(key);
        }
        segment.entries.insert(key, value);
        while segment.entries.len() > self.max_entries_per_segment {
            if let Some(oldest) = segment.lru.pop_front() {
                segment.entries.remove(&oldest);
            } else {
                break;
            }
        }
    }

    fn evict_old_segments_for_new_segment(&mut self) {
        while self.segments.len() >= self.max_segments {
            let Some(oldest) = self.segments.keys().min().copied() else {
                break;
            };
            self.segments.remove(&oldest);
        }
    }
}

impl QuoteSegment {
    fn touch(&mut self, key: QuoteKey) {
        self.lru.retain(|existing| *existing != key);
        self.lru.push_back(key);
    }
}

#[derive(Debug)]
struct V3DirectionBlocklist {
    ttl: Duration,
    entries: HashMap<PoolDirectionKey, Instant>,
}

impl V3DirectionBlocklist {
    fn from_env() -> Self {
        Self {
            ttl: Duration::from_secs(env_usize("V3_DIRECTION_BLOCKLIST_SECS", 180) as u64),
            entries: HashMap::new(),
        }
    }

    fn blocked(&mut self, key: &PoolDirectionKey) -> bool {
        self.prune();
        self.entries.contains_key(key)
    }

    fn record_failure(&mut self, key: PoolDirectionKey) {
        if self.ttl.is_zero() {
            return;
        }
        self.prune();
        self.entries.insert(key, Instant::now() + self.ttl);
    }

    fn clear(&mut self, key: &PoolDirectionKey) {
        self.entries.remove(key);
    }

    fn prune(&mut self) {
        let now = Instant::now();
        self.entries.retain(|_, expires_at| *expires_at > now);
    }
}

fn shared_v3_direction_blocklist() -> Arc<Mutex<V3DirectionBlocklist>> {
    V3_DIRECTION_BLOCKLIST
        .get_or_init(|| Arc::new(Mutex::new(V3DirectionBlocklist::from_env())))
        .clone()
}

#[derive(Debug)]
struct V3RevertCache {
    threshold: u32,
    max_segments: usize,
    failures: HashMap<u64, HashMap<V3DirectionKey, u32>>,
}

impl V3RevertCache {
    fn from_env() -> Self {
        Self {
            threshold: env_usize("V3_DIRECTION_REVERT_CACHE_THRESHOLD", 2) as u32,
            max_segments: env_usize("QUOTE_CACHE_MAX_SEGMENTS", 4),
            failures: HashMap::new(),
        }
    }

    fn blocked(&mut self, key: &V3DirectionKey) -> bool {
        self.failures
            .get(&key.snapshot_id)
            .and_then(|segment| segment.get(key))
            .copied()
            .unwrap_or(0)
            >= self.threshold
    }

    fn record_failure(&mut self, key: V3DirectionKey) {
        if !self.failures.contains_key(&key.snapshot_id) {
            self.evict_old_segments_for_new_segment();
        }
        let segment = self.failures.entry(key.snapshot_id).or_default();
        let count = segment.entry(key).or_default();
        *count = count.saturating_add(1);
    }

    fn clear(&mut self, key: &V3DirectionKey) {
        if let Some(segment) = self.failures.get_mut(&key.snapshot_id) {
            segment.remove(key);
        }
    }

    fn evict_old_segments_for_new_segment(&mut self) {
        while self.failures.len() >= self.max_segments {
            let Some(oldest) = self.failures.keys().min().copied() else {
                break;
            };
            self.failures.remove(&oldest);
        }
    }
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn u256_to_u128(value: alloy::primitives::U256) -> u128 {
    value.to_string().parse::<u128>().unwrap_or(u128::MAX)
}

fn negative_signed_abs_to_u128(value: &impl ToString) -> u128 {
    let text = value.to_string();
    let Some(raw) = text.strip_prefix('-') else {
        return 0;
    };
    raw.parse::<u128>().unwrap_or(u128::MAX)
}

fn is_slipstream_pool(pool: &PoolState) -> bool {
    pool.dex_name.starts_with("aerodrome_v3")
}

fn is_expected_quote_revert(err: &anyhow::Error) -> bool {
    let message = err.to_string().to_ascii_lowercase();
    message.starts_with("rpc error")
        && (message.contains("revert")
            || message.contains("unexpected error")
            || message.contains("too little received"))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use alloy::primitives::Address;

    use super::{QuoteCache, QuoteKey, V3DirectionKey, V3RevertCache};

    fn key(snapshot_id: u64, amount_in: u128) -> QuoteKey {
        QuoteKey {
            snapshot_id,
            pool_id: Address::repeat_byte(1),
            token_in: Address::repeat_byte(2),
            token_out: Address::repeat_byte(3),
            amount_in,
        }
    }

    #[test]
    fn quote_cache_evicts_per_snapshot_without_full_flush() {
        let mut cache = QuoteCache {
            max_segments: 2,
            max_entries_per_segment: 2,
            segments: HashMap::new(),
        };

        cache.insert(key(1, 1), 10);
        cache.insert(key(1, 2), 20);
        cache.insert(key(1, 3), 30);
        assert!(cache.get(&key(1, 1)).is_none());
        assert_eq!(cache.get(&key(1, 2)), Some(20));
        assert_eq!(cache.get(&key(1, 3)), Some(30));

        cache.insert(key(2, 1), 40);
        cache.insert(key(3, 1), 50);
        assert!(cache.segments.contains_key(&2));
        assert!(cache.segments.contains_key(&3));
        assert!(!cache.segments.contains_key(&1));
    }

    #[test]
    fn quote_cache_does_not_evict_segments_when_updating_existing_segment_at_capacity() {
        let mut cache = QuoteCache {
            max_segments: 2,
            max_entries_per_segment: 4,
            segments: HashMap::new(),
        };

        cache.insert(key(1, 1), 10);
        cache.insert(key(2, 1), 20);
        cache.insert(key(2, 2), 30);

        assert_eq!(cache.get(&key(1, 1)), Some(10));
        assert_eq!(cache.get(&key(2, 1)), Some(20));
        assert_eq!(cache.get(&key(2, 2)), Some(30));
        assert_eq!(cache.segments.len(), 2);
    }

    #[test]
    fn v3_revert_cache_blocks_after_threshold_and_clears_on_success() {
        let direction = V3DirectionKey {
            snapshot_id: 1,
            pool_id: Address::repeat_byte(1),
            token_in: Address::repeat_byte(2),
            token_out: Address::repeat_byte(3),
        };
        let mut cache = V3RevertCache {
            threshold: 2,
            max_segments: 2,
            failures: HashMap::new(),
        };

        cache.record_failure(direction);
        assert!(!cache.blocked(&direction));
        cache.record_failure(direction);
        assert!(cache.blocked(&direction));
        cache.clear(&direction);
        assert!(!cache.blocked(&direction));
    }
}
