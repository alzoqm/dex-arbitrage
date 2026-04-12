use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use alloy::{
    eips::Encodable2718,
    network::TransactionBuilder,
    primitives::B256,
    providers::{ProviderBuilder, WalletProvider},
    rpc::types::TransactionRequest,
    signers::local::PrivateKeySigner,
};
use anyhow::Result;
use parking_lot::Mutex;
use tokio::time::sleep;

use crate::{
    config::Settings,
    rpc::RpcClients,
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
    rpc: Arc<RpcClients>,
    stats: Mutex<std::collections::HashMap<String, ChannelStats>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptOutcome {
    Included,
    Reverted,
    TimedOut,
}

impl Submitter {
    pub fn new(settings: Arc<Settings>, rpc: Arc<RpcClients>) -> Self {
        Self {
            settings,
            rpc,
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

        for channel in self.channels() {
            let provider = ProviderBuilder::new()
                .wallet(signer.clone())
                .connect_http(channel.url.parse()?);
            let envelope = tx.clone().build(&provider.wallet()).await?;
            let raw_tx: alloy::primitives::Bytes = envelope.encoded_2718().into();
            let client = crate::rpc::RpcClient::new(channel.url.clone())?;
            match client
                .send_raw_transaction_with_method(&channel.method, &raw_tx.to_string())
                .await
            {
                Ok(tx_hash) => {
                    self.bump_success(&channel.name);
                    return Ok(SubmissionResult {
                        tx_hash,
                        channel: channel.name,
                    });
                }
                Err(err) => {
                    self.bump_failure(&channel.name);
                    tracing::warn!(
                        channel = %channel.name,
                        method = %channel.method,
                        error = %err,
                        "submit attempt failed"
                    );
                }
            }
        }

        anyhow::bail!("all submit channels failed")
    }

    pub async fn wait_for_receipt(&self, tx_hash: B256) -> Result<ReceiptOutcome> {
        let timeout = Duration::from_millis(env_u64("TX_RECEIPT_TIMEOUT_MS", 120_000));
        let poll_interval = Duration::from_millis(env_u64("TX_RECEIPT_POLL_MS", 1_000));
        let started = Instant::now();

        loop {
            if let Some(success) = self
                .rpc
                .best_read()
                .get_transaction_receipt_status(tx_hash)
                .await?
            {
                return Ok(if success {
                    ReceiptOutcome::Included
                } else {
                    ReceiptOutcome::Reverted
                });
            }
            if started.elapsed() >= timeout {
                return Ok(ReceiptOutcome::TimedOut);
            }
            sleep(poll_interval).await;
        }
    }

    fn channels(&self) -> Vec<SubmitChannel> {
        let mut out = Vec::new();
        match self.settings.chain {
            crate::types::Chain::Base => {
                if let Some(url) = self.settings.rpc.protected_rpc_url.clone() {
                    out.push(SubmitChannel::private(
                        "base-protected",
                        url,
                        &self.settings,
                    ));
                }
                out.push(SubmitChannel::public(
                    "base-public",
                    self.settings.rpc.public_rpc_url.clone(),
                ));
                if let Some(url) = self.settings.rpc.fallback_rpc_url.clone() {
                    out.push(SubmitChannel::public("base-fallback", url));
                }
            }
            crate::types::Chain::Polygon => {
                if let Some(url) = self.settings.rpc.protected_rpc_url.clone() {
                    out.push(SubmitChannel::private(
                        "polygon-private",
                        url,
                        &self.settings,
                    ));
                }
                out.push(SubmitChannel::public(
                    "polygon-public",
                    self.settings.rpc.public_rpc_url.clone(),
                ));
                if let Some(url) = self.settings.rpc.fallback_rpc_url.clone() {
                    out.push(SubmitChannel::public("polygon-fallback", url));
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

#[derive(Debug, Clone)]
struct SubmitChannel {
    name: String,
    url: String,
    method: String,
}

impl SubmitChannel {
    fn public(name: &str, url: String) -> Self {
        Self {
            name: name.to_string(),
            url,
            method: "eth_sendRawTransaction".to_string(),
        }
    }

    fn private(name: &str, url: String, settings: &Settings) -> Self {
        Self {
            name: name.to_string(),
            url,
            method: settings.rpc.private_submit_method.clone(),
        }
    }
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use alloy::primitives::Address;

    use crate::{
        config::{
            ContractSettings, RiskSettings, RpcSettings, SearchSettings, Settings, TokenConfig,
            UniversePolicy,
        },
        types::Chain,
    };

    use super::Submitter;

    fn test_settings(chain: Chain, private_submit_method: &str) -> Arc<Settings> {
        Arc::new(Settings {
            chain,
            chain_id: 8453,
            native_symbol: "ETH".to_string(),
            operator_private_key: Some(
                "0x0123456789012345678901234567890123456789012345678901234567890123".to_string(),
            ),
            deployer_private_key: None,
            safe_owner: None,
            simulation_only: true,
            json_logs: false,
            prometheus_bind: "127.0.0.1:9000".to_string(),
            rpc: RpcSettings {
                public_rpc_url: "http://127.0.0.1:8545".to_string(),
                fallback_rpc_url: Some("http://127.0.0.1:8546".to_string()),
                preconf_rpc_url: None,
                ws_url: None,
                protected_rpc_url: Some("http://127.0.0.1:8547".to_string()),
                private_submit_method: private_submit_method.to_string(),
                simulate_method: "eth_call".to_string(),
            },
            contracts: ContractSettings {
                executor_address: Some(Address::repeat_byte(0x11)),
                aave_pool: None,
                strict_target_allowlist: false,
            },
            risk: RiskSettings {
                max_hops: 3,
                screening_margin_bps: 100,
                min_net_profit: 0,
                min_net_profit_usd_e8: 0,
                min_trade_usd_e8: 0,
                poll_interval_ms: 1000,
                event_backfill_blocks: 0,
                staleness_timeout_ms: 60_000,
                gas_risk_buffer_pct: 0.0,
                gas_price_ceiling_wei: 0,
                max_position: 0,
                max_position_usd_e8: 0,
                max_flash_loan: 0,
                max_flash_loan_usd_e8: 0,
                daily_loss_limit: 0,
                daily_loss_limit_usd_e8: 0,
                min_profit_realization_bps: 9000,
                max_concurrent_tx: 1,
                pool_health_min_bps: 0,
                stable_depeg_cutoff_e6: 0,
            },
            search: SearchSettings::default(),
            policy: UniversePolicy::default(),
            tokens: vec![TokenConfig {
                symbol: "ETH".to_string(),
                address: Address::repeat_byte(0x22),
                decimals: 18,
                is_stable: false,
                is_cycle_anchor: false,
                flash_loan_enabled: false,
                allow_self_funded: true,
                manual_price_usd_e8: Some(1_000_000_000),
                max_position_usd_e8: None,
                max_flash_loan_usd_e8: None,
            }],
            dexes: vec![],
        })
    }

    #[test]
    fn private_submit_channel_uses_configured_method() {
        let submitter = Submitter::new(
            test_settings(Chain::Base, "eth_sendPrivateTransaction"),
            Arc::new(crate::rpc::RpcClients {
                public: Arc::new(
                    crate::rpc::RpcClient::new("http://127.0.0.1:8545".to_string())
                        .expect("rpc client"),
                ),
                fallback: None,
                preconf: None,
                protected: None,
            }),
        );

        let channels = submitter.channels();
        assert_eq!(channels.len(), 3);

        assert_eq!(channels[0].name, "base-protected");
        assert_eq!(channels[0].method, "eth_sendPrivateTransaction");
        assert_eq!(channels[1].name, "base-public");
        assert_eq!(channels[1].method, "eth_sendRawTransaction");
        assert_eq!(channels[2].name, "base-fallback");
        assert_eq!(channels[2].method, "eth_sendRawTransaction");
    }

    #[test]
    fn polygon_private_channel_keeps_private_method_separate_from_public_paths() {
        let submitter = Submitter::new(
            test_settings(Chain::Polygon, "eth_sendPrivateTransaction"),
            Arc::new(crate::rpc::RpcClients {
                public: Arc::new(
                    crate::rpc::RpcClient::new("http://127.0.0.1:8545".to_string())
                        .expect("rpc client"),
                ),
                fallback: None,
                preconf: None,
                protected: None,
            }),
        );

        let channels = submitter.channels();
        assert_eq!(channels[0].name, "polygon-private");
        assert_eq!(channels[0].method, "eth_sendPrivateTransaction");
        assert!(channels[1..]
            .iter()
            .all(|channel| channel.method == "eth_sendRawTransaction"));
    }
}
