use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use serde_json::json;

use crate::{
    config::Settings,
    execution::{capital_selector::CapitalSelector, tx_builder::TxBuilder},
    risk::gas_tracker::{GasQuote, GasTracker},
    rpc::RpcClients,
    types::{ExactPlan, ExecutablePlan},
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

    pub async fn prepare(&self, mut plan: ExactPlan) -> Result<Option<ExecutablePlan>> {
        let (capital_source, capital_profit) = self.capital_selector.choose(&plan).await?;
        plan.capital_source = capital_source;
        plan.expected_profit = capital_profit;
        if plan.expected_profit < self.settings.risk.min_net_profit {
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

        let gas_estimate = self
            .rpc
            .best_read()
            .estimate_gas(executor, Some(operator), calldata.clone())
            .await
            .unwrap_or(plan.gas_limit);
        let gas_quote = self.gas_tracker.quote(&self.rpc.best_read(), gas_estimate).await?;
        if self.gas_tracker.above_ceiling(&gas_quote) {
            return Ok(None);
        }

        let simulation_ok = self
            .simulate(executor, operator, &calldata, &gas_quote)
            .await?;
        if !simulation_ok {
            return Ok(None);
        }

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
            if custom.is_ok() {
                return Ok(true);
            }
        }
        let result = best
            .eth_call(executor, Some(operator), calldata.clone(), "pending")
            .await;
        Ok(result.is_ok())
    }
}
