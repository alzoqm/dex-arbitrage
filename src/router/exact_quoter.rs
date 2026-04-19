use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

use alloy::{
    primitives::{aliases::I24, Uint, U160, U256},
    sol_types::SolCall,
};
use anyhow::Result;
use parking_lot::Mutex;

use crate::{
    abi::{IAerodromeCLQuoter, ICurvePool, IV3QuoterV2},
    amm,
    graph::GraphSnapshot,
    monitoring::metrics as telemetry,
    rpc::RpcClients,
    types::{PoolSpecificState, PoolState},
};

#[derive(Debug, Clone)]
pub struct ExactQuoter {
    rpc: Arc<RpcClients>,
    quote_cache: Arc<Mutex<QuoteCache>>,
    curve_underlying_support: Arc<Mutex<HashMap<alloy::primitives::Address, bool>>>,
    use_v3_rpc_quoter: bool,
    use_curve_rpc_quoter: bool,
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
            curve_underlying_support: Arc::new(Mutex::new(HashMap::new())),
            use_v3_rpc_quoter,
            use_curve_rpc_quoter: std::env::var("USE_CURVE_RPC_QUOTER")
                .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
                .unwrap_or(false),
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
                if self.use_v3_rpc_quoter {
                    if let Some(quoter) = pool.quoter {
                        let zero_for_one = i == 0 && j == 1;
                        if is_slipstream_pool(pool) {
                            match self
                                .quote_slipstream_v3(
                                    quoter,
                                    token_in,
                                    token_out,
                                    amount_in,
                                    state.tick_spacing,
                                    state.sqrt_price_x96,
                                    zero_for_one,
                                )
                                .await
                            {
                                Ok(amount) => amount,
                                Err(err) if is_expected_quote_revert(&err) => 0,
                                Err(err) => return Err(err),
                            }
                        } else {
                            match self
                                .quote_uniswap_v3(
                                    quoter,
                                    token_in,
                                    token_out,
                                    amount_in,
                                    state.fee,
                                    state.sqrt_price_x96,
                                    zero_for_one,
                                )
                                .await
                            {
                                Ok(amount) => amount,
                                Err(err) if is_expected_quote_revert(&err) => 0,
                                Err(err) => return Err(err),
                            }
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
                        .rpc
                        .best_read()
                        .eth_call(
                            pool.pool_id,
                            None,
                            ICurvePool::get_dy_underlyingCall {
                                i: i as i128,
                                j: j as i128,
                                dx: U256::saturating_from(amount_in),
                            }
                            .abi_encode()
                            .into(),
                            "latest",
                        )
                        .await;
                    match raw {
                        Ok(raw) => {
                            let ret = ICurvePool::get_dy_underlyingCall::abi_decode_returns(&raw)?;
                            u256_to_u128(ret)
                        }
                        Err(_) => {
                            self.curve_underlying_support
                                .lock()
                                .insert(pool.pool_id, false);
                            let raw = self
                                .rpc
                                .best_read()
                                .eth_call(
                                    pool.pool_id,
                                    None,
                                    ICurvePool::get_dyCall {
                                        i: i as i128,
                                        j: j as i128,
                                        dx: U256::saturating_from(amount_in),
                                    }
                                    .abi_encode()
                                    .into(),
                                    "latest",
                                )
                                .await?;
                            let ret = ICurvePool::get_dyCall::abi_decode_returns(&raw)?;
                            u256_to_u128(ret)
                        }
                    }
                } else {
                    let raw = self
                        .rpc
                        .best_read()
                        .eth_call(
                            pool.pool_id,
                            None,
                            ICurvePool::get_dyCall {
                                i: i as i128,
                                j: j as i128,
                                dx: U256::saturating_from(amount_in),
                            }
                            .abi_encode()
                            .into(),
                            "latest",
                        )
                        .await?;
                    let ret = ICurvePool::get_dyCall::abi_decode_returns(&raw)?;
                    u256_to_u128(ret)
                };
                amount
            }
            PoolSpecificState::BalancerWeighted(state) => {
                amm::balancer::fallback_quote(state, i, j, amount_in)
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
        let raw = self
            .rpc
            .best_read()
            .eth_call(
                quoter,
                None,
                IV3QuoterV2::quoteExactInputSingleCall {
                    params: IV3QuoterV2::QuoteExactInputSingleParams {
                        tokenIn: token_in,
                        tokenOut: token_out,
                        amountIn: U256::saturating_from(amount_in),
                        fee: Uint::<24, 1>::saturating_from(fee),
                        sqrtPriceLimitX96: v3_sqrt_price_limit_u160(sqrt_price_x96, zero_for_one),
                    },
                }
                .abi_encode()
                .into(),
                "latest",
            )
            .await?;
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
        let raw = self
            .rpc
            .best_read()
            .eth_call(
                quoter,
                None,
                IAerodromeCLQuoter::quoteExactInputSingleCall {
                    params: IAerodromeCLQuoter::QuoteExactInputSingleParams {
                        tokenIn: token_in,
                        tokenOut: token_out,
                        amountIn: U256::saturating_from(amount_in),
                        tickSpacing: tick_spacing,
                        sqrtPriceLimitX96: v3_sqrt_price_limit_u160(sqrt_price_x96, zero_for_one),
                    },
                }
                .abi_encode()
                .into(),
                "latest",
            )
            .await?;
        let ret = IAerodromeCLQuoter::quoteExactInputSingleCall::abi_decode_returns(&raw)?;
        Ok(u256_to_u128(ret.amountOut))
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

fn v3_sqrt_price_limit_u160(current: U256, zero_for_one: bool) -> U160 {
    let bps = std::env::var("V3_SQRT_PRICE_LIMIT_BPS")
        .ok()
        .and_then(|value| value.parse::<u128>().ok())
        .filter(|value| *value < 10_000)
        .unwrap_or(50);
    if bps == 0 || current.is_zero() {
        return U160::ZERO;
    }
    let denominator = U256::from(10_000u128);
    let numerator = if zero_for_one {
        U256::from(10_000u128.saturating_sub(bps))
    } else {
        U256::from(10_000u128.saturating_add(bps))
    };
    u160_saturating_from_u256(current.saturating_mul(numerator) / denominator)
}

fn u160_saturating_from_u256(value: U256) -> U160 {
    let bytes = value.to_be_bytes::<32>();
    if bytes[..12].iter().any(|byte| *byte != 0) {
        return U160::from_be_slice(&[0xff; 20]);
    }
    U160::from_be_slice(&bytes[12..])
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

    use alloy::primitives::{Address, U160, U256};

    use super::{v3_sqrt_price_limit_u160, QuoteCache, QuoteKey};

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
    fn v3_quoter_price_limit_matches_execution_default_bps() {
        let current = U256::from(1_000_000u64);

        assert_eq!(
            v3_sqrt_price_limit_u160(current, true),
            U160::from(995_000u64)
        );
        assert_eq!(
            v3_sqrt_price_limit_u160(current, false),
            U160::from(1_005_000u64)
        );
    }
}
