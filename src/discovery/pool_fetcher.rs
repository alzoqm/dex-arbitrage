use std::{collections::HashMap, sync::Arc, time::SystemTime};

use alloy::{
    primitives::{Address, U256},
    sol_types::SolCall,
};
use anyhow::{Context, Result};

use crate::{
    abi::{IBalancerPool, IBalancerVault, ICurvePool, IUniswapV2Pair, IUniswapV3Pool},
    config::{Settings, TokenConfig},
    rpc::RpcClients,
    types::{
        AmmKind, BalancerPoolState, CurvePoolState, PoolAdmissionStatus, PoolHealth,
        PoolSpecificState, PoolState, V2PoolState, V3PoolState,
    },
};

use super::{factory_scanner::DiscoveredPool, multicall};

#[derive(Debug)]
pub struct PoolFetcher {
    settings: Arc<Settings>,
    rpc: Arc<RpcClients>,
    token_map: HashMap<Address, TokenConfig>,
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
            AmmKind::UniswapV3Like => self.fetch_v3(spec, block_number).await,
            AmmKind::CurvePlain => self.fetch_curve(spec, block_number).await,
            AmmKind::BalancerWeighted => self.fetch_balancer(spec, block_number).await,
        }
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
        let fee_ppm = self
            .settings
            .dexes
            .iter()
            .find(|dex| dex.name == spec.dex_name)
            .map(|dex| dex.fee_ppm)
            .unwrap_or(3_000);
        Ok(Some(PoolState {
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
        }))
    }

    async fn fetch_v3(
        &self,
        spec: &DiscoveredPool,
        block_number: Option<u64>,
    ) -> Result<Option<PoolState>> {
        let (token0, token1, fee, tick_spacing, liquidity, sqrt_price_x96, tick) =
            match self.fetch_v3_multicall(spec.address).await {
                Ok(values) => values,
                Err(_) => self.fetch_v3_sequential(spec.address).await?,
            };

        Ok(Some(PoolState {
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
        }))
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
                supports_underlying: true,
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
        let slot0 = IUniswapV3Pool::slot0Call::abi_decode_returns(
            results
                .get(5)
                .and_then(|value| value.as_ref())
                .context("multicall v3 slot0 failed")?,
        )?;
        Ok((
            token0,
            token1,
            u32::try_from(fee).unwrap_or(u32::MAX),
            i32::try_from(tick_spacing).unwrap_or(0),
            liquidity,
            U256::saturating_from(slot0.sqrtPriceX96),
            i32::try_from(slot0.tick).unwrap_or(0),
        ))
    }

    async fn fetch_v3_sequential(
        &self,
        pool: Address,
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
        let slot0 = IUniswapV3Pool::slot0Call::abi_decode_returns(&raw_slot0)?;
        Ok((
            token0,
            token1,
            u32::try_from(fee).unwrap_or(u32::MAX),
            i32::try_from(tick_spacing).unwrap_or(0),
            liquidity,
            U256::saturating_from(slot0.sqrtPriceX96),
            i32::try_from(slot0.tick).unwrap_or(0),
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
