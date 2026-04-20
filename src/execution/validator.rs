use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use serde_json::json;
use tracing::{debug, warn};

use crate::{
    config::Settings,
    execution::{capital_selector::CapitalSelector, tx_builder::TxBuilder},
    graph::GraphSnapshot,
    monitoring::metrics as telemetry,
    risk::{
        gas_tracker::{GasQuote, GasTracker},
        valuation::{amount_to_usd_e8, calculate_contract_min_profit_raw, native_gas_to_usd_e8},
    },
    rpc::RpcClients,
    types::{CapitalSource, Chain, ExactPlan, ExecutablePlan},
};

#[derive(Debug)]
pub struct Validator {
    settings: Arc<Settings>,
    rpc: Arc<RpcClients>,
    gas_tracker: GasTracker,
    tx_builder: TxBuilder,
    capital_selector: CapitalSelector,
    failure_cache: Arc<parking_lot::Mutex<FailureCache>>,
}

#[derive(Debug, Clone)]
struct SimulationOutcome {
    success: bool,
    reason: Option<String>,
}

impl SimulationOutcome {
    fn failed(reason: String) -> Self {
        Self {
            success: false,
            reason: Some(reason),
        }
    }
}

#[derive(Debug, Clone)]
struct CachedFailure {
    expires_at: Instant,
    reason: String,
}

#[derive(Debug)]
struct FailureCache {
    enabled: bool,
    entries: HashMap<String, CachedFailure>,
    slippage_ttl: Duration,
    transfer_ttl: Duration,
    callback_ttl: Duration,
    overflow_ttl: Duration,
    route_ttl: Duration,
}

impl FailureCache {
    fn from_env() -> Self {
        Self {
            enabled: env_bool("VALIDATOR_FAILURE_CACHE", true),
            entries: HashMap::new(),
            slippage_ttl: Duration::from_secs(env_u64("FAILURE_CACHE_SLIPPAGE_SECS", 180)),
            transfer_ttl: Duration::from_secs(env_u64("FAILURE_CACHE_TRANSFER_SECS", 1_800)),
            callback_ttl: Duration::from_secs(env_u64("FAILURE_CACHE_CALLBACK_SECS", 600)),
            overflow_ttl: Duration::from_secs(env_u64("FAILURE_CACHE_OVERFLOW_SECS", 600)),
            route_ttl: Duration::from_secs(env_u64("FAILURE_CACHE_ROUTE_SECS", 300)),
        }
    }

    fn skip_reason(&mut self, plan: &ExactPlan) -> Option<String> {
        if !self.enabled {
            return None;
        }
        self.prune();
        for key in failure_keys(plan) {
            if let Some(failure) = self.entries.get(&key) {
                return Some(failure.reason.clone());
            }
        }
        None
    }

    fn record(&mut self, plan: &ExactPlan, reason: &str) {
        if !self.enabled {
            return;
        }
        self.prune();
        let class = FailureClass::from_reason(reason);
        let ttl = self.ttl_for(class);
        if ttl.is_zero() {
            return;
        }
        let expires_at = Instant::now() + ttl;
        for key in failure_keys_for_class(plan, class) {
            self.entries.insert(
                key,
                CachedFailure {
                    expires_at,
                    reason: normalize_failure_reason(reason).to_string(),
                },
            );
        }
    }

    fn prune(&mut self) {
        let now = Instant::now();
        self.entries.retain(|_, failure| failure.expires_at > now);
    }

    fn ttl_for(&self, class: FailureClass) -> Duration {
        match class {
            FailureClass::Slippage => self.slippage_ttl,
            FailureClass::Transfer => self.transfer_ttl,
            FailureClass::Callback => self.callback_ttl,
            FailureClass::Overflow => self.overflow_ttl,
            FailureClass::HopMismatch => self.route_ttl,
            FailureClass::Other => self.route_ttl,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureClass {
    Slippage,
    Transfer,
    Callback,
    Overflow,
    HopMismatch,
    Other,
}

impl FailureClass {
    fn from_reason(reason: &str) -> Self {
        let normalized = normalize_failure_reason(reason);
        if normalized.contains("SPLIT_SLIPPAGE") {
            Self::Slippage
        } else if normalized.contains("TRANSFER_FAILED") {
            Self::Transfer
        } else if normalized.contains("NO_CALLBACK_PAYMENT") {
            Self::Callback
        } else if normalized.contains("underflow") || normalized.contains("overflow") {
            Self::Overflow
        } else if normalized.contains("HOP_INPUT_SUM_MISMATCH") {
            Self::HopMismatch
        } else {
            Self::Other
        }
    }
}

fn normalize_failure_reason(reason: &str) -> &str {
    reason
        .split("execution reverted: ")
        .nth(1)
        .unwrap_or(reason)
        .split(" for method")
        .next()
        .unwrap_or(reason)
}

fn failure_keys(plan: &ExactPlan) -> Vec<String> {
    let mut keys = vec![route_failure_key(plan)];
    keys.extend(pool_direction_failure_keys(plan));
    keys.extend(token_pair_failure_keys(plan));
    keys
}

fn failure_keys_for_class(plan: &ExactPlan, class: FailureClass) -> Vec<String> {
    match class {
        FailureClass::Transfer => {
            let mut keys = vec![route_failure_key(plan)];
            keys.extend(pool_direction_failure_keys(plan));
            keys.extend(token_pair_failure_keys(plan));
            keys
        }
        FailureClass::Callback => {
            let mut keys = vec![route_failure_key(plan)];
            keys.extend(
                pool_direction_failure_keys(plan)
                    .into_iter()
                    .filter(|key| key.contains(":UniswapV3Like:")),
            );
            keys
        }
        FailureClass::Overflow | FailureClass::Slippage => {
            let mut keys = vec![route_failure_key(plan)];
            keys.extend(pool_direction_failure_keys(plan));
            keys
        }
        FailureClass::HopMismatch | FailureClass::Other => vec![route_failure_key(plan)],
    }
}

fn route_failure_key(plan: &ExactPlan) -> String {
    let mut parts = vec![format!("route:{}", plan.input_token)];
    for hop in &plan.hops {
        for split in &hop.splits {
            parts.push(format!(
                "{}:{}>{}",
                split.pool_id, split.token_in, split.token_out
            ));
        }
    }
    parts.join("|")
}

fn pool_direction_failure_keys(plan: &ExactPlan) -> Vec<String> {
    plan.hops
        .iter()
        .flat_map(|hop| {
            hop.splits.iter().map(|split| {
                format!(
                    "pool:{}:{:?}:{}>{}",
                    split.pool_id, split.adapter_type, split.token_in, split.token_out
                )
            })
        })
        .collect()
}

fn token_pair_failure_keys(plan: &ExactPlan) -> Vec<String> {
    plan.hops
        .iter()
        .flat_map(|hop| {
            hop.splits
                .iter()
                .map(|split| format!("token:{}>{}", split.token_in, split.token_out))
        })
        .collect()
}

fn plan_split_count(plan: &ExactPlan) -> usize {
    plan.hops.iter().map(|hop| hop.splits.len()).sum()
}

fn format_plan_token_route(plan: &ExactPlan) -> String {
    let mut parts = Vec::with_capacity(plan.hops.len() + 1);
    parts.push(plan.input_token.to_string());
    parts.extend(plan.hops.iter().map(|hop| hop.token_out.to_string()));
    parts.join(">")
}

fn format_plan_dex_route(plan: &ExactPlan) -> String {
    plan.hops
        .iter()
        .map(|hop| {
            hop.splits
                .iter()
                .map(|split| split.dex_name.as_str())
                .collect::<Vec<_>>()
                .join("+")
        })
        .collect::<Vec<_>>()
        .join(">")
}

fn format_plan_pool_route(plan: &ExactPlan) -> String {
    plan.hops
        .iter()
        .map(|hop| {
            hop.splits
                .iter()
                .map(|split| split.pool_id.to_string())
                .collect::<Vec<_>>()
                .join("+")
        })
        .collect::<Vec<_>>()
        .join(">")
}

impl Validator {
    pub fn new(settings: Arc<Settings>, rpc: Arc<RpcClients>) -> Self {
        let gas_tracker = GasTracker::new(&settings);
        let tx_builder = TxBuilder::new(settings.clone());
        let capital_selector = CapitalSelector::new(settings.clone(), rpc.clone());
        Self {
            settings,
            rpc,
            gas_tracker,
            tx_builder,
            capital_selector,
            failure_cache: Arc::new(parking_lot::Mutex::new(FailureCache::from_env())),
        }
    }

    pub fn operator_address(&self) -> Result<alloy::primitives::Address> {
        self.capital_selector.operator_address()
    }

    pub async fn prepare(
        &self,
        mut plan: ExactPlan,
        snapshot: &GraphSnapshot,
    ) -> Result<Option<ExecutablePlan>> {
        // Get token metadata for valuation
        let Some(token) = snapshot
            .tokens
            .iter()
            .find(|t| t.address == plan.input_token)
        else {
            debug!(input_token = %plan.input_token, "input token not found in snapshot");
            return Ok(None);
        };

        let Some(input_value_usd_e8) = amount_to_usd_e8(plan.input_amount, token) else {
            debug!(
                input_token = %plan.input_token,
                input_symbol = %token.symbol,
                "input token price missing, cannot apply USD risk checks"
            );
            return Ok(None);
        };
        plan.input_value_usd_e8 = input_value_usd_e8;

        // Step 1: Capital selection (self-funded vs flash loan)
        let Some(choice) = self.capital_selector.choose(&plan, token).await? else {
            debug!(
                input_token = %plan.input_token,
                input_amount = plan.input_amount,
                "no valid capital source available"
            );
            return Ok(None);
        };

        // Update plan with capital choice
        plan.capital_source = choice.source;
        plan.flash_loan_amount = choice.loan_amount_raw;
        plan.flash_fee_raw = choice.flash_fee_raw;
        plan.actual_flash_fee_raw = choice.actual_flash_fee_raw;
        plan.net_profit_before_gas_raw = choice.net_profit_before_gas_raw;

        if let Some(reason) = self.failure_cache.lock().skip_reason(&plan) {
            debug!(
                input_token = %plan.input_token,
                input_amount = plan.input_amount,
                reason = %reason,
                "validator failure cache skipped known-bad route before gas pricing"
            );
            return Ok(None);
        }

        // Step 2: Build preliminary calldata with contract min profit
        plan.contract_min_profit_raw = calculate_contract_min_profit_raw(
            plan.net_profit_before_gas_raw,
            self.settings.risk.min_profit_realization_bps,
        );

        if plan.contract_min_profit_raw == 0 {
            debug!(
                net_profit_before_gas_raw = plan.net_profit_before_gas_raw,
                "contract min profit is zero, rejecting"
            );
            return Ok(None);
        }

        let deadline_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            + 30;

        let calldata = self.tx_builder.build_calldata(&plan, deadline_unix)?;
        let executor = self.tx_builder.executor_address()?;
        let operator = self.capital_selector.operator_address()?;

        // Step 3: Price gas using a conservative local limit by default. Remote
        // estimateGas is optional because reverted candidates are expected in
        // search and retrying them burns latency/CU before the required eth_call
        // simulation gate.
        let static_gas_limit = adjusted_static_gas_limit(plan.gas_limit, plan.capital_source);
        let gas_estimate = if env_bool("USE_GAS_ESTIMATE_RPC", false) {
            match self
                .rpc
                .best_read()
                .estimate_gas(executor, Some(operator), calldata.clone())
                .await
            {
                Ok(remote) => remote.max(static_gas_limit),
                Err(err) => {
                    warn!(
                        error = %err,
                        static_gas_limit,
                        "estimateGas failed; using conservative static gas limit"
                    );
                    static_gas_limit
                }
            }
        } else {
            static_gas_limit
        };

        let gas_quote = self
            .gas_tracker
            .quote(&self.rpc.best_read(), gas_estimate, calldata.len())
            .await?;

        if self.gas_tracker.above_ceiling(&gas_quote) {
            debug!(
                gas_quote_max_fee = gas_quote.max_fee_per_gas,
                gas_ceiling = self.settings.risk.gas_price_ceiling_wei,
                "gas price above ceiling, rejecting"
            );
            return Ok(None);
        }

        // Step 4: Convert all values to USD for profitability check
        let Some(gross_profit_abs_usd_e8) =
            amount_to_usd_e8(plan.gross_profit_raw.unsigned_abs(), token)
        else {
            debug!(
                input_token = %plan.input_token,
                input_symbol = %token.symbol,
                "input token price missing, cannot value gross profit"
            );
            return Ok(None);
        };
        let Ok(gross_profit_abs_usd_e8) = i128::try_from(gross_profit_abs_usd_e8) else {
            debug!("gross profit USD value exceeds i128 range");
            return Ok(None);
        };
        let gross_profit_usd_e8 = if plan.gross_profit_raw >= 0 {
            gross_profit_abs_usd_e8
        } else {
            -gross_profit_abs_usd_e8
        };

        let Some(flash_fee_usd_e8) = amount_to_usd_e8(plan.flash_fee_raw, token) else {
            debug!(
                input_token = %plan.input_token,
                input_symbol = %token.symbol,
                "input token price missing, cannot value flash fee"
            );
            return Ok(None);
        };
        let Ok(flash_fee_usd_e8) = i128::try_from(flash_fee_usd_e8) else {
            debug!("flash fee USD value exceeds i128 range");
            return Ok(None);
        };

        let Some(actual_flash_fee_usd_e8) = amount_to_usd_e8(plan.actual_flash_fee_raw, token)
        else {
            debug!(
                input_token = %plan.input_token,
                input_symbol = %token.symbol,
                "input token price missing, cannot value actual flash fee"
            );
            return Ok(None);
        };
        let Ok(actual_flash_fee_usd_e8) = i128::try_from(actual_flash_fee_usd_e8) else {
            debug!("actual flash fee USD value exceeds i128 range");
            return Ok(None);
        };

        let Some(flash_loan_value_usd_e8) = amount_to_usd_e8(plan.flash_loan_amount, token) else {
            debug!(
                input_token = %plan.input_token,
                input_symbol = %token.symbol,
                "input token price missing, cannot value flash loan amount"
            );
            return Ok(None);
        };

        let Some((native_symbol, native_price_usd_e8, native_decimals)) =
            self.native_gas_pricing(snapshot)
        else {
            debug!(
                chain = %self.settings.chain,
                wrapped_native_symbol = %self.wrapped_native_symbol(),
                "wrapped native token price not configured, cannot compute gas cost in USD"
            );
            return Ok(None);
        };
        let Some(gas_cost_usd_e8) = native_gas_to_usd_e8(
            gas_quote.buffered_total_cost_wei,
            native_price_usd_e8,
            native_decimals,
        ) else {
            debug!(
                native_symbol = %native_symbol,
                "failed to convert gas cost to USD"
            );
            return Ok(None);
        };
        let Ok(gas_cost_usd_e8) = i128::try_from(gas_cost_usd_e8) else {
            debug!("gas cost USD value exceeds i128 range");
            return Ok(None);
        };

        let net_profit_usd_e8 = gross_profit_usd_e8 - actual_flash_fee_usd_e8 - gas_cost_usd_e8;

        // Fill USD fields in plan
        plan.gross_profit_usd_e8 = gross_profit_usd_e8;
        plan.flash_fee_usd_e8 = flash_fee_usd_e8;
        plan.actual_flash_fee_usd_e8 = actual_flash_fee_usd_e8;
        plan.flash_loan_value_usd_e8 = flash_loan_value_usd_e8;
        plan.gas_cost_usd_e8 = gas_cost_usd_e8;
        plan.net_profit_usd_e8 = net_profit_usd_e8;

        // Step 5: Check USD profitability
        if net_profit_usd_e8 < self.settings.risk.min_net_profit_usd_e8 as i128 {
            debug!(
                net_profit_usd_e8 = net_profit_usd_e8,
                min_profit_usd_e8 = self.settings.risk.min_net_profit_usd_e8,
                "net profit below USD threshold, rejecting"
            );
            return Ok(None);
        }

        // Step 6: Recompute contract min profit after gas is known (optional stricter floor)
        plan.contract_min_profit_raw = calculate_contract_min_profit_raw(
            plan.net_profit_before_gas_raw,
            self.settings.risk.min_profit_realization_bps,
        );

        // Rebuild calldata with final contract_min_profit_raw
        let calldata = self.tx_builder.build_calldata(&plan, deadline_unix)?;

        // Step 7: Simulation
        let simulation_started = Instant::now();
        let simulation = self
            .simulate(executor, operator, &calldata, &gas_quote)
            .await?;
        telemetry::record_simulation_latency(
            simulation_started.elapsed().as_secs_f64(),
            simulation.success,
        );

        if !simulation.success {
            if let Some(reason) = &simulation.reason {
                self.failure_cache.lock().record(&plan, reason);
            }
            if env_bool("LOG_FAILED_PLAN_DETAILS", false) {
                warn!(
                    plan = ?plan,
                    "simulation failed plan details"
                );
            }
            let reason = simulation
                .reason
                .as_deref()
                .map(normalize_failure_reason)
                .unwrap_or("unknown");
            if env_bool("LOG_VALIDATION_FAILURE_SUMMARY", true) {
                warn!(
                    reason = %reason,
                    input_token = %plan.input_token,
                    input_amount = plan.input_amount,
                    net_profit_usd_e8 = net_profit_usd_e8,
                    gross_profit_raw = plan.gross_profit_raw,
                    flash_fee_raw = plan.actual_flash_fee_raw,
                    hop_count = plan.hops.len(),
                    split_count = plan_split_count(&plan),
                    token_route = %format_plan_token_route(&plan),
                    dex_route = %format_plan_dex_route(&plan),
                    pool_route = %format_plan_pool_route(&plan),
                    "simulation failed for executable plan"
                );
            } else {
                warn!(
                    reason = %reason,
                    input_token = %plan.input_token,
                    input_amount = plan.input_amount,
                    net_profit_usd_e8 = net_profit_usd_e8,
                    "simulation failed for executable plan"
                );
            }
            return Ok(None);
        }

        // Step 8: Return executable plan
        Ok(Some(ExecutablePlan {
            exact: ExactPlan {
                gas_limit: gas_estimate,
                gas_l2_execution_cost_wei: gas_quote.l2_execution_cost_wei,
                gas_l1_data_fee_wei: gas_quote.l1_data_fee_wei,
                gas_cost_wei: gas_quote.buffered_total_cost_wei,
                ..plan
            },
            calldata,
            max_fee_per_gas: gas_quote.max_fee_per_gas,
            max_priority_fee_per_gas: gas_quote.max_priority_fee_per_gas,
            nonce: 0,
            deadline_unix,
        }))
    }

    fn wrapped_native_symbol(&self) -> &'static str {
        match self.settings.chain {
            Chain::Base => "WETH",
            Chain::Polygon => "WMATIC",
        }
    }

    fn native_gas_pricing(&self, snapshot: &GraphSnapshot) -> Option<(String, u64, u8)> {
        let wrapped_native = self.wrapped_native_symbol();
        snapshot
            .tokens
            .iter()
            .find(|token| token.symbol.eq_ignore_ascii_case(wrapped_native))
            .and_then(|token| {
                token
                    .manual_price_usd_e8
                    .map(|price| (token.symbol.clone(), price, token.decimals))
            })
    }

    async fn simulate(
        &self,
        executor: alloy::primitives::Address,
        operator: alloy::primitives::Address,
        calldata: &alloy::primitives::Bytes,
        gas_quote: &GasQuote,
    ) -> Result<SimulationOutcome> {
        let best = self.rpc.best_read();
        let method = self.settings.rpc.simulate_method.as_str();

        if method != "eth_call" {
            // Use custom simulation method (eth_simulateV1, eth_callBundle, etc.)
            let custom = best
                .custom_simulate(
                    method,
                    json!([
                        {
                            "from": operator.to_string(),
                            "to": executor.to_string(),
                            "data": calldata.to_string(),
                            "maxFeePerGas": format!("0x{:x}", gas_quote.max_fee_per_gas),
                            "maxPriorityFeePerGas": format!("0x{:x}", gas_quote.max_priority_fee_per_gas),
                        }
                    ]),
                )
                .await;

            return match custom {
                Ok(result) => {
                    // Parse the simulation result
                    match self.parse_simulation_result(result) {
                        Ok(outcome) => Ok(outcome),
                        Err(e) => {
                            warn!(
                                error = %e,
                                "failed to parse simulation result, treating as failure"
                            );
                            Ok(SimulationOutcome::failed(e.to_string()))
                        }
                    }
                }
                Err(e) => {
                    warn!(error = %e, "custom simulation call failed");
                    Ok(SimulationOutcome::failed(e.to_string()))
                }
            };
        }

        // Fallback to standard eth_call
        let result = best
            .eth_call(executor, Some(operator), calldata.clone(), "pending")
            .await;

        match result {
            Ok(return_data) => {
                // Parse the return data to ensure successful execution
                match Self::parse_call_return_data(return_data) {
                    Ok(success) => Ok(SimulationOutcome {
                        success,
                        reason: None,
                    }),
                    Err(e) => {
                        warn!(error = %e, "failed to parse eth_call return data");
                        Ok(SimulationOutcome::failed(e.to_string()))
                    }
                }
            }
            Err(e) => {
                warn!(error = %e, "eth_call failed");
                Ok(SimulationOutcome::failed(e.to_string()))
            }
        }
    }

    /// Parse the result from custom simulation methods
    fn parse_simulation_result(&self, result: serde_json::Value) -> Result<SimulationOutcome> {
        // Handle different response formats from providers

        // Format 1: Alchemy eth_simulateV1 - array of simulation results
        if let Some(array) = result.as_array() {
            if !array.is_empty() {
                let first = &array[0];

                // Check for error in the simulation result
                if let Some(error) = first.get("error") {
                    if error.is_object() {
                        let message = error
                            .get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown error");
                        debug!(%message, "simulation returned error");
                        return Ok(SimulationOutcome::failed(message.to_string()));
                    }
                }

                // Check for revert reason in result
                if let Some(revert_reason) = first.get("revertReason") {
                    if revert_reason.is_string() && !revert_reason.as_str().unwrap().is_empty() {
                        let reason = revert_reason.as_str().unwrap().to_string();
                        warn!(revert = %reason, "simulation reverted");
                        return Ok(SimulationOutcome::failed(reason));
                    }
                }

                // Check if the transaction succeeded
                if let Some(success) = first.get("success") {
                    return Ok(SimulationOutcome {
                        success: success.as_bool().unwrap_or(false),
                        reason: None,
                    });
                }

                // Check for gasUsed (some providers only include it on success)
                if first.get("gasUsed").is_some() {
                    return Ok(SimulationOutcome {
                        success: true,
                        reason: None,
                    });
                }
            }
        }

        // Format 2: Single simulation result object
        if result.is_object() {
            if let Some(error) = result.get("error") {
                if error.is_object() {
                    let message = error
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown error");
                    debug!(%message, "simulation returned error");
                    return Ok(SimulationOutcome::failed(message.to_string()));
                }
            }

            if let Some(success) = result.get("success") {
                return Ok(SimulationOutcome {
                    success: success.as_bool().unwrap_or(false),
                    reason: None,
                });
            }

            if result.get("gasUsed").is_some() {
                return Ok(SimulationOutcome {
                    success: true,
                    reason: None,
                });
            }
        }

        warn!("unable to determine simulation success from result");
        Ok(SimulationOutcome::failed(
            "unknown simulation result".to_string(),
        ))
    }

    /// Parse the return data from eth_call
    fn parse_call_return_data(data: alloy::primitives::Bytes) -> Result<bool> {
        // A successful eth_call to a no-return function returns empty bytes.
        // Reverts are surfaced by the RPC as errors before this parser runs.
        if data.is_empty() {
            return Ok(true);
        }

        if data.len() < 32 {
            return Ok(false);
        }

        let last_byte = data[data.len() - 1];
        Ok(last_byte == 1)
    }
}

fn adjusted_static_gas_limit(base: u64, capital_source: CapitalSource) -> u64 {
    let flash_overhead = env_u64("FLASH_LOAN_GAS_OVERHEAD", 180_000);
    let buffer_bps = env_u64("STATIC_GAS_LIMIT_BUFFER_BPS", 2_000);
    let with_overhead = match capital_source {
        CapitalSource::SelfFunded => base,
        CapitalSource::FlashLoan | CapitalSource::MixedFlashLoan => {
            base.saturating_add(flash_overhead)
        }
    };
    with_overhead.saturating_mul(10_000u64.saturating_add(buffer_bps)) / 10_000
}

fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .ok()
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use alloy::primitives::Bytes;

    use super::Validator;

    #[test]
    fn eth_call_empty_return_data_is_success_for_no_return_functions() {
        assert!(Validator::parse_call_return_data(Bytes::new()).unwrap());
    }

    #[test]
    fn eth_call_bool_return_data_is_parsed() {
        let mut truthy = vec![0u8; 32];
        truthy[31] = 1;
        assert!(Validator::parse_call_return_data(Bytes::from(truthy)).unwrap());

        let falsy = vec![0u8; 32];
        assert!(!Validator::parse_call_return_data(Bytes::from(falsy)).unwrap());
    }
}
