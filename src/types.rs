use std::{
    collections::HashMap,
    fmt::{Display, Formatter},
    str::FromStr,
    time::{Duration, SystemTime},
};

use alloy::primitives::{Address, Bytes, B256, U256};
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Chain {
    Base,
    Polygon,
}

impl Chain {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Polygon => "polygon",
        }
    }

    pub fn config_path(&self) -> &'static str {
        match self {
            Self::Base => "config/base.toml",
            Self::Polygon => "config/polygon.toml",
        }
    }

    pub fn executor_env(&self) -> &'static str {
        match self {
            Self::Base => "BASE_EXECUTOR_ADDRESS",
            Self::Polygon => "POLYGON_EXECUTOR_ADDRESS",
        }
    }

    pub fn aave_pool_env(&self) -> &'static str {
        match self {
            Self::Base => "BASE_AAVE_POOL",
            Self::Polygon => "POLYGON_AAVE_POOL",
        }
    }
}

impl FromStr for Chain {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "base" => Ok(Self::Base),
            "polygon" => Ok(Self::Polygon),
            other => anyhow::bail!("unsupported chain: {other}"),
        }
    }
}

impl Display for Chain {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AmmKind {
    UniswapV2Like,
    UniswapV3Like,
    CurvePlain,
    BalancerWeighted,
}

impl AmmKind {
    pub fn from_toml(value: &str) -> anyhow::Result<Self> {
        match value {
            "uniswap_v2_like" => Ok(Self::UniswapV2Like),
            "uniswap_v3_like" => Ok(Self::UniswapV3Like),
            "curve_plain" => Ok(Self::CurvePlain),
            "balancer_weighted" => Ok(Self::BalancerWeighted),
            other => anyhow::bail!("unsupported amm kind: {other}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DiscoveryKind {
    FactoryAllPairs,
    PoolCreatedLogs,
    CurveRegistry,
    BalancerVaultLogs,
    StaticList,
}

impl DiscoveryKind {
    pub fn from_toml(value: &str) -> anyhow::Result<Self> {
        match value {
            "factory_all_pairs" => Ok(Self::FactoryAllPairs),
            "pool_created_logs" => Ok(Self::PoolCreatedLogs),
            "curve_registry" => Ok(Self::CurveRegistry),
            "balancer_vault_logs" => Ok(Self::BalancerVaultLogs),
            "static_list" => Ok(Self::StaticList),
            other => anyhow::bail!("unsupported discovery kind: {other}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalityLevel {
    Pending,
    Sealed,
    Finalized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolAdmissionStatus {
    Allowed,
    Quarantined,
    Excluded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapitalSource {
    SelfFunded,
    FlashLoan,
    MixedFlashLoan,
}

#[derive(Debug, Clone, Copy)]
pub struct CapitalChoice {
    pub source: CapitalSource,
    pub loan_amount_raw: u128,
    pub flash_fee_raw: u128,
    pub actual_flash_fee_raw: u128,
    pub net_profit_before_gas_raw: i128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterType {
    UniswapV2Like = 0,
    UniswapV3Like = 1,
    CurvePlain = 2,
    BalancerWeighted = 3,
}

impl From<AmmKind> for AdapterType {
    fn from(value: AmmKind) -> Self {
        match value {
            AmmKind::UniswapV2Like => Self::UniswapV2Like,
            AmmKind::UniswapV3Like => Self::UniswapV3Like,
            AmmKind::CurvePlain => Self::CurvePlain,
            AmmKind::BalancerWeighted => Self::BalancerWeighted,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct TokenBehavior {
    pub fee_on_transfer: bool,
    pub rebasing: bool,
    pub erc4626: bool,
    pub callback_token: bool,
    pub rate_oraclized: bool,
}

impl TokenBehavior {
    pub fn is_exotic(&self) -> bool {
        self.fee_on_transfer
            || self.rebasing
            || self.erc4626
            || self.callback_token
            || self.rate_oraclized
    }
}

#[derive(Debug, Clone)]
pub struct TokenInfo {
    pub address: Address,
    pub symbol: String,
    pub decimals: u8,
    pub is_stable: bool,
    pub is_cycle_anchor: bool,
    pub flash_loan_enabled: bool,
    pub allow_self_funded: bool,
    pub behavior: TokenBehavior,
    pub manual_price_usd_e8: Option<u64>,
    pub max_position_usd_e8: Option<u128>,
    pub max_flash_loan_usd_e8: Option<u128>,
}

#[derive(Debug, Clone, Copy)]
pub struct LiquidityInfo {
    pub estimated_usd_e8: u64,
    pub safe_capacity_in: u128,
}

impl Default for LiquidityInfo {
    fn default() -> Self {
        Self {
            estimated_usd_e8: 0,
            safe_capacity_in: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PoolHealth {
    pub stale: bool,
    pub paused: bool,
    pub quarantined: bool,
    pub confidence_bps: u16,
    pub last_successful_refresh_block: u64,
    pub last_refresh_at: SystemTime,
    pub recent_revert_count: u32,
}

impl Default for PoolHealth {
    fn default() -> Self {
        Self {
            stale: false,
            paused: false,
            quarantined: false,
            confidence_bps: 10_000,
            last_successful_refresh_block: 0,
            last_refresh_at: SystemTime::UNIX_EPOCH,
            recent_revert_count: 0,
        }
    }
}

impl PoolHealth {
    pub fn healthy(&self, min_confidence_bps: u16, staleness_timeout: Duration) -> bool {
        if self.paused || self.quarantined || self.stale {
            return false;
        }
        if self.confidence_bps < min_confidence_bps {
            return false;
        }
        match self.last_refresh_at.elapsed() {
            Ok(elapsed) => elapsed <= staleness_timeout,
            Err(_) => true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct V2PoolState {
    pub reserve0: u128,
    pub reserve1: u128,
    pub fee_ppm: u32,
}

#[derive(Debug, Clone)]
pub struct V3PoolState {
    pub sqrt_price_x96: U256,
    pub liquidity: u128,
    pub tick: i32,
    pub fee: u32,
    pub tick_spacing: i32,
}

#[derive(Debug, Clone)]
pub struct CurvePoolState {
    pub balances: Vec<u128>,
    pub amp: u128,
    pub fee: u32,
    pub supports_underlying: bool,
}

#[derive(Debug, Clone)]
pub struct BalancerPoolState {
    pub pool_id: B256,
    pub balances: Vec<u128>,
    pub normalized_weights_1e18: Vec<u128>,
    pub swap_fee_ppm: u32,
}

#[derive(Debug, Clone)]
pub enum PoolSpecificState {
    UniswapV2Like(V2PoolState),
    UniswapV3Like(V3PoolState),
    CurvePlain(CurvePoolState),
    BalancerWeighted(BalancerPoolState),
}

#[derive(Debug, Clone)]
pub struct PoolState {
    pub pool_id: Address,
    pub dex_name: String,
    pub kind: AmmKind,
    pub token_addresses: Vec<Address>,
    pub token_symbols: Vec<String>,
    pub factory: Option<Address>,
    pub registry: Option<Address>,
    pub vault: Option<Address>,
    pub quoter: Option<Address>,
    pub admission_status: PoolAdmissionStatus,
    pub health: PoolHealth,
    pub state: PoolSpecificState,
    pub last_updated_block: u64,
    pub extras: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EdgeRef {
    pub from: usize,
    pub edge_idx: usize,
}

#[derive(Debug, Clone)]
pub struct Edge {
    pub from: usize,
    pub to: usize,
    pub pool_id: Address,
    pub amm_kind: AmmKind,
    pub fee_ppm: u32,
    pub weight_log_q32: i64,
    pub spot_rate_q128: U256,
    pub liquidity: LiquidityInfo,
    pub pool_health: PoolHealth,
    pub dex_name: String,
}

#[derive(Debug, Clone)]
pub struct BlockRef {
    pub number: u64,
    pub hash: B256,
    pub parent_hash: B256,
    pub finality: FinalityLevel,
}

#[derive(Debug, Clone)]
pub struct CandidateHop {
    pub from: Address,
    pub to: Address,
    pub pool_id: Address,
    pub amm_kind: AmmKind,
    pub dex_name: String,
}

#[derive(Debug, Clone)]
pub struct CandidatePath {
    pub snapshot_id: u64,
    pub start_token: Address,
    pub start_symbol: String,
    pub screening_score_q32: i64,
    pub cycle_key: String,
    pub path: SmallVec<[CandidateHop; 8]>,
}

#[derive(Debug, Clone)]
pub enum SplitExtra {
    None,
    V2 {
        fee_ppm: u32,
    },
    V3 {
        zero_for_one: bool,
        sqrt_price_limit_x96: U256,
    },
    Curve {
        i: i128,
        j: i128,
        underlying: bool,
    },
    Balancer {
        pool_id: B256,
    },
}

#[derive(Debug, Clone)]
pub struct SplitPlan {
    pub dex_name: String,
    pub pool_id: Address,
    pub adapter_type: AdapterType,
    pub token_in: Address,
    pub token_out: Address,
    pub amount_in: u128,
    pub min_amount_out: u128,
    pub expected_amount_out: u128,
    pub extra: SplitExtra,
}

#[derive(Debug, Clone)]
pub struct HopPlan {
    pub token_in: Address,
    pub token_out: Address,
    pub total_in: u128,
    pub total_out: u128,
    pub splits: Vec<SplitPlan>,
}

#[derive(Debug, Clone)]
pub struct ExactPlan {
    pub snapshot_id: u64,
    pub input_token: Address,
    pub output_token: Address,
    pub input_amount: u128,
    pub output_amount: u128,

    // Raw input-token units (for contract)
    pub gross_profit_raw: i128,
    pub flash_premium_ppm: u128,
    pub flash_fee_raw: u128,
    pub net_profit_before_gas_raw: i128,
    pub contract_min_profit_raw: u128,

    // USD e8 decision units (for off-chain risk checks)
    pub input_value_usd_e8: u128,
    pub flash_loan_value_usd_e8: u128,
    pub gross_profit_usd_e8: i128,
    pub flash_fee_usd_e8: i128,
    pub actual_flash_fee_usd_e8: i128,
    pub gas_cost_usd_e8: i128,
    pub net_profit_usd_e8: i128,

    // Legacy field for backward compatibility during transition
    pub expected_profit: i128,

    pub gas_limit: u64,
    pub gas_cost_wei: U256,
    pub capital_source: CapitalSource,
    pub flash_loan_amount: u128,
    pub actual_flash_fee_raw: u128,
    pub hops: Vec<HopPlan>,
}

#[derive(Debug, Clone)]
pub struct ExecutablePlan {
    pub exact: ExactPlan,
    pub calldata: Bytes,
    pub max_fee_per_gas: u128,
    pub max_priority_fee_per_gas: u128,
    pub nonce: u64,
    pub deadline_unix: u64,
}

#[derive(Debug, Clone)]
pub struct SubmissionResult {
    pub tx_hash: B256,
    pub channel: String,
}

#[derive(Debug, Clone)]
pub struct RefreshTrigger {
    pub pool_id: Option<Address>,
    pub full_refresh: bool,
    pub source: String,
}

#[derive(Debug, Clone, Default)]
pub struct RefreshBatch {
    pub triggers: Vec<RefreshTrigger>,
}

#[derive(Debug, Clone)]
pub struct RefreshResult {
    pub snapshot_id: u64,
    pub changed_edges: Vec<EdgeRef>,
    pub refreshed_pools: usize,
    pub block_ref: Option<BlockRef>,
}
