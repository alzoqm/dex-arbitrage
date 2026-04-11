use std::sync::Arc;

use alloy::sol_types::SolCall;
use anyhow::Result;

use crate::{
    abi::{ICurvePool, IV3QuoterV2},
    amm,
    config::Settings,
    graph::GraphSnapshot,
    rpc::RpcClients,
    types::{PoolSpecificState, PoolState},
};

#[derive(Debug, Clone)]
pub struct ExactQuoter {
    settings: Arc<Settings>,
    rpc: Arc<RpcClients>,
}

impl ExactQuoter {
    pub fn new(settings: Arc<Settings>, rpc: Arc<RpcClients>) -> Self {
        Self { settings, rpc }
    }

    pub async fn quote_pool(
        &self,
        snapshot: &GraphSnapshot,
        pool: &PoolState,
        token_in: alloy::primitives::Address,
        token_out: alloy::primitives::Address,
        amount_in: u128,
    ) -> Result<u128> {
        let i = pool
            .token_addresses
            .iter()
            .position(|token| *token == token_in)
            .unwrap_or(0);
        let j = pool
            .token_addresses
            .iter()
            .position(|token| *token == token_out)
            .unwrap_or(0);

        let out = match &pool.state {
            PoolSpecificState::UniswapV2Like(state) => {
                let zero_for_one = pool.token_addresses.first().copied() == Some(token_in);
                amm::uniswap_v2::quote_exact_in(state, zero_for_one, amount_in).unwrap_or(0)
            }
            PoolSpecificState::UniswapV3Like(state) => {
                if let Some(quoter) = pool.quoter {
                    let raw = self
                        .rpc
                        .best_read()
                        .eth_call(
                            quoter,
                            None,
                            IV3QuoterV2::quoteExactInputSingleCall {
                                tokenIn: token_in,
                                tokenOut: token_out,
                                fee: state.fee,
                                amountIn: amount_in.into(),
                                sqrtPriceLimitX96: 0,
                            }
                            .abi_encode()
                            .into(),
                            "latest",
                        )
                        .await?;
                    let ret = IV3QuoterV2::quoteExactInputSingleCall::abi_decode_returns(&raw, true)?;
                    u256_to_u128(ret.amountOut)
                } else {
                    let zero_for_one = pool.token_addresses.first().copied() == Some(token_in);
                    amm::uniswap_v3::fallback_quote(state, zero_for_one, amount_in)
                }
            }
            PoolSpecificState::CurvePlain(state) => {
                let try_underlying = state.supports_underlying;
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
                                dx: amount_in.into(),
                            }
                            .abi_encode()
                            .into(),
                            "latest",
                        )
                        .await;
                    match raw {
                        Ok(raw) => {
                            let ret = ICurvePool::get_dy_underlyingCall::abi_decode_returns(&raw, true)?;
                            u256_to_u128(ret._0)
                        }
                        Err(_) => {
                            let raw = self
                                .rpc
                                .best_read()
                                .eth_call(
                                    pool.pool_id,
                                    None,
                                    ICurvePool::get_dyCall {
                                        i: i as i128,
                                        j: j as i128,
                                        dx: amount_in.into(),
                                    }
                                    .abi_encode()
                                    .into(),
                                    "latest",
                                )
                                .await?;
                            let ret = ICurvePool::get_dyCall::abi_decode_returns(&raw, true)?;
                            u256_to_u128(ret._0)
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
                                dx: amount_in.into(),
                            }
                            .abi_encode()
                            .into(),
                            "latest",
                        )
                        .await?;
                    let ret = ICurvePool::get_dyCall::abi_decode_returns(&raw, true)?;
                    u256_to_u128(ret._0)
                };
                amount
            }
            PoolSpecificState::BalancerWeighted(state) => amm::balancer::fallback_quote(state, i, j, amount_in),
        };

        let _ = snapshot.snapshot_id;
        Ok(out)
    }
}

fn u256_to_u128(value: alloy::primitives::U256) -> u128 {
    value.to_string().parse::<u128>().unwrap_or(u128::MAX)
}
