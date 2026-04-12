use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use alloy::sol_types::SolCall;
use anyhow::Result;
use parking_lot::Mutex;

use crate::{abi::IAavePool, config::Settings, rpc::RpcClients};

#[derive(Debug, Clone)]
pub struct FlashLoanEngine {
    settings: Arc<Settings>,
    rpc: Arc<RpcClients>,
    premium_cache: Arc<Mutex<Option<CachedPremium>>>,
}

#[derive(Debug, Clone, Copy)]
struct CachedPremium {
    fetched_at: Instant,
    premium_ppm: u128,
}

impl FlashLoanEngine {
    pub fn new(settings: Arc<Settings>, rpc: Arc<RpcClients>) -> Self {
        Self {
            settings,
            rpc,
            premium_cache: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn premium_ppm(&self) -> Result<u128> {
        let ttl = Duration::from_secs(
            std::env::var("FLASH_PREMIUM_CACHE_SECS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|value| *value > 0)
                .unwrap_or(300),
        );
        if let Some(cached) = *self.premium_cache.lock() {
            if cached.fetched_at.elapsed() <= ttl {
                return Ok(cached.premium_ppm);
            }
        }

        let Some(aave_pool) = self.settings.contracts.aave_pool else {
            return Ok(0);
        };
        let raw = self
            .rpc
            .best_read()
            .eth_call(
                aave_pool,
                None,
                IAavePool::FLASHLOAN_PREMIUM_TOTALCall {}
                    .abi_encode()
                    .into(),
                "latest",
            )
            .await?;
        let ret = IAavePool::FLASHLOAN_PREMIUM_TOTALCall::abi_decode_returns(&raw)?;
        let premium_ppm = ret * 100;
        *self.premium_cache.lock() = Some(CachedPremium {
            fetched_at: Instant::now(),
            premium_ppm,
        });
        Ok(premium_ppm)
    }

    pub async fn fee_for_amount(&self, amount: u128) -> Result<u128> {
        let premium_ppm = self.premium_ppm().await?;
        Ok(amount.saturating_mul(premium_ppm) / 1_000_000)
    }
}
