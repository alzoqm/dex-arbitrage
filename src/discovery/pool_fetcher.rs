use std::{collections::HashMap, sync::Arc, time::SystemTime};

use alloy::{
    primitives::{Address, U256},
    sol_types::SolCall,
};
use anyhow::Result;

use crate::{
    abi::{IBalancerPool, IBalancerVault, ICurvePool, IUniswapV2Pair, IUniswapV3Pool},
    config::{Settings, TokenConfig},
    rpc::RpcClients,
    types::{
        AmmKind, BalancerPoolState, CurvePoolState, PoolAdmissionStatus, PoolHealth,
        PoolSpecificState, PoolState, V2PoolState, V3PoolState,
    },
};

use super::factory_scanner::DiscoveredPool;

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

    pub async fn fetch_pool(&self, spec: &DiscoveredPool) -> Result<Option<PoolState>> {
        match spec.amm_kind {
            AmmKind::UniswapV2Like => self.fetch_v2(spec).await,
            AmmKind::UniswapV3Like => self.fetch_v3(spec).await,
            AmmKind::CurvePlain => self.fetch_curve(spec).await,
            AmmKind::BalancerWeighted => self.fetch_balancer(spec).await,
        }
    }

    async fn fetch_v2(&self, spec: &DiscoveredPool) -> Result<Option<PoolState>> {
        let token0 = self
            .call_address(spec.address, IUniswapV2Pair::token0Call {}.abi_encode())
            .await?;
        let token1 = self
            .call_address(spec.address, IUniswapV2Pair::token1Call {}.abi_encode())
            .await?;
        if !self.token_map.contains_key(&token0) || !self.token_map.contains_key(&token1) {
            return Ok(None);
        }
        let raw = self
            .rpc
            .best_read()
            .eth_call(
                spec.address,
                None,
                IUniswapV2Pair::getReservesCall {}.abi_encode().into(),
                "latest",
            )
            .await?;
        let ret = IUniswapV2Pair::getReservesCall::abi_decode_returns(&raw)?;
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
            token_symbols: vec![
                self.token_map[&token0].symbol.clone(),
                self.token_map[&token1].symbol.clone(),
            ],
            factory: spec.factory,
            registry: spec.registry,
            vault: spec.vault,
            quoter: spec.quoter,
            admission_status: PoolAdmissionStatus::Allowed,
            health: fresh_health(10_000, false),
            state: PoolSpecificState::UniswapV2Like(V2PoolState {
                reserve0: u256_to_u128(U256::saturating_from(ret.reserve0)),
                reserve1: u256_to_u128(U256::saturating_from(ret.reserve1)),
                fee_ppm,
            }),
            last_updated_block: self
                .rpc
                .best_read()
                .block_number()
                .await
                .unwrap_or_default(),
            extras: HashMap::new(),
        }))
    }

    async fn fetch_v3(&self, spec: &DiscoveredPool) -> Result<Option<PoolState>> {
        let token0 = self
            .call_address(spec.address, IUniswapV3Pool::token0Call {}.abi_encode())
            .await?;
        let token1 = self
            .call_address(spec.address, IUniswapV3Pool::token1Call {}.abi_encode())
            .await?;
        if !self.token_map.contains_key(&token0) || !self.token_map.contains_key(&token1) {
            return Ok(None);
        }
        let raw_fee = self
            .rpc
            .best_read()
            .eth_call(
                spec.address,
                None,
                IUniswapV3Pool::feeCall {}.abi_encode().into(),
                "latest",
            )
            .await?;
        let fee_ret = IUniswapV3Pool::feeCall::abi_decode_returns(&raw_fee)?;
        let raw_spacing = self
            .rpc
            .best_read()
            .eth_call(
                spec.address,
                None,
                IUniswapV3Pool::tickSpacingCall {}.abi_encode().into(),
                "latest",
            )
            .await?;
        let spacing_ret = IUniswapV3Pool::tickSpacingCall::abi_decode_returns(&raw_spacing)?;
        let raw_liq = self
            .rpc
            .best_read()
            .eth_call(
                spec.address,
                None,
                IUniswapV3Pool::liquidityCall {}.abi_encode().into(),
                "latest",
            )
            .await?;
        let liq_ret = IUniswapV3Pool::liquidityCall::abi_decode_returns(&raw_liq)?;
        let raw_slot0 = self
            .rpc
            .best_read()
            .eth_call(
                spec.address,
                None,
                IUniswapV3Pool::slot0Call {}.abi_encode().into(),
                "latest",
            )
            .await?;
        let slot0 = IUniswapV3Pool::slot0Call::abi_decode_returns(&raw_slot0)?;

        Ok(Some(PoolState {
            pool_id: spec.address,
            dex_name: spec.dex_name.clone(),
            kind: spec.amm_kind,
            token_addresses: vec![token0, token1],
            token_symbols: vec![
                self.token_map[&token0].symbol.clone(),
                self.token_map[&token1].symbol.clone(),
            ],
            factory: spec.factory,
            registry: spec.registry,
            vault: spec.vault,
            quoter: spec.quoter,
            admission_status: PoolAdmissionStatus::Allowed,
            health: fresh_health(10_000, false),
            state: PoolSpecificState::UniswapV3Like(V3PoolState {
                sqrt_price_x96: U256::saturating_from(slot0.sqrtPriceX96),
                liquidity: liq_ret,
                tick: i32::try_from(slot0.tick).unwrap_or(0),
                fee: u32::try_from(fee_ret).unwrap_or(u32::MAX),
                tick_spacing: i32::try_from(spacing_ret).unwrap_or(0),
            }),
            last_updated_block: self
                .rpc
                .best_read()
                .block_number()
                .await
                .unwrap_or_default(),
            extras: HashMap::new(),
        }))
    }

    async fn fetch_curve(&self, spec: &DiscoveredPool) -> Result<Option<PoolState>> {
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
            if !self.token_map.contains_key(&coin_ret) {
                return Ok(None);
            }
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
            token_symbols.push(self.token_map[&coin_ret].symbol.clone());
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
            last_updated_block: self
                .rpc
                .best_read()
                .block_number()
                .await
                .unwrap_or_default(),
            extras: HashMap::new(),
        }))
    }

    async fn fetch_balancer(&self, spec: &DiscoveredPool) -> Result<Option<PoolState>> {
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
        if tokens_ret
            .tokens
            .iter()
            .any(|token| !self.token_map.contains_key(token))
        {
            return Ok(None);
        }
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
                .map(|token| self.token_map[token].symbol.clone())
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
            last_updated_block: self
                .rpc
                .best_read()
                .block_number()
                .await
                .unwrap_or_default(),
            extras: HashMap::new(),
        }))
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
