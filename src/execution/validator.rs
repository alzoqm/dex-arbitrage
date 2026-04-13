use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use serde_json::json;
use tracing::{debug, warn};

use crate::{
    config::Settings,
    execution::{capital_selector::CapitalSelector, tx_builder::TxBuilder},
    graph::GraphSnapshot,
    risk::{
        gas_tracker::{GasQuote, GasTracker},
        valuation::{amount_to_usd_e8, calculate_contract_min_profit_raw, native_gas_to_usd_e8},
    },
    rpc::RpcClients,
    types::{Chain, ExactPlan, ExecutablePlan},
};

#[derive(Debug)]
pub struct Validator {
    settings: Arc<Settings>,
    rpc: Arc<RpcClients>,
    gas_tracker: GasTracker,
    tx_builder: TxBuilder,
    capital_selector: CapitalSelector,
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

        // Step 3: Estimate gas
        let gas_estimate = self
            .rpc
            .best_read()
            .estimate_gas(executor, Some(operator), calldata.clone())
            .await
            .unwrap_or(plan.gas_limit);

        let gas_quote = self
            .gas_tracker
            .quote(&self.rpc.best_read(), gas_estimate)
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
        let simulation_ok = self
            .simulate(executor, operator, &calldata, &gas_quote)
            .await?;

        if !simulation_ok {
            warn!(
                input_token = %plan.input_token,
                input_amount = plan.input_amount,
                net_profit_usd_e8 = net_profit_usd_e8,
                "simulation failed for executable plan"
            );
            return Ok(None);
        }

        // Step 8: Return executable plan
        Ok(Some(ExecutablePlan {
            exact: ExactPlan {
                gas_limit: gas_estimate,
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
    ) -> Result<bool> {
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
                        Ok(success) => Ok(success),
                        Err(e) => {
                            warn!(
                                error = %e,
                                "failed to parse simulation result, treating as failure"
                            );
                            Ok(false)
                        }
                    }
                }
                Err(e) => {
                    warn!(error = %e, "custom simulation call failed");
                    Ok(false)
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
                    Ok(success) => Ok(success),
                    Err(e) => {
                        warn!(error = %e, "failed to parse eth_call return data");
                        Ok(false)
                    }
                }
            }
            Err(e) => {
                warn!(error = %e, "eth_call failed");
                Ok(false)
            }
        }
    }

    /// Parse the result from custom simulation methods
    fn parse_simulation_result(&self, result: serde_json::Value) -> Result<bool> {
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
                        return Ok(false);
                    }
                }

                // Check for revert reason in result
                if let Some(revert_reason) = first.get("revertReason") {
                    if revert_reason.is_string() && !revert_reason.as_str().unwrap().is_empty() {
                        warn!(revert = %revert_reason.as_str().unwrap(), "simulation reverted");
                        return Ok(false);
                    }
                }

                // Check if the transaction succeeded
                if let Some(success) = first.get("success") {
                    return Ok(success.as_bool().unwrap_or(false));
                }

                // Check for gasUsed (some providers only include it on success)
                if first.get("gasUsed").is_some() {
                    return Ok(true);
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
                    return Ok(false);
                }
            }

            if let Some(success) = result.get("success") {
                return Ok(success.as_bool().unwrap_or(false));
            }

            if result.get("gasUsed").is_some() {
                return Ok(true);
            }
        }

        warn!("unable to determine simulation success from result");
        Ok(false)
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
