use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use alloy::primitives::{keccak256, Address, B256};
use anyhow::Result;

use crate::{
    config::Settings,
    graph::GraphSnapshot,
    rpc::{RpcClients, RpcLog},
    types::{RefreshBatch, RefreshTrigger},
};

#[derive(Debug)]
pub struct EventStream {
    rpc: Arc<RpcClients>,
}

impl EventStream {
    pub fn new(_settings: Arc<Settings>, rpc: Arc<RpcClients>) -> Self {
        Self { rpc }
    }

    pub async fn poll_once(
        &self,
        snapshot: &GraphSnapshot,
        from_block: u64,
    ) -> Result<(u64, RefreshBatch)> {
        let latest = self.rpc.best_read().block_number().await?;
        if latest <= from_block {
            return Ok((from_block, RefreshBatch::default()));
        }

        let addresses = watched_addresses(snapshot);
        if addresses.is_empty() {
            return Ok((latest, RefreshBatch::default()));
        }

        let logs = self
            .get_logs_chunked(from_block + 1, latest, &addresses, &event_topics())
            .await?;
        let batch = self.into_refresh_batch(snapshot, logs);
        Ok((latest, batch))
    }

    async fn get_logs_chunked(
        &self,
        from_block: u64,
        to_block: u64,
        addresses: &[Address],
        topics: &[B256],
    ) -> Result<Vec<RpcLog>> {
        if from_block > to_block {
            return Ok(Vec::new());
        }
        let chunk_blocks = std::env::var("EVENT_LOG_CHUNK_BLOCKS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(10_000);
        let address_chunk_size = std::env::var("EVENT_LOG_ADDRESS_CHUNK_SIZE")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(200);
        let mut logs = Vec::new();
        let mut start = from_block;
        while start <= to_block {
            let end = start.saturating_add(chunk_blocks - 1).min(to_block);
            for address_chunk in addresses.chunks(address_chunk_size) {
                let mut chunk = self
                    .rpc
                    .best_read()
                    .get_logs(start, end, address_chunk, topics)
                    .await?;
                logs.append(&mut chunk);
            }
            if end == u64::MAX {
                break;
            }
            start = end.saturating_add(1);
        }
        Ok(logs)
    }

    fn into_refresh_batch(&self, snapshot: &GraphSnapshot, logs: Vec<RpcLog>) -> RefreshBatch {
        let mut unique = HashSet::new();
        let balancer_map = snapshot
            .pools
            .values()
            .filter_map(|pool| match &pool.state {
                crate::types::PoolSpecificState::BalancerWeighted(state) => {
                    Some((state.pool_id, pool.pool_id))
                }
                _ => None,
            })
            .collect::<HashMap<_, _>>();

        let mut triggers = Vec::new();
        for log in logs {
            if snapshot.pools.contains_key(&log.address) {
                if unique.insert(log.address) {
                    triggers.push(RefreshTrigger {
                        pool_id: Some(log.address),
                        full_refresh: false,
                        source: "pool-log".to_string(),
                    });
                }
                continue;
            }

            if let Some(topic1) = log.topics.get(1) {
                if let Some(pool_id) = balancer_map.get(topic1).copied() {
                    if unique.insert(pool_id) {
                        triggers.push(RefreshTrigger {
                            pool_id: Some(pool_id),
                            full_refresh: false,
                            source: "balancer-vault-log".to_string(),
                        });
                    }
                }
            }
        }

        RefreshBatch { triggers }
    }
}

fn watched_addresses(snapshot: &GraphSnapshot) -> Vec<Address> {
    let mut addresses = snapshot.pools.keys().copied().collect::<Vec<_>>();
    for pool in snapshot.pools.values() {
        if let Some(vault) = pool.vault {
            if !addresses.contains(&vault) {
                addresses.push(vault);
            }
        }
    }
    addresses
}

fn event_topics() -> Vec<B256> {
    [
        "Sync(uint112,uint112)",
        "Swap(address,address,int256,int256,uint160,uint128,int24)",
        "Mint(address,address,int24,int24,uint128,uint256,uint256)",
        "Burn(address,int24,int24,uint128,uint256,uint256)",
        "Initialize(uint160,int24)",
        "TokenExchange(address,int128,uint256,int128,uint256)",
        "TokenExchangeUnderlying(address,int128,uint256,int128,uint256)",
        "AddLiquidity(address,uint256[8],uint256[8],uint256,uint256)",
        "RemoveLiquidity(address,uint256[8],uint256[8],uint256)",
        "RemoveLiquidityOne(address,uint256,int128,uint256)",
        "RemoveLiquidityImbalance(address,uint256[8],uint256[8],uint256,uint256)",
        "RampA(uint256,uint256,uint256,uint256)",
        "StopRampA(uint256,uint256)",
        "Swap(bytes32,address,address,uint256,uint256)",
        "PoolBalanceChanged(bytes32,address,address[],int256[],uint256[])",
    ]
    .into_iter()
    .map(|sig| keccak256(sig.as_bytes()))
    .collect()
}
