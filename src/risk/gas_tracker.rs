use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use alloy::primitives::U256;
use anyhow::Result;
use parking_lot::Mutex;

use crate::{config::Settings, rpc::RpcClient};

#[derive(Debug, Clone)]
pub struct GasQuote {
    pub max_fee_per_gas: u128,
    pub max_priority_fee_per_gas: u128,
    pub buffered_total_cost_wei: U256,
}

#[derive(Debug, Clone)]
pub struct GasTracker {
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
            ceiling_wei: settings.risk.gas_price_ceiling_wei,
            risk_buffer_pct: settings.risk.gas_risk_buffer_pct,
            cache: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn quote(&self, rpc: &RpcClient, gas_limit: u64) -> Result<GasQuote> {
        let ttl = Duration::from_millis(
            std::env::var("GAS_QUOTE_CACHE_MS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|value| *value > 0)
                .unwrap_or(1_500),
        );
        let cached = self.cache.lock().clone();
        let (base, priority) = if let Some(cached) = cached {
            if cached.fetched_at.elapsed() <= ttl {
                (cached.base, cached.priority)
            } else {
                fetch_gas_quote(rpc).await?
            }
        } else {
            fetch_gas_quote(rpc).await?
        };
        self.cache.lock().replace(CachedGasQuote {
            fetched_at: Instant::now(),
            base,
            priority,
        });
        let max_fee = base.saturating_add(priority);
        let buffered = ((max_fee as f64) * (1.0 + self.risk_buffer_pct)) as u128;
        let buffered_total_cost_wei = U256::from(buffered) * U256::from(gas_limit);
        metrics::gauge!("gas_max_fee_per_gas_wei").set(buffered as f64);
        metrics::gauge!("gas_priority_fee_per_gas_wei").set(priority as f64);
        metrics::gauge!("gas_limit_estimate").set(gas_limit as f64);

        Ok(GasQuote {
            max_fee_per_gas: buffered,
            max_priority_fee_per_gas: priority.min(buffered),
            buffered_total_cost_wei,
        })
    }

    pub fn above_ceiling(&self, quote: &GasQuote) -> bool {
        quote.max_fee_per_gas > self.ceiling_wei
    }
}

async fn fetch_gas_quote(rpc: &RpcClient) -> Result<(u128, u128)> {
    let base = rpc.gas_price().await?;
    let priority = rpc.max_priority_fee_per_gas().await.unwrap_or(base / 20);
    Ok((base, priority))
}
