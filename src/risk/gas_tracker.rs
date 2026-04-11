use alloy::primitives::U256;
use anyhow::Result;

use crate::{config::Settings, rpc::RpcClient};

#[derive(Debug, Clone)]
pub struct GasQuote {
    pub max_fee_per_gas: u128,
    pub max_priority_fee_per_gas: u128,
    pub buffered_total_cost_wei: U256,
}

#[derive(Debug, Clone)]
pub struct GasTracker {
    ceiling_wei: u128,
    risk_buffer_pct: f64,
}

impl GasTracker {
    pub fn new(settings: &Settings) -> Self {
        Self {
            ceiling_wei: settings.risk.gas_price_ceiling_wei,
            risk_buffer_pct: settings.risk.gas_risk_buffer_pct,
        }
    }

    pub async fn quote(&self, rpc: &RpcClient, gas_limit: u64) -> Result<GasQuote> {
        let base = rpc.gas_price().await?;
        let priority = rpc.max_priority_fee_per_gas().await.unwrap_or(base / 20);
        let max_fee = base.saturating_add(priority);
        let buffered = ((max_fee as f64) * (1.0 + self.risk_buffer_pct)) as u128;
        let buffered_total_cost_wei = U256::from(buffered) * U256::from(gas_limit);

        Ok(GasQuote {
            max_fee_per_gas: buffered,
            max_priority_fee_per_gas: priority.min(buffered),
            buffered_total_cost_wei,
        })
    }

    pub fn above_ceiling(&self, quote: &GasQuote) -> bool {
        quote.max_fee_per_gas > self.ceiling_wei
    }
}
