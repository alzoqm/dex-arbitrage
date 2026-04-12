use std::{collections::HashMap, env, fs, str::FromStr};

use alloy::primitives::Address;
use anyhow::{Context, Result};
use serde::Deserialize;

use crate::types::{AmmKind, Chain, DiscoveryKind};

#[derive(Debug, Clone)]
pub struct Settings {
    pub chain: Chain,
    pub chain_id: u64,
    pub native_symbol: String,
    pub operator_private_key: Option<String>,
    pub deployer_private_key: Option<String>,
    pub safe_owner: Option<Address>,
    pub simulation_only: bool,
    pub json_logs: bool,
    pub prometheus_bind: String,
    pub rpc: RpcSettings,
    pub contracts: ContractSettings,
    pub risk: RiskSettings,
    pub policy: UniversePolicy,
    pub tokens: Vec<TokenConfig>,
    pub dexes: Vec<DexConfig>,
}

#[derive(Debug, Clone)]
pub struct RpcSettings {
    pub public_rpc_url: String,
    pub fallback_rpc_url: Option<String>,
    pub preconf_rpc_url: Option<String>,
    pub ws_url: Option<String>,
    pub protected_rpc_url: Option<String>,
    pub private_submit_method: String,
    pub simulate_method: String,
}

#[derive(Debug, Clone)]
pub struct ContractSettings {
    pub executor_address: Option<Address>,
    pub aave_pool: Option<Address>,
    pub strict_target_allowlist: bool,
}

#[derive(Debug, Clone)]
pub struct RiskSettings {
    pub max_hops: usize,
    pub screening_margin_bps: u32,
    pub min_net_profit: i128,
    pub min_net_profit_usd_e8: u128,
    pub min_trade_usd_e8: u128,
    pub poll_interval_ms: u64,
    pub event_backfill_blocks: u64,
    pub staleness_timeout_ms: u64,
    pub gas_risk_buffer_pct: f64,
    pub gas_price_ceiling_wei: u128,
    pub max_position: u128,
    pub max_position_usd_e8: u128,
    pub max_flash_loan: u128,
    pub max_flash_loan_usd_e8: u128,
    pub daily_loss_limit: i128,
    pub daily_loss_limit_usd_e8: u128,
    pub min_profit_realization_bps: u32,
    pub max_concurrent_tx: usize,
    pub pool_health_min_bps: u16,
    pub stable_depeg_cutoff_e6: u32,
}

#[derive(Debug, Clone)]
pub struct TokenConfig {
    pub symbol: String,
    pub address: Address,
    pub decimals: u8,
    pub is_stable: bool,
    pub is_cycle_anchor: bool,
    pub flash_loan_enabled: bool,
    pub allow_self_funded: bool,
    pub manual_price_usd_e8: Option<u64>,
    pub max_position_usd_e8: Option<u128>,
    pub max_flash_loan_usd_e8: Option<u128>,
}

#[derive(Debug, Clone, Default)]
pub struct UniversePolicy {
    pub venues: Vec<String>,
    pub symbols: Vec<String>,
}

impl UniversePolicy {
    pub fn symbol_allowed(&self, symbol: &str) -> bool {
        self.symbols.is_empty()
            || self
                .symbols
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(symbol))
    }
}

#[derive(Debug, Clone)]
pub struct DexConfig {
    pub name: String,
    pub amm_kind: AmmKind,
    pub discovery_kind: DiscoveryKind,
    pub factory: Option<Address>,
    pub registry: Option<Address>,
    pub vault: Option<Address>,
    pub quoter: Option<Address>,
    pub fee_ppm: u32,
    pub start_block: u64,
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
struct FileConfig {
    chain: String,
    chain_id: u64,
    native_symbol: String,
    #[serde(default)]
    venues: Option<Vec<String>>,
    #[serde(default)]
    symbols: Option<Vec<String>>,
    #[serde(default)]
    policy: FilePolicyConfig,
    max_hops: usize,
    screening_margin_bps: u32,
    min_net_profit_default: i128,
    min_net_profit_usd_e8: Option<u128>,
    min_trade_usd_e8: Option<u128>,
    poll_interval_ms: u64,
    event_backfill_blocks: u64,
    staleness_timeout_ms: u64,
    gas_risk_buffer_pct: f64,
    gas_price_ceiling_wei: u128,
    max_position_default: u128,
    max_position_usd_e8: Option<u128>,
    max_flash_loan_default: u128,
    max_flash_loan_usd_e8: Option<u128>,
    daily_loss_limit_default: i128,
    daily_loss_limit_usd_e8: Option<u128>,
    min_profit_realization_bps: Option<u32>,
    pool_health_min_bps: u16,
    stable_depeg_cutoff_e6: u32,
    strict_target_allowlist: bool,
    tokens: Vec<FileTokenConfig>,
    dexes: Vec<FileDexConfig>,
}

#[derive(Debug, Default, Deserialize)]
struct FilePolicyConfig {
    #[serde(default)]
    venues: Option<Vec<String>>,
    #[serde(default)]
    symbols: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct FileTokenConfig {
    symbol: String,
    address_env: String,
    decimals: u8,
    is_stable: bool,
    #[serde(default)]
    is_cycle_anchor: Option<bool>,
    #[serde(default)]
    flash_loan_enabled: Option<bool>,
    #[serde(default)]
    allow_self_funded: Option<bool>,
    price_env: String,
    #[serde(default)]
    max_position_usd_e8: Option<u128>,
    #[serde(default)]
    max_flash_loan_usd_e8: Option<u128>,
}

#[derive(Debug, Deserialize)]
struct FileDexConfig {
    name: String,
    amm: String,
    discovery: String,
    factory_env: String,
    quoter_env: String,
    registry_env: String,
    vault_env: String,
    fee_ppm: u32,
    start_block: u64,
    enabled: bool,
}

impl Settings {
    pub fn load(chain: Chain) -> Result<Self> {
        let manifest_dir = env::current_dir().context("failed to get current directory")?;
        let path = manifest_dir.join(chain.config_path());
        let file = fs::read_to_string(&path)
            .with_context(|| format!("failed to read config file {}", path.display()))?;
        let file_cfg: FileConfig = toml::from_str(&file).context("failed to parse chain toml")?;

        if file_cfg.chain != chain.as_str() {
            anyhow::bail!(
                "config chain mismatch: expected {}, found {}",
                chain,
                file_cfg.chain
            );
        }

        let simulation_only = env_bool("SIMULATION_ONLY", true);
        let json_logs = env_bool("JSON_LOGS", false);
        let strict_target_allowlist =
            env_bool("STRICT_TARGET_ALLOWLIST", file_cfg.strict_target_allowlist);
        let safe_owner = env_opt("SAFE_OWNER").and_then(|v| parse_address(&v).ok());

        let rpc = match chain {
            Chain::Base => RpcSettings {
                public_rpc_url: env_req("BASE_PUBLIC_RPC_URL")?,
                fallback_rpc_url: env_opt("BASE_FALLBACK_RPC_URL"),
                preconf_rpc_url: env_opt("BASE_PRECONF_RPC_URL"),
                ws_url: env_opt("BASE_WSS_URL"),
                protected_rpc_url: env_opt("BASE_PROTECTED_RPC_URL"),
                private_submit_method: env_or(
                    "BASE_PRIVATE_SUBMIT_METHOD",
                    "eth_sendRawTransaction",
                ),
                simulate_method: env_or("BASE_SIMULATE_METHOD", "eth_call"),
            },
            Chain::Polygon => RpcSettings {
                public_rpc_url: env_req("POLYGON_PUBLIC_RPC_URL")?,
                fallback_rpc_url: env_opt("POLYGON_FALLBACK_RPC_URL"),
                preconf_rpc_url: env_opt("POLYGON_PRIVATE_MEMPOOL_URL"),
                ws_url: env_opt("POLYGON_WSS_URL"),
                protected_rpc_url: env_opt("POLYGON_PRIVATE_MEMPOOL_URL"),
                private_submit_method: env_or(
                    "POLYGON_PRIVATE_SUBMIT_METHOD",
                    "eth_sendRawTransaction",
                ),
                simulate_method: env_or("POLYGON_SIMULATE_METHOD", "eth_call"),
            },
        };

        let contracts = ContractSettings {
            executor_address: env_opt(chain.executor_env()).and_then(|v| parse_address(&v).ok()),
            aave_pool: env_opt(chain.aave_pool_env()).and_then(|v| parse_address(&v).ok()),
            strict_target_allowlist,
        };

        let risk = RiskSettings {
            max_hops: file_cfg.max_hops,
            screening_margin_bps: file_cfg.screening_margin_bps,
            min_net_profit: env_opt_i128(if chain == Chain::Base {
                "BASE_MIN_NET_PROFIT"
            } else {
                "POLYGON_MIN_NET_PROFIT"
            })
            .unwrap_or(file_cfg.min_net_profit_default),
            min_net_profit_usd_e8: env_opt_u128(if chain == Chain::Base {
                "BASE_MIN_NET_PROFIT_USD_E8"
            } else {
                "POLYGON_MIN_NET_PROFIT_USD_E8"
            })
            .or(file_cfg.min_net_profit_usd_e8)
            .unwrap_or({
                // Backward compatible default: convert old raw USDC value
                // 100000 raw USDC (6 decimals) at $1 = $0.10 = 10_000_000 e8
                10_000_000
            }),
            min_trade_usd_e8: env_opt_u128(if chain == Chain::Base {
                "BASE_MIN_TRADE_USD_E8"
            } else {
                "POLYGON_MIN_TRADE_USD_E8"
            })
            .or(file_cfg.min_trade_usd_e8)
            .unwrap_or(1_000_000_000), // $10 default
            poll_interval_ms: env_opt_u64("POLL_INTERVAL_MS").unwrap_or(file_cfg.poll_interval_ms),
            event_backfill_blocks: env_opt_u64("EVENT_BACKFILL_BLOCKS")
                .unwrap_or(file_cfg.event_backfill_blocks),
            staleness_timeout_ms: env_opt_u64("STALENESS_TIMEOUT_MS")
                .unwrap_or(file_cfg.staleness_timeout_ms),
            gas_risk_buffer_pct: env_opt_f64("GAS_RISK_BUFFER_PCT")
                .unwrap_or(file_cfg.gas_risk_buffer_pct),
            gas_price_ceiling_wei: env_opt_u128(if chain == Chain::Base {
                "BASE_GAS_PRICE_CEILING_WEI"
            } else {
                "POLYGON_GAS_PRICE_CEILING_WEI"
            })
            .unwrap_or(file_cfg.gas_price_ceiling_wei),
            max_position: env_opt_u128(if chain == Chain::Base {
                "BASE_MAX_POSITION"
            } else {
                "POLYGON_MAX_POSITION"
            })
            .unwrap_or(file_cfg.max_position_default),
            max_position_usd_e8: env_opt_u128(if chain == Chain::Base {
                "BASE_MAX_POSITION_USD_E8"
            } else {
                "POLYGON_MAX_POSITION_USD_E8"
            })
            .or(file_cfg.max_position_usd_e8)
            .unwrap_or(200_000_000_000), // $2000 default
            max_flash_loan: env_opt_u128(if chain == Chain::Base {
                "BASE_MAX_FLASH_LOAN"
            } else {
                "POLYGON_MAX_FLASH_LOAN"
            })
            .unwrap_or(file_cfg.max_flash_loan_default),
            max_flash_loan_usd_e8: env_opt_u128(if chain == Chain::Base {
                "BASE_MAX_FLASH_LOAN_USD_E8"
            } else {
                "POLYGON_MAX_FLASH_LOAN_USD_E8"
            })
            .or(file_cfg.max_flash_loan_usd_e8)
            .unwrap_or(1_000_000_000_000), // $10000 default
            daily_loss_limit: env_opt_i128("DAILY_LOSS_LIMIT")
                .unwrap_or(file_cfg.daily_loss_limit_default),
            daily_loss_limit_usd_e8: env_opt_u128("DAILY_LOSS_LIMIT_USD_E8")
                .or(file_cfg.daily_loss_limit_usd_e8)
                .unwrap_or(50_000_000_000), // $500 default
            min_profit_realization_bps: env_opt_u32("MIN_PROFIT_REALIZATION_BPS")
                .or(file_cfg.min_profit_realization_bps)
                .unwrap_or(9000), // 90% default
            max_concurrent_tx: env_opt_usize("MAX_CONCURRENT_TX").unwrap_or(1),
            pool_health_min_bps: env_opt_u16("POOL_HEALTH_MIN_BPS")
                .unwrap_or(file_cfg.pool_health_min_bps),
            stable_depeg_cutoff_e6: env_opt_u32("STABLE_DEPEG_CUTOFF_E6")
                .unwrap_or(file_cfg.stable_depeg_cutoff_e6),
        };

        if risk.min_profit_realization_bps > 10_000 {
            anyhow::bail!(
                "MIN_PROFIT_REALIZATION_BPS must be between 0 and 10000, got {}",
                risk.min_profit_realization_bps
            );
        }

        let policy = UniversePolicy {
            venues: file_cfg
                .policy
                .venues
                .clone()
                .or_else(|| file_cfg.venues.clone())
                .unwrap_or_default(),
            symbols: file_cfg
                .policy
                .symbols
                .clone()
                .or_else(|| file_cfg.symbols.clone())
                .unwrap_or_default(),
        };

        let symbol_filter = policy
            .symbols
            .clone()
            .into_iter()
            .map(|symbol| symbol.to_ascii_lowercase())
            .collect::<std::collections::HashSet<_>>();
        let venue_filter = policy
            .venues
            .clone()
            .into_iter()
            .map(|venue| venue.to_ascii_lowercase())
            .collect::<std::collections::HashSet<_>>();

        let tokens = file_cfg
            .tokens
            .into_iter()
            .filter(|token| {
                symbol_filter.is_empty()
                    || symbol_filter.contains(&token.symbol.to_ascii_lowercase())
            })
            .filter_map(|token| {
                let address = match env_opt(&token.address_env) {
                    Some(addr) if !addr.trim().is_empty() => parse_address(&addr).ok(),
                    _ => None,
                }?;
                let manual_price_usd_e8 = if token.price_env.trim().is_empty() {
                    None
                } else {
                    env_opt_u64(&token.price_env)
                };
                // Start/end anchors are assigned from live Aave reserves during discovery.
                let _configured_cycle_anchor = token.is_cycle_anchor;
                let _configured_flash_loan_enabled = token.flash_loan_enabled;
                let is_cycle_anchor = false;
                let flash_loan_enabled = false;
                let allow_self_funded = token.allow_self_funded.unwrap_or(token.is_stable);
                Some(TokenConfig {
                    symbol: token.symbol,
                    address,
                    decimals: token.decimals,
                    is_stable: token.is_stable,
                    is_cycle_anchor,
                    flash_loan_enabled,
                    allow_self_funded,
                    manual_price_usd_e8,
                    max_position_usd_e8: token.max_position_usd_e8,
                    max_flash_loan_usd_e8: token.max_flash_loan_usd_e8,
                })
            })
            .collect::<Vec<_>>();

        let dexes = file_cfg
            .dexes
            .into_iter()
            .filter(|dex| {
                venue_filter.is_empty() || venue_filter.contains(&dex.name.to_ascii_lowercase())
            })
            .map(|dex| -> Result<DexConfig> {
                let amm_kind = AmmKind::from_toml(&dex.amm)?;
                let discovery_kind = DiscoveryKind::from_toml(&dex.discovery)?;
                let factory = resolve_address_from_env_name(&dex.factory_env)?;
                let registry = resolve_address_from_env_name(&dex.registry_env)?;
                let vault = resolve_address_from_env_name(&dex.vault_env)?;
                let quoter = resolve_address_from_env_name(&dex.quoter_env)?;
                Ok(DexConfig {
                    name: dex.name,
                    amm_kind,
                    discovery_kind,
                    factory,
                    registry,
                    vault,
                    quoter,
                    fee_ppm: dex.fee_ppm,
                    start_block: dex.start_block,
                    enabled: dex.enabled,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        validate_dex_configs(&dexes)?;

        Ok(Self {
            chain,
            chain_id: file_cfg.chain_id,
            native_symbol: file_cfg.native_symbol,
            operator_private_key: env_opt("OPERATOR_PRIVATE_KEY"),
            deployer_private_key: env_opt("DEPLOYER_PRIVATE_KEY"),
            safe_owner,
            simulation_only,
            json_logs,
            prometheus_bind: env_or("PROMETHEUS_BIND", "0.0.0.0:9898"),
            rpc,
            contracts,
            risk,
            policy,
            tokens,
            dexes,
        })
    }

    pub fn token_map(&self) -> HashMap<Address, TokenConfig> {
        self.tokens
            .iter()
            .cloned()
            .map(|token| (token.address, token))
            .collect()
    }

    pub fn stable_tokens(&self) -> Vec<Address> {
        self.tokens
            .iter()
            .filter(|token| token.is_stable)
            .map(|token| token.address)
            .collect()
    }

    pub fn cycle_anchor_tokens(&self) -> Vec<Address> {
        self.tokens
            .iter()
            .filter(|token| token.flash_loan_enabled)
            .map(|token| token.address)
            .collect()
    }

    pub fn flash_loan_tokens(&self) -> Vec<Address> {
        self.tokens
            .iter()
            .filter(|token| token.flash_loan_enabled)
            .map(|token| token.address)
            .collect()
    }

    pub fn token_by_address(&self, address: Address) -> Option<&TokenConfig> {
        self.tokens.iter().find(|token| token.address == address)
    }

    pub fn data_path(&self, suffix: &str) -> String {
        format!("state/{}_{}.json", self.chain.as_str(), suffix)
    }
}

fn resolve_address_from_env_name(env_name: &str) -> Result<Option<Address>> {
    if env_name.trim().is_empty() {
        return Ok(None);
    }
    match env_opt(env_name) {
        Some(value) if !value.trim().is_empty() => Ok(Some(parse_address(&value)?)),
        _ => Ok(None),
    }
}

fn parse_address(value: &str) -> Result<Address> {
    Address::from_str(value).with_context(|| format!("invalid address: {value}"))
}

fn validate_dex_configs(dexes: &[DexConfig]) -> Result<()> {
    if env_bool("ALLOW_DUPLICATE_DEX_FACTORIES", false) {
        return Ok(());
    }

    let mut seen = HashMap::<(DiscoveryKind, AmmKind, Address), String>::new();
    for dex in dexes.iter().filter(|dex| dex.enabled) {
        let Some(factory) = dex.factory else {
            continue;
        };
        let key = (dex.discovery_kind, dex.amm_kind, factory);
        if let Some(existing) = seen.get(&key) {
            anyhow::bail!(
                "duplicate enabled DEX factory config: {} and {} both use {} for {:?}/{:?}. Set ALLOW_DUPLICATE_DEX_FACTORIES=true only if this is intentional.",
                existing,
                dex.name,
                factory,
                dex.discovery_kind,
                dex.amm_kind
            );
        }
        seen.insert(key, dex.name.clone());
    }
    Ok(())
}

fn env_req(key: &str) -> Result<String> {
    env::var(key).with_context(|| format!("required env missing: {key}"))
}

fn env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_opt(key: &str) -> Option<String> {
    env::var(key).ok().filter(|v| !v.trim().is_empty())
}

fn env_opt_u64(key: &str) -> Option<u64> {
    env_opt(key).and_then(|v| v.parse().ok())
}

fn env_opt_u128(key: &str) -> Option<u128> {
    env_opt(key).and_then(|v| v.parse().ok())
}

fn env_opt_i128(key: &str) -> Option<i128> {
    env_opt(key).and_then(|v| v.parse().ok())
}

fn env_opt_u32(key: &str) -> Option<u32> {
    env_opt(key).and_then(|v| v.parse().ok())
}

fn env_opt_u16(key: &str) -> Option<u16> {
    env_opt(key).and_then(|v| v.parse().ok())
}

fn env_opt_usize(key: &str) -> Option<usize> {
    env_opt(key).and_then(|v| v.parse().ok())
}

fn env_opt_f64(key: &str) -> Option<f64> {
    env_opt(key).and_then(|v| v.parse().ok())
}

fn env_bool(key: &str, default: bool) -> bool {
    env_opt(key)
        .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(default)
}
