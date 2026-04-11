
use std::{collections::HashMap, env, fs, path::Path, str::FromStr};

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
    pub poll_interval_ms: u64,
    pub event_backfill_blocks: u64,
    pub staleness_timeout_ms: u64,
    pub gas_risk_buffer_pct: f64,
    pub gas_price_ceiling_wei: u128,
    pub max_position: u128,
    pub max_flash_loan: u128,
    pub daily_loss_limit: i128,
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
    pub manual_price_usd_e8: Option<u64>,
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
    max_hops: usize,
    screening_margin_bps: u32,
    min_net_profit_default: i128,
    poll_interval_ms: u64,
    event_backfill_blocks: u64,
    staleness_timeout_ms: u64,
    gas_risk_buffer_pct: f64,
    gas_price_ceiling_wei: u128,
    max_position_default: u128,
    max_flash_loan_default: u128,
    daily_loss_limit_default: i128,
    pool_health_min_bps: u16,
    stable_depeg_cutoff_e6: u32,
    strict_target_allowlist: bool,
    tokens: Vec<FileTokenConfig>,
    dexes: Vec<FileDexConfig>,
}

#[derive(Debug, Deserialize)]
struct FileTokenConfig {
    symbol: String,
    address_env: String,
    decimals: u8,
    is_stable: bool,
    price_env: String,
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
            anyhow::bail!("config chain mismatch: expected {}, found {}", chain, file_cfg.chain);
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
                private_submit_method: env_or("BASE_PRIVATE_SUBMIT_METHOD", "eth_sendRawTransaction"),
                simulate_method: env_or("BASE_SIMULATE_METHOD", "eth_call"),
            },
            Chain::Polygon => RpcSettings {
                public_rpc_url: env_req("POLYGON_PUBLIC_RPC_URL")?,
                fallback_rpc_url: env_opt("POLYGON_FALLBACK_RPC_URL"),
                preconf_rpc_url: env_opt("POLYGON_PRIVATE_MEMPOOL_URL"),
                ws_url: env_opt("POLYGON_WSS_URL"),
                protected_rpc_url: env_opt("POLYGON_PRIVATE_MEMPOOL_URL"),
                private_submit_method: env_or("POLYGON_PRIVATE_SUBMIT_METHOD", "eth_sendRawTransaction"),
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
            min_net_profit: env_opt_i128(if chain == Chain::Base { "BASE_MIN_NET_PROFIT" } else { "POLYGON_MIN_NET_PROFIT" })
                .unwrap_or(file_cfg.min_net_profit_default),
            poll_interval_ms: env_opt_u64("POLL_INTERVAL_MS").unwrap_or(file_cfg.poll_interval_ms),
            event_backfill_blocks: env_opt_u64("EVENT_BACKFILL_BLOCKS").unwrap_or(file_cfg.event_backfill_blocks),
            staleness_timeout_ms: env_opt_u64("STALENESS_TIMEOUT_MS").unwrap_or(file_cfg.staleness_timeout_ms),
            gas_risk_buffer_pct: env_opt_f64("GAS_RISK_BUFFER_PCT").unwrap_or(file_cfg.gas_risk_buffer_pct),
            gas_price_ceiling_wei: env_opt_u128(if chain == Chain::Base { "BASE_GAS_PRICE_CEILING_WEI" } else { "POLYGON_GAS_PRICE_CEILING_WEI" })
                .unwrap_or(file_cfg.gas_price_ceiling_wei),
            max_position: env_opt_u128(if chain == Chain::Base { "BASE_MAX_POSITION" } else { "POLYGON_MAX_POSITION" })
                .unwrap_or(file_cfg.max_position_default),
            max_flash_loan: env_opt_u128(if chain == Chain::Base { "BASE_MAX_FLASH_LOAN" } else { "POLYGON_MAX_FLASH_LOAN" })
                .unwrap_or(file_cfg.max_flash_loan_default),
            daily_loss_limit: env_opt_i128("DAILY_LOSS_LIMIT").unwrap_or(file_cfg.daily_loss_limit_default),
            max_concurrent_tx: env_opt_usize("MAX_CONCURRENT_TX").unwrap_or(1),
            pool_health_min_bps: env_opt_u16("POOL_HEALTH_MIN_BPS").unwrap_or(file_cfg.pool_health_min_bps),
            stable_depeg_cutoff_e6: env_opt_u32("STABLE_DEPEG_CUTOFF_E6").unwrap_or(file_cfg.stable_depeg_cutoff_e6),
        };

        let tokens = file_cfg
            .tokens
            .into_iter()
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
                Some(TokenConfig {
                    symbol: token.symbol,
                    address,
                    decimals: token.decimals,
                    is_stable: token.is_stable,
                    manual_price_usd_e8,
                })
            })
            .collect::<Vec<_>>();

        if tokens.is_empty() {
            anyhow::bail!("no token addresses configured for {}", chain);
        }

        let dexes = file_cfg
            .dexes
            .into_iter()
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
