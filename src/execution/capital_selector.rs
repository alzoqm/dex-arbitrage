use std::sync::Arc;

use alloy::{signers::local::PrivateKeySigner, sol_types::SolCall};
use anyhow::Result;

use crate::{
    abi::IERC20,
    config::Settings,
    execution::flash_loan::FlashLoanEngine,
    rpc::RpcClients,
    types::{CapitalSource, ExactPlan},
};

#[derive(Debug, Clone)]
pub struct CapitalSelector {
    settings: Arc<Settings>,
    rpc: Arc<RpcClients>,
    flash: FlashLoanEngine,
}

impl CapitalSelector {
    pub fn new(settings: Arc<Settings>, rpc: Arc<RpcClients>) -> Self {
        let flash = FlashLoanEngine::new(settings.clone(), rpc.clone());
        Self { settings, rpc, flash }
    }

    pub async fn choose(&self, plan: &ExactPlan) -> Result<(CapitalSource, i128)> {
        let self_balance = self.executor_balance(plan.input_token).await.unwrap_or(0);
        let flash_fee = self.flash.fee_for_amount(plan.input_amount).await.unwrap_or(0);

        let self_profit = plan.output_amount as i128 - plan.input_amount as i128;
        let flash_profit = self_profit - flash_fee as i128;

        if self_balance >= plan.input_amount && self_profit >= flash_profit {
            Ok((CapitalSource::SelfFunded, self_profit))
        } else if self.settings.contracts.aave_pool.is_some() && plan.input_amount <= self.settings.risk.max_flash_loan {
            Ok((CapitalSource::FlashLoan, flash_profit))
        } else {
            Ok((CapitalSource::SelfFunded, self_profit))
        }
    }

    pub fn operator_address(&self) -> Result<alloy::primitives::Address> {
        let signer: PrivateKeySigner = self
            .settings
            .operator_private_key
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("OPERATOR_PRIVATE_KEY missing"))?
            .parse()?;
        Ok(signer.address())
    }

    async fn executor_balance(&self, token: alloy::primitives::Address) -> Result<u128> {
        let Some(executor) = self.settings.contracts.executor_address else {
            return Ok(0);
        };
        let raw = self
            .rpc
            .best_read()
            .eth_call(
                token,
                None,
                IERC20::balanceOfCall { account: executor }.abi_encode().into(),
                "latest",
            )
            .await?;
        let ret = IERC20::balanceOfCall::abi_decode_returns(&raw, true)?;
        Ok(ret._0.to_string().parse::<u128>().unwrap_or(u128::MAX))
    }
}
