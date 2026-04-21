use std::sync::Arc;

use alloy::{
    primitives::{Address, Bytes, U256},
    sol_types::SolCall,
};
use anyhow::Result;

use crate::{
    abi::IArbitrageExecutor,
    config::Settings,
    types::{CapitalSource, ExactPlan, SplitExtra},
};

#[derive(Debug, Clone)]
pub struct TxBuilder {
    settings: Arc<Settings>,
}

impl TxBuilder {
    pub fn new(settings: Arc<Settings>) -> Self {
        Self { settings }
    }

    pub fn build_calldata(&self, plan: &ExactPlan, deadline_unix: u64) -> Result<Bytes> {
        let hops = plan
            .hops
            .iter()
            .map(|hop| IArbitrageExecutor::Hop {
                splits: hop
                    .splits
                    .iter()
                    .map(|split| IArbitrageExecutor::Split {
                        adapterType: map_adapter(split.adapter_type),
                        target: split.pool_id,
                        tokenIn: split.token_in,
                        tokenOut: split.token_out,
                        amountIn: U256::saturating_from(split.amount_in),
                        minAmountOut: U256::saturating_from(split.min_amount_out),
                        extraData: encode_extra(&split.extra),
                    })
                    .collect(),
            })
            .collect::<Vec<_>>();

        let execution = IArbitrageExecutor::ExecutionParams {
            inputToken: plan.input_token,
            inputAmount: U256::saturating_from(plan.input_amount),
            hops,
            minProfit: U256::saturating_from(plan.contract_min_profit_raw),
            deadline: U256::saturating_from(deadline_unix),
            snapshotId: plan.snapshot_id,
        };

        let calldata = match plan.capital_source {
            CapitalSource::SelfFunded => {
                IArbitrageExecutor::executeSelfFundedCall { params: execution }.abi_encode()
            }
            CapitalSource::FlashLoan | CapitalSource::MixedFlashLoan => {
                IArbitrageExecutor::executeFlashLoanCall {
                    params: IArbitrageExecutor::FlashLoanParams {
                        loanAsset: plan.input_token,
                        loanAmount: U256::saturating_from(plan.flash_loan_amount),
                        execution,
                    },
                }
                .abi_encode()
            }
        };
        Ok(calldata.into())
    }

    pub fn executor_address(&self) -> Result<Address> {
        self.settings
            .contracts
            .executor_address
            .ok_or_else(|| anyhow::anyhow!("executor address missing"))
    }
}

fn map_adapter(adapter: crate::types::AdapterType) -> IArbitrageExecutor::AdapterType {
    match adapter {
        crate::types::AdapterType::UniswapV2Like => IArbitrageExecutor::AdapterType::UniswapV2Like,
        crate::types::AdapterType::UniswapV3Like => IArbitrageExecutor::AdapterType::UniswapV3Like,
        crate::types::AdapterType::TraderJoeLb => IArbitrageExecutor::AdapterType::TraderJoeLb,
        crate::types::AdapterType::CurvePlain => IArbitrageExecutor::AdapterType::CurvePlain,
        crate::types::AdapterType::BalancerWeighted => {
            IArbitrageExecutor::AdapterType::BalancerWeighted
        }
        crate::types::AdapterType::AerodromeV2Like => {
            IArbitrageExecutor::AdapterType::AerodromeV2Like
        }
    }
}

fn encode_extra(extra: &SplitExtra) -> Bytes {
    match extra {
        SplitExtra::None => Bytes::new(),
        SplitExtra::V2 { fee_ppm } => {
            let mut out = vec![0u8; 32];
            out[28..32].copy_from_slice(&fee_ppm.to_be_bytes());
            out.into()
        }
        SplitExtra::AerodromeV2 { stable, fee_ppm } => {
            let mut out = vec![0u8; 64];
            out[31] = u8::from(*stable);
            out[60..64].copy_from_slice(&fee_ppm.to_be_bytes());
            out.into()
        }
        SplitExtra::V3 {
            sqrt_price_limit_x96,
            ..
        } => sqrt_price_limit_x96.to_be_bytes::<32>().to_vec().into(),
        SplitExtra::TraderJoeLb => Bytes::new(),
        SplitExtra::Curve { i, j, underlying } => {
            let mut out = Vec::new();
            out.extend_from_slice(&i.to_be_bytes());
            out.extend_from_slice(&j.to_be_bytes());
            out.push(u8::from(*underlying));
            out.into()
        }
        SplitExtra::Balancer { pool_id } => pool_id.as_slice().to_vec().into(),
    }
}

#[cfg(test)]
mod tests {
    use alloy::primitives::{Bytes, U256};

    use crate::types::SplitExtra;

    use super::encode_extra;

    #[test]
    fn v3_extra_encodes_sqrt_price_limit() {
        let raw = encode_extra(&SplitExtra::V3 {
            zero_for_one: true,
            sqrt_price_limit_x96: U256::from(123u64),
        });
        let bytes: &[u8] = raw.as_ref();

        assert_eq!(bytes.len(), 32);
        assert_eq!(U256::from_be_slice(bytes), U256::from(123u64));
    }

    #[test]
    fn none_extra_stays_empty() {
        let raw: Bytes = encode_extra(&SplitExtra::None);
        assert!(raw.is_empty());
    }

    #[test]
    fn v3_extra_encodes_zero_as_full_width_big_endian() {
        let raw = encode_extra(&SplitExtra::V3 {
            zero_for_one: false,
            sqrt_price_limit_x96: U256::ZERO,
        });
        let bytes: &[u8] = raw.as_ref();

        assert_eq!(bytes.len(), 32);
        assert!(bytes.iter().all(|byte| *byte == 0));
    }

    #[test]
    fn v3_extra_encodes_max_value_without_truncation() {
        let raw = encode_extra(&SplitExtra::V3 {
            zero_for_one: true,
            sqrt_price_limit_x96: U256::MAX,
        });
        let bytes: &[u8] = raw.as_ref();

        assert_eq!(bytes.len(), 32);
        assert!(bytes.iter().all(|byte| *byte == 0xff));
    }
}
