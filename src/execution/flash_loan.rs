use std::sync::Arc;

use alloy::sol_types::SolCall;
use anyhow::Result;

use crate::{abi::IAavePool, config::Settings, rpc::RpcClients};

#[derive(Debug, Clone)]
pub struct FlashLoanEngine {
    settings: Arc<Settings>,
    rpc: Arc<RpcClients>,
}

impl FlashLoanEngine {
    pub fn new(settings: Arc<Settings>, rpc: Arc<RpcClients>) -> Self {
        Self { settings, rpc }
    }

    pub async fn premium_ppm(&self) -> Result<u128> {
        let Some(aave_pool) = self.settings.contracts.aave_pool else {
            return Ok(0);
        };
        let raw = self
            .rpc
            .best_read()
            .eth_call(aave_pool, None, IAavePool::FLASHLOAN_PREMIUM_TOTALCall {}.abi_encode().into(), "latest")
            .await?;
        let ret = IAavePool::FLASHLOAN_PREMIUM_TOTALCall::abi_decode_returns(&raw, true)?;
        Ok(ret._0 as u128 * 100)
    }

    pub async fn fee_for_amount(&self, amount: u128) -> Result<u128> {
        let premium_ppm = self.premium_ppm().await?;
        Ok(amount.saturating_mul(premium_ppm) / 1_000_000)
    }
}
