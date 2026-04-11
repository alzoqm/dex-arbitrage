use std::sync::Arc;

use alloy::primitives::{Address, Bytes};
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
                        amountIn: split.amount_in.into(),
                        minAmountOut: split.min_amount_out.into(),
                        extraData: encode_extra(&split.extra),
                    })
                    .collect(),
            })
            .collect::<Vec<_>>();

        let execution = IArbitrageExecutor::ExecutionParams {
            inputToken: plan.input_token,
            inputAmount: plan.input_amount.into(),
            hops,
            minProfit: (((plan.expected_profit as f64) * 0.90).max(0.0) as u128).into(),
            deadline: (deadline_unix as u128).into(),
            snapshotId: plan.snapshot_id,
        };

        let calldata = match plan.capital_source {
            CapitalSource::SelfFunded => IArbitrageExecutor::executeSelfFundedCall { params: execution }
                .abi_encode(),
            CapitalSource::FlashLoan => IArbitrageExecutor::executeFlashLoanCall {
                params: IArbitrageExecutor::FlashLoanParams {
                    loanAsset: plan.input_token,
                    loanAmount: plan.input_amount.into(),
                    execution,
                },
            }
            .abi_encode(),
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
        crate::types::AdapterType::CurvePlain => IArbitrageExecutor::AdapterType::CurvePlain,
        crate::types::AdapterType::BalancerWeighted => IArbitrageExecutor::AdapterType::BalancerWeighted,
    }
}

fn encode_extra(extra: &SplitExtra) -> Bytes {
    match extra {
        SplitExtra::None => Bytes::new(),
        SplitExtra::V3 { .. } => Bytes::new(),
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
