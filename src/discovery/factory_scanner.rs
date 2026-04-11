use std::sync::Arc;

use alloy::{primitives::{keccak256, Address, B256}, sol_types::SolCall};
use anyhow::{Context, Result};

use crate::{
    abi::{IBalancerVault, ICurveRegistry, IUniswapV2Factory},
    config::{DexConfig, Settings},
    rpc::RpcClients,
    types::{AmmKind, BlockRef, DiscoveryKind, FinalityLevel},
};

#[derive(Debug, Clone)]
pub struct DiscoveredPool {
    pub address: Address,
    pub dex_name: String,
    pub amm_kind: AmmKind,
    pub factory: Option<Address>,
    pub registry: Option<Address>,
    pub vault: Option<Address>,
    pub quoter: Option<Address>,
    pub balancer_pool_id: Option<B256>,
}

#[derive(Debug)]
pub struct FactoryScanner {
    settings: Arc<Settings>,
    rpc: Arc<RpcClients>,
}

impl FactoryScanner {
    pub fn new(settings: Arc<Settings>, rpc: Arc<RpcClients>) -> Self {
        Self { settings, rpc }
    }

    pub async fn scan_all(&self) -> Result<Vec<DiscoveredPool>> {
        let mut pools = Vec::new();
        for dex in self.settings.dexes.iter().filter(|dex| dex.enabled) {
            let mut part = match dex.discovery_kind {
                DiscoveryKind::FactoryAllPairs => self.scan_v2_factory(dex).await?,
                DiscoveryKind::PoolCreatedLogs => self.scan_v3_factory_logs(dex).await?,
                DiscoveryKind::CurveRegistry => self.scan_curve_registry(dex).await?,
                DiscoveryKind::BalancerVaultLogs => self.scan_balancer_vault_logs(dex).await?,
                DiscoveryKind::StaticList => Vec::new(),
            };
            pools.append(&mut part);
        }
        Ok(pools)
    }

    pub async fn latest_block(&self) -> Result<u64> {
        self.rpc.best_read().block_number().await
    }

    pub async fn current_block_ref(&self) -> Result<BlockRef> {
        let (number, hash, parent_hash) = self.rpc.best_read().get_block_by_number("latest").await?;
        Ok(BlockRef {
            number,
            hash,
            parent_hash,
            finality: FinalityLevel::Sealed,
        })
    }

    pub fn spec_for_pool(&self, snapshot: &crate::graph::GraphSnapshot, pool_id: Address) -> Option<DiscoveredPool> {
        let pool = snapshot.pool(pool_id)?;
        Some(DiscoveredPool {
            address: pool.pool_id,
            dex_name: pool.dex_name.clone(),
            amm_kind: pool.kind,
            factory: pool.factory,
            registry: pool.registry,
            vault: pool.vault,
            quoter: pool.quoter,
            balancer_pool_id: match &pool.state {
                crate::types::PoolSpecificState::BalancerWeighted(state) => Some(state.pool_id),
                _ => None,
            },
        })
    }

    async fn scan_v2_factory(&self, dex: &DexConfig) -> Result<Vec<DiscoveredPool>> {
        let Some(factory) = dex.factory else { return Ok(Vec::new()) };
        let data = IUniswapV2Factory::allPairsLengthCall {}.abi_encode();
        let raw = self.rpc.best_read().eth_call(factory, None, data.into(), "latest").await?;
        let ret = IUniswapV2Factory::allPairsLengthCall::abi_decode_returns(&raw, true)
            .context("decode allPairsLength failed")?;
        let len = u256_to_u64(ret._0);

        let mut pools = Vec::with_capacity(len as usize);
        for idx in 0..len {
            let data = IUniswapV2Factory::allPairsCall { index: idx.into() }.abi_encode();
            let raw = self.rpc.best_read().eth_call(factory, None, data.into(), "latest").await?;
            let ret = IUniswapV2Factory::allPairsCall::abi_decode_returns(&raw, true)
                .context("decode allPairs failed")?;
            pools.push(DiscoveredPool {
                address: ret._0,
                dex_name: dex.name.clone(),
                amm_kind: dex.amm_kind,
                factory: dex.factory,
                registry: dex.registry,
                vault: dex.vault,
                quoter: dex.quoter,
                balancer_pool_id: None,
            });
        }
        Ok(pools)
    }

    async fn scan_v3_factory_logs(&self, dex: &DexConfig) -> Result<Vec<DiscoveredPool>> {
        let Some(factory) = dex.factory else { return Ok(Vec::new()) };
        let latest = self.latest_block().await?;
        let topic = signature_hash("PoolCreated(address,address,uint24,int24,address)");
        let logs = self
            .rpc
            .best_read()
            .get_logs(dex.start_block, latest, &[factory], &[topic])
            .await?;

        let mut pools = Vec::new();
        for log in logs {
            let data = log.data.as_ref();
            if data.len() < 64 {
                continue;
            }
            let pool_addr = decode_address_from_word(&data[32..64]);
            pools.push(DiscoveredPool {
                address: pool_addr,
                dex_name: dex.name.clone(),
                amm_kind: dex.amm_kind,
                factory: dex.factory,
                registry: dex.registry,
                vault: dex.vault,
                quoter: dex.quoter,
                balancer_pool_id: None,
            });
        }
        Ok(pools)
    }

    async fn scan_curve_registry(&self, dex: &DexConfig) -> Result<Vec<DiscoveredPool>> {
        let Some(registry) = dex.registry else { return Ok(Vec::new()) };
        let data = ICurveRegistry::pool_countCall {}.abi_encode();
        let raw = self.rpc.best_read().eth_call(registry, None, data.into(), "latest").await?;
        let ret = ICurveRegistry::pool_countCall::abi_decode_returns(&raw, true)
            .context("decode curve pool_count failed")?;
        let len = u256_to_u64(ret._0);
        let mut pools = Vec::with_capacity(len as usize);
        for idx in 0..len {
            let data = ICurveRegistry::pool_listCall { index: idx.into() }.abi_encode();
            let raw = self.rpc.best_read().eth_call(registry, None, data.into(), "latest").await?;
            let ret = ICurveRegistry::pool_listCall::abi_decode_returns(&raw, true)
                .context("decode curve pool_list failed")?;
            pools.push(DiscoveredPool {
                address: ret._0,
                dex_name: dex.name.clone(),
                amm_kind: dex.amm_kind,
                factory: dex.factory,
                registry: dex.registry,
                vault: dex.vault,
                quoter: dex.quoter,
                balancer_pool_id: None,
            });
        }
        Ok(pools)
    }

    async fn scan_balancer_vault_logs(&self, dex: &DexConfig) -> Result<Vec<DiscoveredPool>> {
        let Some(vault) = dex.vault else { return Ok(Vec::new()) };
        let latest = self.latest_block().await?;
        let topic = signature_hash("PoolRegistered(bytes32,address,uint8)");
        let logs = self
            .rpc
            .best_read()
            .get_logs(dex.start_block, latest, &[vault], &[topic])
            .await?;
        let mut pools = Vec::new();
        for log in logs {
            if log.topics.len() < 3 {
                continue;
            }
            let pool_id = log.topics[1];
            let pool_addr = topic_to_address(log.topics[2]);
            pools.push(DiscoveredPool {
                address: pool_addr,
                dex_name: dex.name.clone(),
                amm_kind: dex.amm_kind,
                factory: dex.factory,
                registry: dex.registry,
                vault: dex.vault,
                quoter: dex.quoter,
                balancer_pool_id: Some(pool_id),
            });
        }
        Ok(pools)
    }
}

fn signature_hash(signature: &str) -> B256 {
    keccak256(signature.as_bytes())
}

fn topic_to_address(topic: B256) -> Address {
    let bytes = topic.as_slice();
    Address::from_slice(&bytes[12..32])
}

fn decode_address_from_word(word: &[u8]) -> Address {
    Address::from_slice(&word[12..32])
}

fn u256_to_u64(value: alloy::primitives::U256) -> u64 {
    value.to_string().parse::<u64>().unwrap_or(0)
}
