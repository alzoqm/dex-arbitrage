pub mod admission;
pub mod event_stream;
pub mod factory_scanner;
pub mod pool_fetcher;

use std::{collections::HashMap, sync::Arc};

use alloy::{
    primitives::{Address, U256},
    sol_types::SolCall,
};
use anyhow::Result;

use crate::{
    abi::{IAavePool, IERC20},
    config::Settings,
    graph::GraphSnapshot,
    rpc::RpcClients,
    types::{BlockRef, PoolSpecificState, PoolState, TokenBehavior, TokenInfo},
};

use self::{
    admission::AdmissionEngine,
    event_stream::EventStream,
    factory_scanner::{DiscoveredPool, FactoryScanner},
    pool_fetcher::PoolFetcher,
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
}

impl DiscoveryManager {
    pub fn new(settings: Arc<Settings>, rpc: Arc<RpcClients>) -> Self {
        Self {
            scanner: FactoryScanner::new(settings.clone(), rpc.clone()),
            fetcher: PoolFetcher::new(settings.clone(), rpc.clone()),
            admission: AdmissionEngine::new(settings.clone()),
            settings,
            rpc,
        }
    }

    pub async fn bootstrap(&self) -> Result<DiscoveryOutput> {
        let discovered = self.scanner.scan_all().await?;
        let mut pools = HashMap::new();

        for spec in discovered {
            if let Some(pool) = self.fetcher.fetch_pool(&spec).await? {
                if self.admission.admit(&pool) {
                    pools.insert(pool.pool_id, pool);
                }
            }
        }

        let mut tokens = self.discover_tokens(&pools).await?;
        derive_missing_prices_from_pools(&mut tokens, &pools);

        let latest = self.scanner.latest_block().await.unwrap_or(0);
        let block_ref = if latest > 0 {
            self.scanner.current_block_ref().await.ok()
        } else {
            None
        };

        let snapshot = GraphSnapshot::build(0, block_ref, tokens.clone(), pools.clone());
        Ok(DiscoveryOutput {
            tokens,
            pools,
            snapshot,
        })
    }

    pub async fn refresh_pools(
        &self,
        current_snapshot_id: u64,
        tokens: Vec<TokenInfo>,
        current_pools: HashMap<Address, PoolState>,
        changed_specs: Vec<DiscoveredPool>,
        block_ref: Option<BlockRef>,
    ) -> Result<(GraphSnapshot, Vec<Address>)> {
        let mut pools = current_pools;
        let mut changed_pool_ids = Vec::new();
        let mut tokens = tokens;

        for spec in changed_specs {
            if let Some(pool) = self.fetcher.fetch_pool(&spec).await? {
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

    async fn discover_tokens(&self, pools: &HashMap<Address, PoolState>) -> Result<Vec<TokenInfo>> {
        let mut tokens = self.configured_tokens();
        let flash_reserves = self.aave_flash_reserves().await?;
        self.extend_tokens_with_discovered(&mut tokens, pools.values(), &flash_reserves)
            .await?;
        apply_aave_flash_reserves(&mut tokens, &flash_reserves);
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

        for pool in pools {
            for address in pool.token_addresses.iter().copied() {
                if !known.insert(address) {
                    continue;
                }
                let (symbol, decimals) = self.fetch_token_metadata(address).await;
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
        }

        for address in flash_reserves.keys().copied() {
            if !known.insert(address) {
                continue;
            }
            let (symbol, decimals) = self.fetch_token_metadata(address).await;
            if !self.settings.policy.symbol_allowed(&symbol) {
                continue;
            }
            let is_stable = is_stable_symbol(&symbol);
            tokens.push(TokenInfo {
                address,
                symbol,
                decimals,
                is_stable,
                is_cycle_anchor: true,
                flash_loan_enabled: true,
                allow_self_funded: false,
                behavior: TokenBehavior::default(),
                manual_price_usd_e8: is_stable.then_some(100_000_000),
                max_position_usd_e8: None,
                max_flash_loan_usd_e8: None,
            });
        }

        Ok(())
    }

    async fn aave_flash_reserves(&self) -> Result<HashMap<Address, ()>> {
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
        for asset in reserves {
            let raw = self
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
                .await?;
            let config = IAavePool::getConfigurationCall::abi_decode_returns(&raw)?;
            if reserve_accepts_flash_loan(config.data) {
                out.insert(asset, ());
            }
        }
        Ok(out)
    }

    async fn fetch_token_metadata(&self, address: Address) -> (String, u8) {
        let symbol = self
            .fetch_token_symbol(address)
            .await
            .unwrap_or_else(|| address.to_string());
        let decimals = self.fetch_token_decimals(address).await.unwrap_or(18);
        (symbol, decimals)
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

        if let Ok(symbol) = IERC20::symbolCall::abi_decode_returns(&raw) {
            let trimmed = symbol.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }

        decode_bytes32_string(raw.as_ref())
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
        let flash_loan_enabled = flash_reserves.contains_key(&token.address);
        token.is_cycle_anchor = flash_loan_enabled;
        token.flash_loan_enabled = flash_loan_enabled;
    }
}

fn reserve_bit(config_data: U256, bit: usize) -> bool {
    ((config_data >> bit) & U256::from(1u8)) == U256::from(1u8)
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

    for known_idx in 0..addresses.len() {
        let known = addresses[known_idx];
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

        for unknown_idx in 0..addresses.len() {
            if known_idx == unknown_idx {
                continue;
            }
            let unknown = addresses[unknown_idx];
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
    fn aave_reserves_are_the_only_cycle_anchors() {
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
        assert!(!tokens[1].is_cycle_anchor);
        assert!(!tokens[1].flash_loan_enabled);
    }
}
