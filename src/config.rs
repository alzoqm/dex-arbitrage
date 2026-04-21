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
    pub search: SearchSettings,
    pub policy: UniversePolicy,
    pub execution: ExecutionSettings,
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
pub struct ExecutionSettings {
    /// Allow public RPC fallback for transaction submission.
    /// Default: false (private/protected only for production safety).
    pub allow_public_fallback: bool,
    /// Require fresh simulation immediately before signing.
    /// Default: true (production safety).
    pub require_fresh_simulation: bool,
    /// Enable nonce replacement for stuck transactions.
    /// Default: true.
    pub enable_nonce_replacement: bool,
    /// Minimum priority fee bump for nonce replacement (in wei).
    /// Default: 1 gwei.
    pub nonce_bump_priority_fee_wei: u128,
    /// Minimum base fee bump for nonce replacement (in wei).
    /// Default: 1 gwei.
    pub nonce_bump_base_fee_wei: u128,
}

impl Default for ExecutionSettings {
    fn default() -> Self {
        Self {
            allow_public_fallback: false,
            require_fresh_simulation: true,
            enable_nonce_replacement: true,
            nonce_bump_priority_fee_wei: 1_000_000_000,
            nonce_bump_base_fee_wei: 1_000_000_000,
        }
    }
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
pub struct SearchSettings {
    pub top_k_paths_per_side: usize,
    pub max_virtual_branches_per_node: usize,
    pub path_beam_width: usize,
    pub max_candidates_per_refresh: usize,
    pub candidate_selection_buffer_multiplier: usize,
    pub dedup_token_paths: bool,
    pub max_pair_edges_per_pair: usize,
    pub max_split_parallel_pools: usize,
}

impl Default for SearchSettings {
    fn default() -> Self {
        Self {
            top_k_paths_per_side: 8,
            max_virtual_branches_per_node: 16,
            path_beam_width: 96,
            max_candidates_per_refresh: 512,
            candidate_selection_buffer_multiplier: 1,
            dedup_token_paths: true,
            max_pair_edges_per_pair: 1,
            max_split_parallel_pools: 3,
        }
    }
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
    min_net_profit_default: i64,
    min_net_profit_usd_e8: Option<u64>,
    min_trade_usd_e8: Option<u64>,
    poll_interval_ms: u64,
    event_backfill_blocks: u64,
    staleness_timeout_ms: u64,
    gas_risk_buffer_pct: f64,
    gas_price_ceiling_wei: u64,
    max_position_default: u64,
    max_position_usd_e8: Option<u64>,
    max_flash_loan_default: u64,
    max_flash_loan_usd_e8: Option<u64>,
    daily_loss_limit_default: i64,
    daily_loss_limit_usd_e8: Option<u64>,
    min_profit_realization_bps: Option<u32>,
    pool_health_min_bps: u16,
    stable_depeg_cutoff_e6: u32,
    strict_target_allowlist: bool,
    #[serde(default)]
    search: FileSearchConfig,
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

#[derive(Debug, Default, Deserialize)]
struct FileSearchConfig {
    top_k_paths_per_side: Option<usize>,
    max_virtual_branches_per_node: Option<usize>,
    path_beam_width: Option<usize>,
    max_candidates_per_refresh: Option<usize>,
    candidate_selection_buffer_multiplier: Option<usize>,
    dedup_token_paths: Option<bool>,
    max_pair_edges_per_pair: Option<usize>,
    max_split_parallel_pools: Option<usize>,
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
    max_position_usd_e8: Option<u64>,
    #[serde(default)]
    max_flash_loan_usd_e8: Option<u64>,
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
        let safe_owner = parse_env_address_opt("SAFE_OWNER")?;

        let public_rpc_key = chain.env_key("PUBLIC_RPC_URL");
        let fallback_rpc_key = chain.env_key("FALLBACK_RPC_URL");
        let preconf_rpc_key = chain.env_key("PRECONF_RPC_URL");
        let ws_key = chain.env_key("WSS_URL");
        let protected_rpc_key = match chain {
            Chain::Polygon => chain.env_key("PRIVATE_MEMPOOL_URL"),
            _ => chain.env_key("PROTECTED_RPC_URL"),
        };
        let private_submit_key = chain.env_key("PRIVATE_SUBMIT_METHOD");
        let simulate_method_key = chain.env_key("SIMULATE_METHOD");
        let rpc = RpcSettings {
            public_rpc_url: env_req(&public_rpc_key)?,
            fallback_rpc_url: env_opt(&fallback_rpc_key),
            preconf_rpc_url: env_opt(&preconf_rpc_key),
            ws_url: env_opt(&ws_key),
            protected_rpc_url: env_opt(&protected_rpc_key),
            private_submit_method: env_or(&private_submit_key, "eth_sendRawTransaction"),
            simulate_method: env_or(&simulate_method_key, "eth_call"),
        };

        let contracts = ContractSettings {
            executor_address: parse_env_address_opt(chain.executor_env())?,
            aave_pool: parse_env_address_opt(chain.aave_pool_env())?,
            strict_target_allowlist,
        };

        let risk = RiskSettings {
            max_hops: file_cfg.max_hops,
            screening_margin_bps: file_cfg.screening_margin_bps,
            min_net_profit: env_opt_i128(&chain.env_key("MIN_NET_PROFIT"))
                .unwrap_or(i128::from(file_cfg.min_net_profit_default)),
            min_net_profit_usd_e8: env_opt_u128(&chain.env_key("MIN_NET_PROFIT_USD_E8"))
            .or(file_cfg.min_net_profit_usd_e8.map(u128::from))
            .unwrap_or({
                // Backward compatible default: convert old raw USDC value
                // 100000 raw USDC (6 decimals) at $1 = $0.10 = 10_000_000 e8
                10_000_000
            }),
            min_trade_usd_e8: env_opt_u128(&chain.env_key("MIN_TRADE_USD_E8"))
            .or(file_cfg.min_trade_usd_e8.map(u128::from))
            .unwrap_or(1_000_000_000), // $10 default
            poll_interval_ms: env_opt_u64("POLL_INTERVAL_MS").unwrap_or(file_cfg.poll_interval_ms),
            event_backfill_blocks: env_opt_u64("EVENT_BACKFILL_BLOCKS")
                .unwrap_or(file_cfg.event_backfill_blocks),
            staleness_timeout_ms: env_opt_u64("STALENESS_TIMEOUT_MS")
                .unwrap_or(file_cfg.staleness_timeout_ms),
            gas_risk_buffer_pct: env_opt_f64("GAS_RISK_BUFFER_PCT")
                .unwrap_or(file_cfg.gas_risk_buffer_pct),
            gas_price_ceiling_wei: env_opt_u128(&chain.env_key("GAS_PRICE_CEILING_WEI"))
            .unwrap_or(u128::from(file_cfg.gas_price_ceiling_wei)),
            max_position: env_opt_u128(&chain.env_key("MAX_POSITION"))
                .unwrap_or(u128::from(file_cfg.max_position_default)),
            max_position_usd_e8: env_opt_u128(&chain.env_key("MAX_POSITION_USD_E8"))
            .or(file_cfg.max_position_usd_e8.map(u128::from))
            .unwrap_or(200_000_000_000), // $2000 default
            max_flash_loan: env_opt_u128(&chain.env_key("MAX_FLASH_LOAN"))
                .unwrap_or(u128::from(file_cfg.max_flash_loan_default)),
            max_flash_loan_usd_e8: env_opt_u128(&chain.env_key("MAX_FLASH_LOAN_USD_E8"))
            .or(file_cfg.max_flash_loan_usd_e8.map(u128::from))
            .unwrap_or(1_000_000_000_000), // $10000 default
            daily_loss_limit: env_opt_i128("DAILY_LOSS_LIMIT")
                .unwrap_or(i128::from(file_cfg.daily_loss_limit_default)),
            daily_loss_limit_usd_e8: env_opt_u128("DAILY_LOSS_LIMIT_USD_E8")
                .or(file_cfg.daily_loss_limit_usd_e8.map(u128::from))
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

        let search = SearchSettings {
            top_k_paths_per_side: configured_usize(
                "SEARCH_TOP_K_PATHS_PER_SIDE",
                file_cfg.search.top_k_paths_per_side,
                8,
            ),
            max_virtual_branches_per_node: configured_usize(
                "SEARCH_MAX_VIRTUAL_BRANCHES_PER_NODE",
                file_cfg.search.max_virtual_branches_per_node,
                16,
            ),
            path_beam_width: configured_usize(
                "SEARCH_PATH_BEAM_WIDTH",
                file_cfg.search.path_beam_width,
                96,
            ),
            max_candidates_per_refresh: configured_usize(
                "SEARCH_MAX_CANDIDATES_PER_REFRESH",
                file_cfg.search.max_candidates_per_refresh,
                512,
            ),
            candidate_selection_buffer_multiplier: configured_usize(
                "SEARCH_CANDIDATE_SELECTION_BUFFER_MULTIPLIER",
                file_cfg.search.candidate_selection_buffer_multiplier,
                1,
            ),
            dedup_token_paths: env_bool(
                "SEARCH_DEDUP_TOKEN_PATHS",
                file_cfg.search.dedup_token_paths.unwrap_or(true),
            ),
            max_pair_edges_per_pair: configured_usize(
                "SEARCH_MAX_PAIR_EDGES_PER_PAIR",
                file_cfg.search.max_pair_edges_per_pair,
                1,
            ),
            max_split_parallel_pools: configured_usize(
                "MAX_SPLIT_PARALLEL_POOLS",
                file_cfg.search.max_split_parallel_pools,
                3,
            ),
        };

        let env_venues = env_csv("POLICY_VENUES");
        let env_symbols = env_csv("POLICY_SYMBOLS");
        let policy = UniversePolicy {
            venues: if env_venues.is_empty() {
                file_cfg
                    .policy
                    .venues
                    .clone()
                    .or_else(|| file_cfg.venues.clone())
                    .unwrap_or_default()
            } else {
                env_venues
            },
            symbols: if env_symbols.is_empty() {
                file_cfg
                    .policy
                    .symbols
                    .clone()
                    .or_else(|| file_cfg.symbols.clone())
                    .unwrap_or_default()
            } else {
                env_symbols
            },
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
            .map(|token| -> Result<Option<TokenConfig>> {
                let Some(addr) = env_opt(&token.address_env) else {
                    return Ok(None);
                };
                let address = parse_address(&addr)
                    .with_context(|| format!("invalid token address env {}", token.address_env))?;
                let manual_price_usd_e8 = if token.price_env.trim().is_empty() {
                    None
                } else {
                    env_opt_u64(&token.price_env)
                };
                let allow_self_funded = token.allow_self_funded.unwrap_or(token.is_stable);
                let flash_loan_enabled = token.flash_loan_enabled.unwrap_or(false);
                let is_cycle_anchor = token
                    .is_cycle_anchor
                    .unwrap_or(token.is_stable || allow_self_funded || flash_loan_enabled);
                Ok(Some(TokenConfig {
                    symbol: token.symbol,
                    address,
                    decimals: token.decimals,
                    is_stable: token.is_stable,
                    is_cycle_anchor,
                    flash_loan_enabled,
                    allow_self_funded,
                    manual_price_usd_e8,
                    max_position_usd_e8: token.max_position_usd_e8.map(u128::from),
                    max_flash_loan_usd_e8: token.max_flash_loan_usd_e8.map(u128::from),
                }))
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
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

        let execution = ExecutionSettings {
            allow_public_fallback: env_bool("ALLOW_PUBLIC_FALLBACK", false), // Default: false for production safety
            require_fresh_simulation: env_bool("REQUIRE_FRESH_SIMULATION", true),
            enable_nonce_replacement: env_bool("ENABLE_NONCE_REPLACEMENT", true),
            nonce_bump_priority_fee_wei: env_opt_u128("NONCE_BUMP_PRIORITY_FEE_WEI")
                .unwrap_or(1_000_000_000), // 1 gwei default
            nonce_bump_base_fee_wei: env_opt_u128("NONCE_BUMP_BASE_FEE_WEI")
                .unwrap_or(1_000_000_000), // 1 gwei default
        };

        Ok(Self {
            chain,
            chain_id: file_cfg.chain_id,
            native_symbol: file_cfg.native_symbol,
            operator_private_key: env_opt("OPERATOR_PRIVATE_KEY"),
            deployer_private_key: env_opt("DEPLOYER_PRIVATE_KEY"),
            safe_owner,
            simulation_only,
            json_logs,
            prometheus_bind: env_or("PROMETHEUS_BIND", "127.0.0.1:9898"),
            rpc,
            contracts,
            risk,
            search,
            policy,
            execution,
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
            .filter(|token| token.is_cycle_anchor)
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

fn parse_env_address_opt(key: &str) -> Result<Option<Address>> {
    env_opt(key)
        .map(|value| parse_address(&value).with_context(|| format!("invalid address env {key}")))
        .transpose()
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
    let value = env::var(key).with_context(|| format!("required env missing: {key}"))?;
    if is_unset_marker(&value) {
        anyhow::bail!("required env unset: {key}");
    }
    Ok(value)
}

fn env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_opt(key: &str) -> Option<String> {
    env::var(key).ok().filter(|v| !is_unset_marker(v))
}

fn env_csv(key: &str) -> Vec<String> {
    env_opt(key)
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn is_unset_marker(value: &str) -> bool {
    let trimmed = value.trim().trim_matches('"');
    trimmed.is_empty() || trimmed.contains("확인 필요") || trimmed.contains("추가 세팅 필요")
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

fn configured_usize(env_key: &str, file_value: Option<usize>, default: usize) -> usize {
    env_opt_usize(env_key)
        .filter(|value| *value > 0)
        .or_else(|| file_value.filter(|value| *value > 0))
        .unwrap_or(default)
}

fn env_opt_f64(key: &str) -> Option<f64> {
    env_opt(key).and_then(|v| v.parse().ok())
}

fn env_bool(key: &str, default: bool) -> bool {
    env_opt(key)
        .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    #[test]
    fn env_opt_treats_setup_markers_as_unset() {
        let key = "DEX_ARB_TEST_SETUP_MARKER";
        std::env::set_var(key, "추가 세팅 필요");
        assert_eq!(super::env_opt(key), None);

        std::env::set_var(key, "\"확인 필요\"");
        assert_eq!(super::env_opt(key), None);

        std::env::set_var(key, "0x0000000000000000000000000000000000000001");
        assert_eq!(
            super::env_opt(key),
            Some("0x0000000000000000000000000000000000000001".to_string())
        );

        std::env::remove_var(key);
    }
}
