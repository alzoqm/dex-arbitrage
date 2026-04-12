use std::{collections::HashMap, fs, sync::Arc};

use alloy::{
    primitives::{keccak256, Address, B256, U256},
    sol_types::SolCall,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{
    abi::{ICurveRegistry, IUniswapV2Factory},
    config::{DexConfig, Settings},
    rpc::RpcClients,
    types::{AmmKind, BlockRef, DiscoveryKind, FinalityLevel},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
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
        let latest = self.latest_block().await?;
        let mut cache = DiscoveryCache::load(&self.settings)
            .unwrap_or_else(|_| DiscoveryCache::new(self.settings.chain_id));
        let mut pools = Vec::new();
        for dex in self.settings.dexes.iter().filter(|dex| dex.enabled) {
            let cached = cache.dexes.remove(&dex.name);
            let (mut part, next_cursor) = match dex.discovery_kind {
                DiscoveryKind::FactoryAllPairs => {
                    self.scan_v2_factory(dex, cached.as_ref(), latest).await?
                }
                DiscoveryKind::PoolCreatedLogs => {
                    self.scan_v3_factory_logs(dex, cached.as_ref(), latest)
                        .await?
                }
                DiscoveryKind::CurveRegistry => {
                    self.scan_curve_registry(dex, cached.as_ref(), latest)
                        .await?
                }
                DiscoveryKind::BalancerVaultLogs => {
                    self.scan_balancer_vault_logs(dex, cached.as_ref(), latest)
                        .await?
                }
                DiscoveryKind::StaticList => (Vec::new(), ScanCursor::default()),
            };
            cache.dexes.insert(
                dex.name.clone(),
                CachedDexScan::from_scan(dex, latest, next_cursor, &part),
            );
            pools.append(&mut part);
        }
        cache.save(&self.settings).ok();
        Ok(pools)
    }

    pub async fn latest_block(&self) -> Result<u64> {
        self.rpc.best_read().block_number().await
    }

    pub async fn current_block_ref(&self) -> Result<BlockRef> {
        let (number, hash, parent_hash) =
            self.rpc.best_read().get_block_by_number("latest").await?;
        Ok(BlockRef {
            number,
            hash,
            parent_hash,
            finality: FinalityLevel::Sealed,
        })
    }

    pub fn spec_for_pool(
        &self,
        snapshot: &crate::graph::GraphSnapshot,
        pool_id: Address,
    ) -> Option<DiscoveredPool> {
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

    async fn scan_v2_factory(
        &self,
        dex: &DexConfig,
        cached: Option<&CachedDexScan>,
        latest: u64,
    ) -> Result<(Vec<DiscoveredPool>, ScanCursor)> {
        let Some(factory) = dex.factory else {
            return Ok((Vec::new(), ScanCursor::default()));
        };
        let data = IUniswapV2Factory::allPairsLengthCall {}.abi_encode();
        let raw = self
            .rpc
            .best_read()
            .eth_call(factory, None, data.into(), "latest")
            .await?;
        let ret = IUniswapV2Factory::allPairsLengthCall::abi_decode_returns(&raw)
            .context("decode allPairsLength failed")?;
        let len = u256_to_u64(ret);

        let mut pools = cached_pools(cached, dex);
        let start = cached
            .filter(|cached| cached.matches(dex, latest))
            .map(|cached| cached.cursor.count.min(len))
            .unwrap_or(0);
        for idx in start..len {
            let data = IUniswapV2Factory::allPairsCall {
                index: U256::saturating_from(idx),
            }
            .abi_encode();
            let raw = self
                .rpc
                .best_read()
                .eth_call(factory, None, data.into(), "latest")
                .await?;
            let ret = IUniswapV2Factory::allPairsCall::abi_decode_returns(&raw)
                .context("decode allPairs failed")?;
            pools.push(DiscoveredPool {
                address: ret,
                dex_name: dex.name.clone(),
                amm_kind: dex.amm_kind,
                factory: dex.factory,
                registry: dex.registry,
                vault: dex.vault,
                quoter: dex.quoter,
                balancer_pool_id: None,
            });
        }
        dedup_pools(&mut pools);
        Ok((
            pools,
            ScanCursor {
                block: latest,
                count: len,
            },
        ))
    }

    async fn scan_v3_factory_logs(
        &self,
        dex: &DexConfig,
        cached: Option<&CachedDexScan>,
        latest: u64,
    ) -> Result<(Vec<DiscoveredPool>, ScanCursor)> {
        let Some(factory) = dex.factory else {
            return Ok((Vec::new(), ScanCursor::default()));
        };
        let topic = signature_hash("PoolCreated(address,address,uint24,int24,address)");
        let mut pools = cached_pools(cached, dex);
        let from_block = cached
            .filter(|cached| cached.matches(dex, latest))
            .map(|cached| cached.cursor.block.saturating_add(1))
            .unwrap_or(dex.start_block);
        let logs = self
            .get_logs_chunked(from_block, latest, &[factory], &[topic])
            .await?;

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
        dedup_pools(&mut pools);
        Ok((
            pools,
            ScanCursor {
                block: latest,
                count: 0,
            },
        ))
    }

    async fn scan_curve_registry(
        &self,
        dex: &DexConfig,
        cached: Option<&CachedDexScan>,
        latest: u64,
    ) -> Result<(Vec<DiscoveredPool>, ScanCursor)> {
        let Some(registry) = dex.registry else {
            return Ok((Vec::new(), ScanCursor::default()));
        };
        let data = ICurveRegistry::pool_countCall {}.abi_encode();
        let raw = self
            .rpc
            .best_read()
            .eth_call(registry, None, data.into(), "latest")
            .await?;
        let ret = ICurveRegistry::pool_countCall::abi_decode_returns(&raw)
            .context("decode curve pool_count failed")?;
        let len = u256_to_u64(ret);
        let mut pools = cached_pools(cached, dex);
        let start = cached
            .filter(|cached| cached.matches(dex, latest))
            .map(|cached| cached.cursor.count.min(len))
            .unwrap_or(0);
        for idx in start..len {
            let data = ICurveRegistry::pool_listCall {
                index: U256::saturating_from(idx),
            }
            .abi_encode();
            let raw = self
                .rpc
                .best_read()
                .eth_call(registry, None, data.into(), "latest")
                .await?;
            let ret = ICurveRegistry::pool_listCall::abi_decode_returns(&raw)
                .context("decode curve pool_list failed")?;
            pools.push(DiscoveredPool {
                address: ret,
                dex_name: dex.name.clone(),
                amm_kind: dex.amm_kind,
                factory: dex.factory,
                registry: dex.registry,
                vault: dex.vault,
                quoter: dex.quoter,
                balancer_pool_id: None,
            });
        }
        dedup_pools(&mut pools);
        Ok((
            pools,
            ScanCursor {
                block: latest,
                count: len,
            },
        ))
    }

    async fn scan_balancer_vault_logs(
        &self,
        dex: &DexConfig,
        cached: Option<&CachedDexScan>,
        latest: u64,
    ) -> Result<(Vec<DiscoveredPool>, ScanCursor)> {
        let Some(vault) = dex.vault else {
            return Ok((Vec::new(), ScanCursor::default()));
        };
        let topic = signature_hash("PoolRegistered(bytes32,address,uint8)");
        let from_block = cached
            .filter(|cached| cached.matches(dex, latest))
            .map(|cached| cached.cursor.block.saturating_add(1))
            .unwrap_or(dex.start_block);
        let logs = self
            .get_logs_chunked(from_block, latest, &[vault], &[topic])
            .await?;
        let mut pools = cached_pools(cached, dex);
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
        dedup_pools(&mut pools);
        Ok((
            pools,
            ScanCursor {
                block: latest,
                count: 0,
            },
        ))
    }

    async fn get_logs_chunked(
        &self,
        from_block: u64,
        to_block: u64,
        addresses: &[Address],
        topics: &[B256],
    ) -> Result<Vec<crate::rpc::RpcLog>> {
        if from_block > to_block {
            return Ok(Vec::new());
        }
        let chunk_blocks = std::env::var("DISCOVERY_LOG_CHUNK_BLOCKS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(50_000);
        let mut all = Vec::new();
        let mut start = from_block;
        while start <= to_block {
            let end = start.saturating_add(chunk_blocks - 1).min(to_block);
            let mut logs = self
                .rpc
                .best_read()
                .get_logs(start, end, addresses, topics)
                .await?;
            all.append(&mut logs);
            if end == u64::MAX {
                break;
            }
            start = end.saturating_add(1);
        }
        Ok(all)
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
struct ScanCursor {
    block: u64,
    count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedDexScan {
    amm: AmmKind,
    discovery: DiscoveryKind,
    factory: Option<Address>,
    registry: Option<Address>,
    vault: Option<Address>,
    quoter: Option<Address>,
    latest_block: u64,
    cursor: ScanCursor,
    pools: Vec<DiscoveredPool>,
}

impl CachedDexScan {
    fn from_scan(
        dex: &DexConfig,
        latest_block: u64,
        cursor: ScanCursor,
        pools: &[DiscoveredPool],
    ) -> Self {
        Self {
            amm: dex.amm_kind,
            discovery: dex.discovery_kind,
            factory: dex.factory,
            registry: dex.registry,
            vault: dex.vault,
            quoter: dex.quoter,
            latest_block,
            cursor,
            pools: pools.to_vec(),
        }
    }

    fn matches(&self, dex: &DexConfig, latest: u64) -> bool {
        self.amm == dex.amm_kind
            && self.discovery == dex.discovery_kind
            && self.factory == dex.factory
            && self.registry == dex.registry
            && self.vault == dex.vault
            && self.quoter == dex.quoter
            && self.latest_block <= latest
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DiscoveryCache {
    version: u32,
    chain_id: u64,
    dexes: HashMap<String, CachedDexScan>,
}

impl DiscoveryCache {
    fn new(chain_id: u64) -> Self {
        Self {
            version: 1,
            chain_id,
            dexes: HashMap::new(),
        }
    }

    fn load(settings: &Settings) -> Result<Self> {
        let raw = fs::read_to_string(settings.data_path("discovery_cache"))?;
        let cache: Self = serde_json::from_str(&raw)?;
        if cache.version != 1 || cache.chain_id != settings.chain_id {
            anyhow::bail!("discovery cache version or chain id mismatch");
        }
        Ok(cache)
    }

    fn save(&self, settings: &Settings) -> Result<()> {
        fs::create_dir_all("state").ok();
        fs::write(
            settings.data_path("discovery_cache"),
            serde_json::to_string_pretty(self)?,
        )?;
        Ok(())
    }
}

fn cached_pools(cached: Option<&CachedDexScan>, dex: &DexConfig) -> Vec<DiscoveredPool> {
    cached
        .filter(|cached| {
            cached.amm == dex.amm_kind
                && cached.discovery == dex.discovery_kind
                && cached.factory == dex.factory
                && cached.registry == dex.registry
                && cached.vault == dex.vault
                && cached.quoter == dex.quoter
        })
        .map(|cached| cached.pools.clone())
        .unwrap_or_default()
}

fn dedup_pools(pools: &mut Vec<DiscoveredPool>) {
    let mut seen = std::collections::HashSet::<(String, Address)>::new();
    pools.retain(|pool| seen.insert((pool.dex_name.clone(), pool.address)));
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
