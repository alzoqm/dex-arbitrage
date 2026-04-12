use std::{collections::HashMap, sync::Arc};

use alloy::{
    primitives::{Uint, U160, U256},
    sol_types::SolCall,
};
use anyhow::Result;
use parking_lot::Mutex;

use crate::{
    abi::{ICurvePool, IV3QuoterV2},
    amm,
    graph::GraphSnapshot,
    rpc::RpcClients,
    types::{PoolSpecificState, PoolState},
};

#[derive(Debug, Clone)]
pub struct ExactQuoter {
    rpc: Arc<RpcClients>,
    quote_cache: Arc<Mutex<HashMap<QuoteKey, u128>>>,
    curve_underlying_support: Arc<Mutex<HashMap<alloy::primitives::Address, bool>>>,
    use_v3_rpc_quoter: bool,
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
    pub fn new(_settings: Arc<crate::config::Settings>, rpc: Arc<RpcClients>) -> Self {
        Self {
            rpc,
            quote_cache: Arc::new(Mutex::new(HashMap::new())),
            curve_underlying_support: Arc::new(Mutex::new(HashMap::new())),
            use_v3_rpc_quoter: std::env::var("USE_V3_RPC_QUOTER")
                .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
                .unwrap_or(true),
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
        if let Some(cached) = self.quote_cache.lock().get(&cache_key).copied() {
            return Ok(cached);
        }

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
            PoolSpecificState::UniswapV3Like(state) => {
                if pool.token_addresses.len() != 2 {
                    return Ok(0);
                }
                if self.use_v3_rpc_quoter {
                    if let Some(quoter) = pool.quoter {
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
                                        fee: Uint::<24, 1>::saturating_from(state.fee),
                                        sqrtPriceLimitX96: U160::ZERO,
                                    },
                                }
                                .abi_encode()
                                .into(),
                                "latest",
                            )
                            .await?;
                        let ret = IV3QuoterV2::quoteExactInputSingleCall::abi_decode_returns(&raw)?;
                        u256_to_u128(ret.amountOut)
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

    fn insert_quote(&self, key: QuoteKey, value: u128) {
        let mut cache = self.quote_cache.lock();
        if cache.len() > 20_000 {
            cache.clear();
        }
        cache.insert(key, value);
    }
}

fn u256_to_u128(value: alloy::primitives::U256) -> u128 {
    value.to_string().parse::<u128>().unwrap_or(u128::MAX)
}
