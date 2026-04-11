use std::sync::Arc;

use alloy::{signers::local::PrivateKeySigner, sol_types::SolCall};
use anyhow::Result;

use crate::{
    abi::IERC20,
    config::Settings,
    execution::flash_loan::FlashLoanEngine,
    risk::valuation::{amount_to_usd_e8, calculate_net_profit_before_gas_raw},
    rpc::RpcClients,
    types::{CapitalChoice, CapitalSource, ExactPlan, TokenInfo},
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

    pub async fn choose(
        &self,
        plan: &ExactPlan,
        token: &TokenInfo,
    ) -> Result<Option<CapitalChoice>> {
        let gross_profit_raw = plan.gross_profit_raw;
        if gross_profit_raw <= 0 {
            return Ok(None);
        }

        // Check self-funded option
        let self_funded = if token.allow_self_funded {
            let balance = self.executor_balance(plan.input_token).await.unwrap_or(0);
            if balance >= plan.input_amount {
                Some(CapitalChoice {
                    source: CapitalSource::SelfFunded,
                    flash_fee_raw: 0,
                    net_profit_before_gas_raw: gross_profit_raw,
                })
            } else {
                None
            }
        } else {
            None
        };

        // Check flash loan option
        let flash = if token.flash_loan_enabled && self.settings.contracts.aave_pool.is_some() {
            // Check per-token flash cap
            let flash_cap_usd_e8 = token
                .max_flash_loan_usd_e8
                .unwrap_or(self.settings.risk.max_flash_loan_usd_e8);

            match amount_to_usd_e8(plan.input_amount, token) {
                Some(input_usd_e8) if input_usd_e8 <= flash_cap_usd_e8 => {
                    match self.flash.fee_for_amount(plan.input_amount).await {
                        Ok(flash_fee) => {
                            let net_profit = calculate_net_profit_before_gas_raw(
                                plan.input_amount,
                                plan.output_amount,
                                flash_fee,
                            );
                            if net_profit > 0 {
                                Some(CapitalChoice {
                                    source: CapitalSource::FlashLoan,
                                    flash_fee_raw: flash_fee,
                                    net_profit_before_gas_raw: net_profit,
                                })
                            } else {
                                None
                            }
                        }
                        Err(_) => None, // Premium lookup failure - reject flash loan
                    }
                }
                _ => None,
            }
        } else {
            None
        };

        // Choose best option
        Ok(self.best_choice(self_funded, flash))
    }

    fn best_choice(
        &self,
        self_funded: Option<CapitalChoice>,
        flash: Option<CapitalChoice>,
    ) -> Option<CapitalChoice> {
        match (self_funded, flash) {
            (Some(self_opt), Some(flash_opt)) => {
                // Prefer flash loan if it has better net profit
                if flash_opt.net_profit_before_gas_raw > self_opt.net_profit_before_gas_raw {
                    Some(flash_opt)
                } else {
                    Some(self_opt)
                }
            }
            (Some(self_opt), None) => Some(self_opt),
            (None, Some(flash_opt)) => Some(flash_opt),
            (None, None) => None,
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
