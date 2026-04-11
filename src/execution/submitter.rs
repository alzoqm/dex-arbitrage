use std::sync::Arc;

use alloy::{
    network::TransactionBuilder,
    providers::{Provider, ProviderBuilder, WalletProvider},
    rpc::types::TransactionRequest,
    signers::local::PrivateKeySigner,
};
use anyhow::Result;
use parking_lot::Mutex;

use crate::{
    config::Settings,
    types::{ExecutablePlan, SubmissionResult},
};

#[derive(Debug, Default, Clone)]
struct ChannelStats {
    submitted: u64,
    failed: u64,
}

#[derive(Debug)]
pub struct Submitter {
    settings: Arc<Settings>,
    stats: Mutex<std::collections::HashMap<String, ChannelStats>>,
}

impl Submitter {
    pub fn new(settings: Arc<Settings>) -> Self {
        Self {
            settings,
            stats: Mutex::new(std::collections::HashMap::new()),
        }
    }

    pub async fn submit(&self, plan: &ExecutablePlan) -> Result<SubmissionResult> {
        let executor = self
            .settings
            .contracts
            .executor_address
            .ok_or_else(|| anyhow::anyhow!("executor address missing"))?;
        let signer: PrivateKeySigner = self
            .settings
            .operator_private_key
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("OPERATOR_PRIVATE_KEY missing"))?
            .parse()?;

        let tx = TransactionRequest::default()
            .with_to(executor)
            .with_nonce(plan.nonce)
            .with_chain_id(self.settings.chain_id)
            .with_input(plan.calldata.clone())
            .with_gas_limit(plan.exact.gas_limit)
            .with_max_priority_fee_per_gas(plan.max_priority_fee_per_gas)
            .with_max_fee_per_gas(plan.max_fee_per_gas);

        for (name, url) in self.channels() {
            let provider = ProviderBuilder::new().wallet(signer.clone()).connect_http(url.parse()?);
            let envelope = tx.clone().build(&provider.wallet()).await?;
            match provider.send_tx_envelope(envelope).await {
                Ok(pending) => {
                    self.bump_success(&name);
                    return Ok(SubmissionResult {
                        tx_hash: *pending.tx_hash(),
                        channel: name,
                    });
                }
                Err(err) => {
                    self.bump_failure(&name);
                    tracing::warn!(channel = %name, error = %err, "submit attempt failed");
                }
            }
        }

        anyhow::bail!("all submit channels failed")
    }

    fn channels(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        match self.settings.chain {
            crate::types::Chain::Base => {
                if let Some(url) = self.settings.rpc.protected_rpc_url.clone() {
                    out.push(("base-protected".to_string(), url));
                }
                out.push(("base-public".to_string(), self.settings.rpc.public_rpc_url.clone()));
                if let Some(url) = self.settings.rpc.fallback_rpc_url.clone() {
                    out.push(("base-fallback".to_string(), url));
                }
            }
            crate::types::Chain::Polygon => {
                if let Some(url) = self.settings.rpc.protected_rpc_url.clone() {
                    out.push(("polygon-private".to_string(), url));
                }
                out.push(("polygon-public".to_string(), self.settings.rpc.public_rpc_url.clone()));
                if let Some(url) = self.settings.rpc.fallback_rpc_url.clone() {
                    out.push(("polygon-fallback".to_string(), url));
                }
            }
        }
        out
    }

    fn bump_success(&self, channel: &str) {
        let mut stats = self.stats.lock();
        let entry = stats.entry(channel.to_string()).or_default();
        entry.submitted += 1;
    }

    fn bump_failure(&self, channel: &str) {
        let mut stats = self.stats.lock();
        let entry = stats.entry(channel.to_string()).or_default();
        entry.failed += 1;
    }
}
