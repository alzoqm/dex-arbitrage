use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use alloy::{signers::local::PrivateKeySigner, sol_types::SolCall};
use anyhow::Result;
use parking_lot::Mutex;

use crate::{
    abi::IERC20,
    config::Settings,
    risk::valuation::{amount_to_usd_e8, calculate_net_profit_before_gas_raw},
    rpc::RpcClients,
    types::{CapitalChoice, CapitalSource, ExactPlan, TokenInfo},
};

#[derive(Debug, Clone)]
pub struct CapitalSelector {
    settings: Arc<Settings>,
    rpc: Arc<RpcClients>,
    balance_cache: Arc<Mutex<HashMap<alloy::primitives::Address, CachedBalance>>>,
}

#[derive(Debug, Clone, Copy)]
struct CachedBalance {
    fetched_at: Instant,
    value: u128,
}

impl CapitalSelector {
    pub fn new(settings: Arc<Settings>, rpc: Arc<RpcClients>) -> Self {
        Self {
            settings,
            rpc,
            balance_cache: Arc::new(Mutex::new(HashMap::new())),
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
        if !token.flash_loan_enabled {
            return Ok(None);
        }
        if self.settings.contracts.aave_pool.is_none() {
            return Ok(None);
        }

        let executor_balance = self.executor_balance(plan.input_token).await.unwrap_or(0);
        let conservative_flash_fee_raw =
            plan.input_amount.saturating_mul(plan.flash_premium_ppm) / 1_000_000;
        let own_amount = executor_balance.min(plan.input_amount);
        let loan_amount = plan.input_amount.saturating_sub(own_amount);
        let actual_flash_fee_raw = loan_amount.saturating_mul(plan.flash_premium_ppm) / 1_000_000;

        Ok(Self::choice_from_balance_and_fees(
            plan,
            token,
            self.settings.contracts.aave_pool.is_some(),
            self.settings.risk.max_flash_loan_usd_e8,
            loan_amount,
            conservative_flash_fee_raw,
            actual_flash_fee_raw,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn choice_from_balance_and_fees(
        plan: &ExactPlan,
        token: &TokenInfo,
        aave_pool_configured: bool,
        global_flash_cap_usd_e8: u128,
        loan_amount: u128,
        conservative_flash_fee_raw: u128,
        actual_flash_fee_raw: u128,
    ) -> Option<CapitalChoice> {
        if !token.flash_loan_enabled {
            return None;
        }
        if loan_amount > plan.input_amount {
            return None;
        }

        let net_profit = calculate_net_profit_before_gas_raw(
            plan.input_amount,
            plan.output_amount,
            conservative_flash_fee_raw,
        );
        if net_profit <= 0 {
            return None;
        }

        let source = if loan_amount == 0 {
            CapitalSource::SelfFunded
        } else {
            if !aave_pool_configured {
                return None;
            }

            let flash_cap_usd_e8 = token
                .max_flash_loan_usd_e8
                .unwrap_or(global_flash_cap_usd_e8);
            let loan_value_usd_e8 = amount_to_usd_e8(loan_amount, token)?;
            if loan_value_usd_e8 > flash_cap_usd_e8 {
                return None;
            }

            if loan_amount == plan.input_amount {
                CapitalSource::FlashLoan
            } else {
                CapitalSource::MixedFlashLoan
            }
        };

        Some(CapitalChoice {
            source,
            loan_amount_raw: loan_amount,
            flash_fee_raw: conservative_flash_fee_raw,
            actual_flash_fee_raw,
            net_profit_before_gas_raw: net_profit,
        })
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
        let ttl = Duration::from_millis(
            std::env::var("EXECUTOR_BALANCE_CACHE_MS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|value| *value > 0)
                .unwrap_or(1_500),
        );
        if let Some(cached) = self.balance_cache.lock().get(&token).copied() {
            if cached.fetched_at.elapsed() <= ttl {
                return Ok(cached.value);
            }
        }

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
        let value = ret.to_string().parse::<u128>().unwrap_or(u128::MAX);
        self.balance_cache.lock().insert(
            token,
            CachedBalance {
                fetched_at: Instant::now(),
                value,
            },
        );
        Ok(value)
    }
}
