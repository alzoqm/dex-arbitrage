pub mod admission;
pub mod event_stream;
pub mod factory_scanner;
pub mod pool_fetcher;

use std::{collections::HashMap, sync::Arc};

use alloy::primitives::Address;
use anyhow::Result;

use crate::{
    config::Settings,
    graph::GraphSnapshot,
    rpc::RpcClients,
    types::{BlockRef, PoolState, TokenBehavior, TokenInfo},
};

use self::{
    admission::AdmissionEngine,
    event_stream::EventStream,
    factory_scanner::{DiscoveredPool, FactoryScanner},
    pool_fetcher::PoolFetcher,
};

#[derive(Debug, Clone)]
pub struct DiscoveryOutput {
    pub tokens: Vec<TokenInfo>,
    pub pools: HashMap<Address, PoolState>,
    pub snapshot: GraphSnapshot,
}

#[derive(Debug)]
pub struct DiscoveryManager {
    settings: Arc<Settings>,
    scanner: FactoryScanner,
    fetcher: PoolFetcher,
    admission: AdmissionEngine,
}

impl DiscoveryManager {
    pub fn new(settings: Arc<Settings>, rpc: Arc<RpcClients>) -> Self {
        Self {
            scanner: FactoryScanner::new(settings.clone(), rpc.clone()),
            fetcher: PoolFetcher::new(settings.clone(), rpc.clone()),
            admission: AdmissionEngine::new(settings.clone()),
            settings,
        }
    }

    pub async fn bootstrap(&self) -> Result<DiscoveryOutput> {
        let tokens = self
            .settings
            .tokens
            .iter()
            .map(|token| TokenInfo {
                address: token.address,
                symbol: token.symbol.clone(),
                decimals: token.decimals,
                is_stable: token.is_stable,
                is_cycle_anchor: token.is_cycle_anchor,
                flash_loan_enabled: token.flash_loan_enabled,
                allow_self_funded: token.allow_self_funded,
                behavior: TokenBehavior::default(),
                manual_price_usd_e8: token.manual_price_usd_e8,
                max_position_usd_e8: token.max_position_usd_e8,
                max_flash_loan_usd_e8: token.max_flash_loan_usd_e8,
            })
            .collect::<Vec<_>>();

        let discovered = self.scanner.scan_all().await?;
        let mut pools = HashMap::new();

        for spec in discovered {
            if let Some(pool) = self.fetcher.fetch_pool(&spec).await? {
                if self.admission.admit(&pool) {
                    pools.insert(pool.pool_id, pool);
                }
            }
        }

        let latest = self.scanner.latest_block().await.unwrap_or(0);
        let block_ref = if latest > 0 {
            self.scanner.current_block_ref().await.ok()
        } else {
            None
        };

        let snapshot = GraphSnapshot::build(0, block_ref, tokens.clone(), pools.clone());
        Ok(DiscoveryOutput {
            tokens,
            pools,
            snapshot,
        })
    }

    pub async fn refresh_pools(
        &self,
        current_snapshot_id: u64,
        tokens: Vec<TokenInfo>,
        current_pools: HashMap<Address, PoolState>,
        changed_specs: Vec<DiscoveredPool>,
        block_ref: Option<BlockRef>,
    ) -> Result<(GraphSnapshot, Vec<Address>)> {
        let mut pools = current_pools;
        let mut changed_pool_ids = Vec::new();

        for spec in changed_specs {
            if let Some(pool) = self.fetcher.fetch_pool(&spec).await? {
                if self.admission.admit(&pool) {
                    changed_pool_ids.push(pool.pool_id);
                    pools.insert(pool.pool_id, pool);
                }
            }
        }

        let snapshot = GraphSnapshot::build(current_snapshot_id + 1, block_ref, tokens, pools);
        Ok((snapshot, changed_pool_ids))
    }

    pub fn event_stream(&self, rpc: Arc<RpcClients>) -> EventStream {
        EventStream::new(self.settings.clone(), rpc)
    }
}
