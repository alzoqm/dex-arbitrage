use std::{
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant},
};

use alloy::{primitives::U256, sol_types::SolCall};
use anyhow::Result;
use parking_lot::Mutex;
use tokio::time::sleep;
use tracing::warn;

use crate::{abi::IBaseGasPriceOracle, config::Settings, rpc::RpcClient, types::Chain};

#[derive(Debug, Clone)]
pub struct GasQuote {
    pub max_fee_per_gas: u128,
    pub max_priority_fee_per_gas: u128,
    pub l2_execution_cost_wei: U256,
    pub l1_data_fee_wei: U256,
    pub buffered_total_cost_wei: U256,
}

#[derive(Debug, Clone)]
pub struct GasTracker {
    chain: Chain,
    ceiling_wei: u128,
    risk_buffer_pct: f64,
    cache: Arc<Mutex<Option<CachedGasQuote>>>,
}

#[derive(Debug, Clone)]
struct CachedGasQuote {
    fetched_at: Instant,
    base: u128,
    priority: u128,
}

impl GasTracker {
    pub fn new(settings: &Settings) -> Self {
        Self {
            chain: settings.chain,
            ceiling_wei: settings.risk.gas_price_ceiling_wei,
            risk_buffer_pct: settings.risk.gas_risk_buffer_pct,
            cache: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn quote(
        &self,
        rpc: &RpcClient,
        gas_limit: u64,
        calldata_len: usize,
    ) -> Result<GasQuote> {
        let ttl = Duration::from_millis(
            std::env::var("GAS_QUOTE_CACHE_MS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|value| *value > 0)
                .unwrap_or(1_500),
        );
        let stale_ttl = Duration::from_millis(
            std::env::var("GAS_QUOTE_STALE_CACHE_MS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|value| *value > 0)
                .unwrap_or(30_000),
        );
        let cached = self.cache.lock().clone();
        let (base, priority, fresh) = match cached {
            Some(cached) if cached.fetched_at.elapsed() <= ttl => {
                (cached.base, cached.priority, false)
            }
            Some(cached) => match fetch_gas_quote_with_retry(rpc).await {
                Ok((base, priority)) => (base, priority, true),
                Err(err) if cached.fetched_at.elapsed() <= stale_ttl => {
                    warn!(
                        error = %err,
                        cached_age_ms = cached.fetched_at.elapsed().as_millis(),
                        "gas quote refresh failed; using recent cached gas quote"
                    );
                    (cached.base, cached.priority, false)
                }
                Err(err) => return Err(err),
            },
            None => {
                let (base, priority) = fetch_gas_quote_with_retry(rpc).await?;
                (base, priority, true)
            }
        };
        if fresh {
            self.cache.lock().replace(CachedGasQuote {
                fetched_at: Instant::now(),
                base,
                priority,
            });
        }
        let max_fee = base.saturating_add(priority);
        let buffered = ((max_fee as f64) * (1.0 + self.risk_buffer_pct)) as u128;
        let l2_execution_cost_wei = U256::from(buffered) * U256::from(gas_limit);
        let l1_data_fee_wei = self.estimate_l1_data_fee(rpc, calldata_len).await?;
        let buffered_total_cost_wei = l2_execution_cost_wei.saturating_add(l1_data_fee_wei);
        metrics::gauge!("gas_max_fee_per_gas_wei").set(buffered as f64);
        metrics::gauge!("gas_priority_fee_per_gas_wei").set(priority as f64);
        metrics::gauge!("gas_limit_estimate").set(gas_limit as f64);
        metrics::gauge!("gas_l2_execution_cost_wei").set(u256_to_f64(l2_execution_cost_wei));
        metrics::gauge!("gas_l1_data_fee_wei").set(u256_to_f64(l1_data_fee_wei));

        Ok(GasQuote {
            max_fee_per_gas: buffered,
            max_priority_fee_per_gas: priority.min(buffered),
            l2_execution_cost_wei,
            l1_data_fee_wei,
            buffered_total_cost_wei,
        })
    }

    pub fn above_ceiling(&self, quote: &GasQuote) -> bool {
        quote.max_fee_per_gas > self.ceiling_wei
    }

    async fn estimate_l1_data_fee(&self, rpc: &RpcClient, calldata_len: usize) -> Result<U256> {
        if self.chain != Chain::Base {
            return Ok(U256::ZERO);
        }
        let overhead = std::env::var("BASE_L1_FEE_TX_SIZE_OVERHEAD_BYTES")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(160);
        let tx_size = calldata_len.saturating_add(overhead);
        let oracle =
            alloy::primitives::Address::from_str("0x420000000000000000000000000000000000000F")?;
        let calldata = IBaseGasPriceOracle::getL1FeeUpperBoundCall {
            txSize: U256::from(tx_size),
        }
        .abi_encode()
        .into();
        let raw = eth_call_with_retry(rpc, oracle, calldata).await?;
        Ok(IBaseGasPriceOracle::getL1FeeUpperBoundCall::abi_decode_returns(&raw)?)
    }
}

async fn fetch_gas_quote_with_retry(rpc: &RpcClient) -> Result<(u128, u128)> {
    let attempts = gas_quote_attempts();
    for attempt in 1..=attempts {
        match fetch_gas_quote(rpc).await {
            Ok(quote) => return Ok(quote),
            Err(err) if attempt < attempts => {
                warn!(
                    error = %err,
                    attempt,
                    attempts,
                    "gas quote RPC failed; retrying"
                );
                sleep(gas_quote_retry_delay(attempt)).await;
            }
            Err(err) => return Err(err),
        }
    }
    unreachable!("gas quote attempts is always at least one")
}

async fn fetch_gas_quote(rpc: &RpcClient) -> Result<(u128, u128)> {
    let base = rpc.gas_price().await?;
    let priority = rpc.max_priority_fee_per_gas().await.unwrap_or(base / 20);
    Ok((base, priority))
}

async fn eth_call_with_retry(
    rpc: &RpcClient,
    to: alloy::primitives::Address,
    calldata: alloy::primitives::Bytes,
) -> Result<alloy::primitives::Bytes> {
    let attempts = gas_quote_attempts();
    for attempt in 1..=attempts {
        match rpc.eth_call(to, None, calldata.clone(), "latest").await {
            Ok(raw) => return Ok(raw),
            Err(err) if attempt < attempts => {
                warn!(
                    error = %err,
                    attempt,
                    attempts,
                    "L1 data fee RPC failed; retrying"
                );
                sleep(gas_quote_retry_delay(attempt)).await;
            }
            Err(err) => return Err(err),
        }
    }
    unreachable!("gas quote attempts is always at least one")
}

fn gas_quote_attempts() -> usize {
    std::env::var("GAS_QUOTE_RETRIES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(3)
}

fn gas_quote_retry_delay(attempt: usize) -> Duration {
    let base_ms = std::env::var("GAS_QUOTE_RETRY_DELAY_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(150);
    let multiplier = 1u64 << attempt.saturating_sub(1).min(4);
    Duration::from_millis(base_ms.saturating_mul(multiplier))
}

fn u256_to_f64(value: U256) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(f64::MAX)
}
