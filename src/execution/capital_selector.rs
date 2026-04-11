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
        Self {
            settings,
            rpc,
            flash,
        }
    }

    pub async fn choose(&self, plan: &ExactPlan) -> Result<Option<(CapitalSource, i128)>> {
        let Some(token) = self.settings.token_by_address(plan.input_token) else {
            return Ok(None);
        };
        let self_profit = plan.output_amount as i128 - plan.input_amount as i128;
        if self_profit <= 0 {
            return Ok(None);
        }

        if token.allow_self_funded {
            let self_balance = self.executor_balance(plan.input_token).await.unwrap_or(0);
            if self_balance >= plan.input_amount {
                return Ok(Some((CapitalSource::SelfFunded, self_profit)));
            }
        }

        if token.flash_loan_enabled
            && self.settings.contracts.aave_pool.is_some()
            && plan.input_amount <= self.settings.risk.max_flash_loan
        {
            let flash_fee = match self.flash.fee_for_amount(plan.input_amount).await {
                Ok(fee) => fee,
                Err(_) => return Ok(None),
            };
            let flash_profit = self_profit - flash_fee as i128;
            if flash_profit > 0 {
                return Ok(Some((CapitalSource::FlashLoan, flash_profit)));
            }
        }

        Ok(None)
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
                IERC20::balanceOfCall { account: executor }
                    .abi_encode()
                    .into(),
                "latest",
            )
            .await?;
        let ret = IERC20::balanceOfCall::abi_decode_returns(&raw)?;
        Ok(ret.to_string().parse::<u128>().unwrap_or(u128::MAX))
    }
}
