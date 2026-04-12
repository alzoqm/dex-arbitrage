use std::sync::Arc;

use alloy::{
    primitives::{Address, Bytes},
    sol_types::SolCall,
};
use anyhow::Result;

use crate::{abi::IMulticall3, rpc::RpcClients};

pub fn multicall3_address() -> Option<Address> {
    std::env::var("MULTICALL3_ADDRESS")
        .ok()
        .and_then(|value| value.parse().ok())
        .or_else(|| "0xcA11bde05977b3631167028862bE2a173976CA11".parse().ok())
}

pub async fn aggregate3(
    rpc: &Arc<RpcClients>,
    calls: Vec<(Address, Vec<u8>)>,
) -> Result<Vec<Option<Bytes>>> {
    if calls.is_empty() {
        return Ok(Vec::new());
    }
    let Some(multicall) = multicall3_address() else {
        anyhow::bail!("MULTICALL3_ADDRESS is not configured");
    };

    let calls = calls
        .into_iter()
        .map(|(target, call_data)| IMulticall3::Call3 {
            target,
            allowFailure: true,
            callData: call_data.into(),
        })
        .collect::<Vec<_>>();
    let raw = rpc
        .best_read()
        .eth_call(
            multicall,
            None,
            IMulticall3::aggregate3Call { calls }.abi_encode().into(),
            "latest",
        )
        .await?;
    let results = IMulticall3::aggregate3Call::abi_decode_returns(&raw)?;
    Ok(results
        .into_iter()
        .map(|result| result.success.then_some(result.returnData))
        .collect())
}
