use std::{
    collections::{HashMap, HashSet, VecDeque},
    hash::{DefaultHasher, Hash, Hasher},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use alloy::{
    primitives::{keccak256, Address, B256, U256},
    providers::{Provider, ProviderBuilder, WsConnect},
    rpc::types::Filter,
};
use anyhow::Result;
use futures::StreamExt;
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::{
    config::Settings,
    graph::GraphSnapshot,
    monitoring::metrics as telemetry,
    rpc::{adapt_log_chunk_size, should_reduce_chunk_size, RpcClients, RpcLog},
    types::{AmmKind, PoolSpecificState, PoolStatePatch, RefreshBatch, RefreshTrigger},
};

pub struct EventStream {
    settings: Arc<Settings>,
    rpc: Arc<RpcClients>,
    ws_started: Arc<AtomicBool>,
    ws_rx: Arc<Mutex<Option<mpsc::Receiver<RpcLog>>>>,
    recent_wss_logs: Arc<Mutex<RecentLogKeys>>,
    last_wss_reconcile_at: Arc<Mutex<Option<Instant>>>,
}

impl EventStream {
    pub fn new(settings: Arc<Settings>, rpc: Arc<RpcClients>) -> Self {
        Self {
            settings,
            rpc,
            ws_started: Arc::new(AtomicBool::new(false)),
            ws_rx: Arc::new(Mutex::new(None)),
            recent_wss_logs: Arc::new(Mutex::new(RecentLogKeys::default())),
            last_wss_reconcile_at: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn poll_once(
        &self,
        snapshot: &GraphSnapshot,
        from_block: u64,
    ) -> Result<(u64, RefreshBatch)> {
        let addresses = watched_addresses(snapshot);
        if addresses.is_empty() {
            return Ok((from_block, RefreshBatch::default()));
        }

        if matches!(event_ingest_mode(), EventIngestMode::Wss) {
            return self
                .poll_wss_hybrid(snapshot, from_block, &addresses, &event_topics())
                .await;
        }

        let latest = self.rpc.best_read().block_number().await?;
        if latest <= from_block {
            return Ok((from_block, RefreshBatch::default()));
        }

        let logs = self
            .poll_logs(from_block + 1, latest, &addresses, &event_topics())
            .await?;
        let log_count = logs.len();
        let batch = self.build_refresh_batch(snapshot, logs);
        telemetry::record_event_ingestion(
            event_ingest_mode().as_str(),
            log_count,
            self.settings.chain.as_str(),
        );
        Ok((latest, batch))
    }

    async fn poll_wss_hybrid(
        &self,
        snapshot: &GraphSnapshot,
        from_block: u64,
        addresses: &[Address],
        topics: &[B256],
    ) -> Result<(u64, RefreshBatch)> {
        let latest = self.rpc.best_read().block_number().await?;
        if !self.start_wss(addresses, topics) {
            if latest <= from_block {
                return Ok((from_block, RefreshBatch::default()));
            }
            let logs = self
                .poll_wss_reconcile_logs(from_block + 1, latest, addresses, topics)
                .await?;
            let log_count = logs.len();
            let batch = self.build_refresh_batch(snapshot, logs);
            telemetry::record_event_ingestion(
                "wss-fallback",
                log_count,
                self.settings.chain.as_str(),
            );
            return Ok((latest, batch));
        }

        let mut wss_logs = filter_watched_logs(self.drain_wss_logs(), addresses, topics);
        wss_logs.retain(|log| {
            log.block_number
                .map(|block| block > from_block)
                .unwrap_or(true)
        });
        self.record_wss_log_keys(&wss_logs);

        let should_reconcile = latest > from_block && self.wss_reconcile_due(from_block, latest);
        let mut next_from_block = from_block;
        let mut logs = Vec::new();
        let wss_log_count = wss_logs.len();
        let mut backfill_log_count = 0usize;

        if should_reconcile {
            let backfill_logs = self
                .poll_wss_reconcile_logs(from_block + 1, latest, addresses, topics)
                .await?;
            backfill_log_count = backfill_logs.len();
            logs.extend(self.filter_seen_wss_logs(backfill_logs));
            next_from_block = latest;
            *self.last_wss_reconcile_at.lock() = Some(Instant::now());
        }

        logs.extend(wss_logs);
        let logs = dedup_logs(logs);
        let log_count = logs.len();
        let batch = self.build_refresh_batch(snapshot, logs);
        if should_reconcile {
            info!(
                from_block = from_block + 1,
                to_block = latest,
                wss_logs = wss_log_count,
                backfill_logs = backfill_log_count,
                accepted_logs = log_count,
                mode = wss_reconcile_mode().as_str(),
                "wss reconciliation complete"
            );
        }
        telemetry::record_event_ingestion("wss", log_count, self.settings.chain.as_str());
        Ok((next_from_block, batch))
    }

    async fn poll_logs(
        &self,
        from_block: u64,
        to_block: u64,
        addresses: &[Address],
        topics: &[B256],
    ) -> Result<Vec<RpcLog>> {
        match event_ingest_mode() {
            EventIngestMode::Wss => {
                self.start_wss(addresses, topics);
                Ok(filter_watched_logs(
                    self.drain_wss_logs(),
                    addresses,
                    topics,
                ))
            }
            EventIngestMode::BlockReceipts => {
                let max_receipt_blocks = std::env::var("EVENT_RECEIPT_MAX_BLOCKS")
                    .ok()
                    .and_then(|value| value.parse::<u64>().ok())
                    .filter(|value| *value > 0)
                    .unwrap_or(64);
                let span = to_block.saturating_sub(from_block).saturating_add(1);
                if span <= max_receipt_blocks {
                    match self
                        .get_block_receipts_range(from_block, to_block, addresses, topics)
                        .await
                    {
                        Ok(logs) => Ok(logs),
                        Err(err) => {
                            warn!(error = %err, "eth_getBlockReceipts failed; falling back to topic logs");
                            self.get_topic_logs_chunked(from_block, to_block, topics)
                                .await
                                .map(|logs| filter_watched_logs(logs, addresses, topics))
                        }
                    }
                } else {
                    self.get_topic_logs_chunked(from_block, to_block, topics)
                        .await
                        .map(|logs| filter_watched_logs(logs, addresses, topics))
                }
            }
            EventIngestMode::TopicLogs => self
                .get_topic_logs_chunked(from_block, to_block, topics)
                .await
                .map(|logs| filter_watched_logs(logs, addresses, topics)),
            EventIngestMode::AddressLogs => {
                self.get_address_logs_chunked(from_block, to_block, addresses, topics)
                    .await
            }
        }
    }

    async fn poll_wss_reconcile_logs(
        &self,
        from_block: u64,
        to_block: u64,
        addresses: &[Address],
        topics: &[B256],
    ) -> Result<Vec<RpcLog>> {
        match wss_reconcile_mode() {
            EventIngestMode::BlockReceipts => {
                let max_receipt_blocks = std::env::var("EVENT_RECEIPT_MAX_BLOCKS")
                    .ok()
                    .and_then(|value| value.parse::<u64>().ok())
                    .filter(|value| *value > 0)
                    .unwrap_or(64);
                let span = to_block.saturating_sub(from_block).saturating_add(1);
                if span <= max_receipt_blocks {
                    match self
                        .get_block_receipts_range(from_block, to_block, addresses, topics)
                        .await
                    {
                        Ok(logs) => Ok(logs),
                        Err(err) => {
                            warn!(error = %err, "wss reconciliation via block receipts failed; falling back to topic logs");
                            self.get_topic_logs_chunked(from_block, to_block, topics)
                                .await
                                .map(|logs| filter_watched_logs(logs, addresses, topics))
                        }
                    }
                } else {
                    self.get_topic_logs_chunked(from_block, to_block, topics)
                        .await
                        .map(|logs| filter_watched_logs(logs, addresses, topics))
                }
            }
            EventIngestMode::AddressLogs => {
                self.get_address_logs_chunked(from_block, to_block, addresses, topics)
                    .await
            }
            EventIngestMode::Wss | EventIngestMode::TopicLogs => self
                .get_topic_logs_chunked(from_block, to_block, topics)
                .await
                .map(|logs| filter_watched_logs(logs, addresses, topics)),
        }
    }

    fn start_wss(&self, addresses: &[Address], topics: &[B256]) -> bool {
        if self.ws_started.swap(true, Ordering::SeqCst) {
            return true;
        }
        let Some(ws_url) = self.settings.rpc.ws_url.clone() else {
            warn!("EVENT_INGEST_MODE=wss requested but chain WSS URL is not configured; no WSS logs will be received");
            self.ws_started.store(false, Ordering::SeqCst);
            return false;
        };
        let topics = topics.to_vec();
        let addresses = addresses.to_vec();
        let filter_mode = wss_filter_mode();
        let capacity = std::env::var("EVENT_WSS_CHANNEL_CAPACITY")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(16_384);
        let (tx, rx) = mpsc::channel(capacity);
        *self.ws_rx.lock() = Some(rx);
        let ws_started = self.ws_started.clone();
        tokio::spawn(async move {
            if let Err(err) =
                run_wss_log_subscription(ws_url, addresses, topics, filter_mode, tx).await
            {
                warn!(error = %err, "WSS log subscription stopped");
            }
            ws_started.store(false, Ordering::SeqCst);
        });
        true
    }

    fn drain_wss_logs(&self) -> Vec<RpcLog> {
        let mut out = Vec::new();
        let mut guard = self.ws_rx.lock();
        let Some(rx) = guard.as_mut() else {
            return out;
        };
        while let Ok(log) = rx.try_recv() {
            out.push(log);
        }
        out
    }

    fn record_wss_log_keys(&self, logs: &[RpcLog]) {
        let capacity = recent_wss_log_capacity();
        let mut recent = self.recent_wss_logs.lock();
        for log in logs {
            recent.insert(log_key(log), capacity);
        }
    }

    fn filter_seen_wss_logs(&self, logs: Vec<RpcLog>) -> Vec<RpcLog> {
        let recent = self.recent_wss_logs.lock();
        logs.into_iter()
            .filter(|log| !recent.contains(&log_key(log)))
            .collect()
    }

    fn wss_reconcile_due(&self, from_block: u64, latest: u64) -> bool {
        let block_gap = latest.saturating_sub(from_block);
        if block_gap >= wss_reconcile_interval_blocks() {
            return true;
        }
        let Some(last) = *self.last_wss_reconcile_at.lock() else {
            return block_gap > 0;
        };
        last.elapsed() >= wss_reconcile_interval()
    }

    async fn get_block_receipts_range(
        &self,
        from_block: u64,
        to_block: u64,
        addresses: &[Address],
        topics: &[B256],
    ) -> Result<Vec<RpcLog>> {
        let mut logs = Vec::new();
        for block in from_block..=to_block {
            let mut block_logs = self.rpc.best_read().get_block_receipts_logs(block).await?;
            logs.append(&mut block_logs);
        }
        Ok(filter_watched_logs(logs, addresses, topics))
    }

    async fn get_topic_logs_chunked(
        &self,
        from_block: u64,
        to_block: u64,
        topics: &[B256],
    ) -> Result<Vec<RpcLog>> {
        if from_block > to_block {
            return Ok(Vec::new());
        }
        let chunk_blocks = std::env::var("EVENT_LOG_CHUNK_BLOCKS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(5_000);
        let mut logs = Vec::new();
        let mut start = from_block;
        while start <= to_block {
            let end = start.saturating_add(chunk_blocks - 1).min(to_block);
            let mut chunk = self
                .rpc
                .best_read()
                .get_logs(start, end, &[], topics)
                .await?;
            logs.append(&mut chunk);
            if end == u64::MAX {
                break;
            }
            start = end.saturating_add(1);
        }
        Ok(logs)
    }

    async fn get_address_logs_chunked(
        &self,
        from_block: u64,
        to_block: u64,
        addresses: &[Address],
        topics: &[B256],
    ) -> Result<Vec<RpcLog>> {
        if from_block > to_block {
            return Ok(Vec::new());
        }

        // Use chain-aware chunk size with adaptive splitting
        let provider_mode = std::env::var("RPC_PROVIDER_MODE")
            .ok()
            .unwrap_or_else(|| "payg".to_string());
        let mut chunk_blocks = std::env::var("EVENT_LOG_CHUNK_BLOCKS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(5_000);

        // Apply chain-aware limits
        chunk_blocks = adapt_log_chunk_size(self.settings.chain, &provider_mode, chunk_blocks);

        let address_chunk_size = std::env::var("EVENT_LOG_ADDRESS_CHUNK_SIZE")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(200);

        let mut logs = Vec::new();
        let mut start = from_block;
        const MAX_RETRIES: usize = 3;

        while start <= to_block {
            let mut retry_count = 0;

            loop {
                let end = start.saturating_add(chunk_blocks - 1).min(to_block);
                let mut range_logs = Vec::new();
                let mut retry_range = false;

                for address_chunk in addresses.chunks(address_chunk_size) {
                    match self
                        .rpc
                        .best_read()
                        .get_logs(start, end, address_chunk, topics)
                        .await
                    {
                        Ok(mut chunk) => {
                            range_logs.append(&mut chunk);
                        }
                        Err(e) => {
                            let error_msg = e.to_string();
                            if should_reduce_chunk_size(&error_msg) && retry_count < MAX_RETRIES {
                                tracing::warn!(
                                    %error_msg,
                                    chunk_size = chunk_blocks,
                                    retry = retry_count,
                                    block_range = %format!("{}..{}", start, end),
                                    "address log query failed, reducing chunk size and retrying block range"
                                );
                                chunk_blocks = (chunk_blocks / 2).max(1);
                                retry_count += 1;
                                retry_range = true;
                                break;
                            } else {
                                return Err(e.context("failed to get address logs after retries"));
                            }
                        }
                    }
                }

                if retry_range {
                    continue;
                }

                logs.append(&mut range_logs);
                if end == u64::MAX {
                    return Ok(logs);
                }
                start = end.saturating_add(1);
                break;
            }
        }
        Ok(logs)
    }

    fn build_refresh_batch(&self, snapshot: &GraphSnapshot, logs: Vec<RpcLog>) -> RefreshBatch {
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

        let mut triggers = HashMap::<Address, RefreshTrigger>::new();
        for log in logs {
            if snapshot.pools.contains_key(&log.address) {
                let patch = decode_pool_patch(snapshot, &log);
                upsert_trigger(
                    &mut triggers,
                    log.address,
                    RefreshTrigger {
                        pool_id: Some(log.address),
                        full_refresh: patch.is_none(),
                        source: if patch.is_some() {
                            "pool-log-patch".to_string()
                        } else {
                            "pool-log-refresh".to_string()
                        },
                        patch,
                    },
                );
                continue;
            }

            if let Some(topic1) = log.topics.get(1) {
                if let Some(pool_id) = balancer_map.get(topic1).copied() {
                    let patch = decode_balancer_vault_patch(pool_id, &log);
                    upsert_trigger(
                        &mut triggers,
                        pool_id,
                        RefreshTrigger {
                            pool_id: Some(pool_id),
                            full_refresh: patch.is_none(),
                            source: if patch.is_some() {
                                "balancer-vault-log-patch".to_string()
                            } else {
                                "balancer-vault-log-refresh".to_string()
                            },
                            patch,
                        },
                    );
                }
            }
        }

        RefreshBatch {
            triggers: triggers.into_values().collect(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum EventIngestMode {
    Wss,
    BlockReceipts,
    TopicLogs,
    AddressLogs,
}

#[derive(Debug, Clone, Copy)]
enum WssFilterMode {
    TopicLogs,
    AddressLogs,
}

impl EventIngestMode {
    fn as_str(self) -> &'static str {
        match self {
            EventIngestMode::Wss => "wss",
            EventIngestMode::BlockReceipts => "block_receipts",
            EventIngestMode::TopicLogs => "topic_logs",
            EventIngestMode::AddressLogs => "address_logs",
        }
    }
}

impl WssFilterMode {
    fn as_str(self) -> &'static str {
        match self {
            WssFilterMode::TopicLogs => "topic_logs",
            WssFilterMode::AddressLogs => "address_logs",
        }
    }
}

fn event_ingest_mode() -> EventIngestMode {
    match std::env::var("EVENT_INGEST_MODE")
        .unwrap_or_else(|_| "address_logs".to_string())
        .to_ascii_lowercase()
        .as_str()
    {
        "wss" | "ws" | "websocket" => EventIngestMode::Wss,
        "address_logs" | "address" | "legacy" => EventIngestMode::AddressLogs,
        "topic_logs" | "topic" | "logs" => EventIngestMode::TopicLogs,
        _ => EventIngestMode::BlockReceipts,
    }
}

fn wss_reconcile_mode() -> EventIngestMode {
    match std::env::var("EVENT_WSS_RECONCILE_MODE")
        .unwrap_or_else(|_| "topic_logs".to_string())
        .to_ascii_lowercase()
        .as_str()
    {
        "block_receipts" | "receipts" => EventIngestMode::BlockReceipts,
        "address_logs" | "address" | "legacy" => EventIngestMode::AddressLogs,
        _ => EventIngestMode::TopicLogs,
    }
}

fn wss_filter_mode() -> WssFilterMode {
    match std::env::var("EVENT_WSS_FILTER_MODE")
        .unwrap_or_else(|_| "topic_logs".to_string())
        .to_ascii_lowercase()
        .as_str()
    {
        "address_logs" | "address" | "legacy" => WssFilterMode::AddressLogs,
        _ => WssFilterMode::TopicLogs,
    }
}

fn wss_reconcile_interval_blocks() -> u64 {
    std::env::var("EVENT_WSS_RECONCILE_INTERVAL_BLOCKS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(2)
}

fn wss_reconcile_interval() -> Duration {
    Duration::from_millis(
        std::env::var("EVENT_WSS_RECONCILE_INTERVAL_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(10_000),
    )
}

fn recent_wss_log_capacity() -> usize {
    std::env::var("EVENT_WSS_RECENT_LOG_CACHE")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(100_000)
}

async fn run_wss_log_subscription(
    ws_url: String,
    addresses: Vec<Address>,
    topics: Vec<B256>,
    filter_mode: WssFilterMode,
    tx: mpsc::Sender<RpcLog>,
) -> Result<()> {
    let provider = ProviderBuilder::new()
        .connect_ws(WsConnect::new(ws_url))
        .await?;
    info!(
        mode = filter_mode.as_str(),
        topic_count = topics.len(),
        address_count = addresses.len(),
        "starting WSS log subscription"
    );
    let filter = match filter_mode {
        WssFilterMode::AddressLogs => Filter::new().address(addresses).event_signature(topics),
        WssFilterMode::TopicLogs => Filter::new().event_signature(topics),
    };
    let sub = provider.subscribe_logs(&filter).await?;
    let mut stream = sub.into_stream();
    while let Some(log) = stream.next().await {
        let rpc_log = RpcLog {
            address: log.address(),
            topics: log.topics().to_vec(),
            data: log.data().data.clone(),
            block_number: log.block_number,
            block_hash: log.block_hash,
            tx_hash: log.transaction_hash,
        };
        if tx.send(rpc_log).await.is_err() {
            break;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct LogKey {
    block_number: Option<u64>,
    tx_hash: Option<B256>,
    address: Address,
    topic0: Option<B256>,
    data_hash: u64,
}

#[derive(Debug, Default)]
struct RecentLogKeys {
    order: VecDeque<LogKey>,
    set: HashSet<LogKey>,
}

impl RecentLogKeys {
    fn insert(&mut self, key: LogKey, capacity: usize) {
        if self.set.insert(key) {
            self.order.push_back(key);
        }
        while self.order.len() > capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.set.remove(&oldest);
            } else {
                break;
            }
        }
    }

    fn contains(&self, key: &LogKey) -> bool {
        self.set.contains(key)
    }
}

fn dedup_logs(logs: Vec<RpcLog>) -> Vec<RpcLog> {
    let mut seen = HashSet::new();
    logs.into_iter()
        .filter(|log| seen.insert(log_key(log)))
        .collect()
}

fn log_key(log: &RpcLog) -> LogKey {
    LogKey {
        block_number: log.block_number,
        tx_hash: log.tx_hash,
        address: log.address,
        topic0: log.topics.first().copied(),
        data_hash: hash_log_data(log.data.as_ref()),
    }
}

fn hash_log_data(data: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    data.hash(&mut hasher);
    hasher.finish()
}

fn upsert_trigger(
    triggers: &mut HashMap<Address, RefreshTrigger>,
    pool_id: Address,
    trigger: RefreshTrigger,
) {
    match triggers.get(&pool_id) {
        Some(existing) if existing.full_refresh => {}
        Some(_) if trigger.full_refresh => {
            triggers.insert(pool_id, trigger);
        }
        Some(_) => {}
        None => {
            triggers.insert(pool_id, trigger);
        }
    }
}

fn filter_watched_logs(logs: Vec<RpcLog>, addresses: &[Address], topics: &[B256]) -> Vec<RpcLog> {
    let watched = addresses.iter().copied().collect::<HashSet<_>>();
    let topics = topics.iter().copied().collect::<HashSet<_>>();
    logs.into_iter()
        .filter(|log| watched.contains(&log.address))
        .filter(|log| {
            log.topics
                .first()
                .map(|topic| topics.contains(topic))
                .unwrap_or(false)
        })
        .collect()
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

fn decode_pool_patch(snapshot: &GraphSnapshot, log: &RpcLog) -> Option<PoolStatePatch> {
    let pool = snapshot.pool(log.address)?;
    let topic0 = *log.topics.first()?;
    if pool.kind == AmmKind::UniswapV2Like && topic0 == signature_hash("Sync(uint112,uint112)") {
        let data = log.data.as_ref();
        return Some(PoolStatePatch::UniswapV2Sync {
            pool_id: log.address,
            reserve0: word_u128(data, 0)?,
            reserve1: word_u128(data, 1)?,
            block_number: log.block_number,
        });
    }

    if pool.kind == AmmKind::UniswapV3Like {
        if topic0 == signature_hash("Swap(address,address,int256,int256,uint160,uint128,int24)") {
            let data = log.data.as_ref();
            return Some(PoolStatePatch::UniswapV3Swap {
                pool_id: log.address,
                sqrt_price_x96: word_u256(data, 2)?,
                liquidity: word_u128(data, 3)?,
                tick: word_i24(data, 4)?,
                block_number: log.block_number,
            });
        }
        if topic0 == signature_hash("Initialize(uint160,int24)") {
            let data = log.data.as_ref();
            let liquidity = match &pool.state {
                PoolSpecificState::UniswapV3Like(state) => state.liquidity,
                _ => 0,
            };
            return Some(PoolStatePatch::UniswapV3Swap {
                pool_id: log.address,
                sqrt_price_x96: word_u256(data, 0)?,
                liquidity,
                tick: word_i24(data, 1)?,
                block_number: log.block_number,
            });
        }
    }

    None
}

fn decode_balancer_vault_patch(pool_id: Address, log: &RpcLog) -> Option<PoolStatePatch> {
    let topic0 = *log.topics.first()?;
    if topic0 == signature_hash("Swap(bytes32,address,address,uint256,uint256)")
        && log.topics.len() >= 4
    {
        let data = log.data.as_ref();
        return Some(PoolStatePatch::BalancerSwap {
            pool_id,
            token_in: topic_to_address(log.topics[2]),
            token_out: topic_to_address(log.topics[3]),
            amount_in: word_u128(data, 0)?,
            amount_out: word_u128(data, 1)?,
            block_number: log.block_number,
        });
    }

    if topic0 == signature_hash("PoolBalanceChanged(bytes32,address,address[],int256[],uint256[])")
    {
        let data = log.data.as_ref();
        let token_offset = word_usize(data, 0)?;
        let delta_offset = word_usize(data, 1)?;
        let tokens = address_array(data, token_offset)?;
        let deltas = i128_array(data, delta_offset)?;
        if tokens.len() == deltas.len() {
            return Some(PoolStatePatch::BalancerBalanceChanged {
                pool_id,
                tokens,
                deltas,
                block_number: log.block_number,
            });
        }
    }

    None
}

fn topic_to_address(topic: B256) -> Address {
    let bytes = topic.as_slice();
    Address::from_slice(&bytes[12..32])
}

fn signature_hash(signature: &str) -> B256 {
    keccak256(signature.as_bytes())
}

fn word(data: &[u8], index: usize) -> Option<&[u8]> {
    let start = index.checked_mul(32)?;
    let end = start.checked_add(32)?;
    data.get(start..end)
}

fn word_u128(data: &[u8], index: usize) -> Option<u128> {
    let bytes = word(data, index)?;
    let mut out = [0u8; 16];
    out.copy_from_slice(&bytes[16..32]);
    Some(u128::from_be_bytes(out))
}

fn word_usize(data: &[u8], index: usize) -> Option<usize> {
    usize::try_from(word_u128(data, index)?).ok()
}

fn word_u256(data: &[u8], index: usize) -> Option<U256> {
    Some(U256::from_be_slice(word(data, index)?))
}

fn word_i24(data: &[u8], index: usize) -> Option<i32> {
    let bytes = word(data, index)?;
    let raw = ((bytes[29] as i32) << 16) | ((bytes[30] as i32) << 8) | bytes[31] as i32;
    if raw & 0x80_0000 != 0 {
        Some(raw | !0xff_ffff)
    } else {
        Some(raw)
    }
}

fn word_i128_at(data: &[u8], byte_offset: usize) -> Option<i128> {
    let bytes = data.get(byte_offset..byte_offset.checked_add(32)?)?;
    let negative = bytes.first().map(|byte| byte & 0x80 != 0).unwrap_or(false);
    if negative && !bytes[..16].iter().all(|byte| *byte == 0xff) {
        return None;
    }
    if !negative && !bytes[..16].iter().all(|byte| *byte == 0x00) {
        return None;
    }
    let mut out = [0u8; 16];
    out.copy_from_slice(&bytes[16..32]);
    Some(i128::from_be_bytes(out))
}

fn address_array(data: &[u8], offset: usize) -> Option<Vec<Address>> {
    let len = word_u128_at_offset(data, offset)?;
    let len = usize::try_from(len).ok()?;
    let mut out = Vec::with_capacity(len);
    for idx in 0..len {
        let word_start = offset.checked_add(32)?.checked_add(idx.checked_mul(32)?)?;
        let bytes = data.get(word_start..word_start.checked_add(32)?)?;
        out.push(Address::from_slice(&bytes[12..32]));
    }
    Some(out)
}

fn i128_array(data: &[u8], offset: usize) -> Option<Vec<i128>> {
    let len = word_u128_at_offset(data, offset)?;
    let len = usize::try_from(len).ok()?;
    let mut out = Vec::with_capacity(len);
    for idx in 0..len {
        let word_start = offset.checked_add(32)?.checked_add(idx.checked_mul(32)?)?;
        out.push(word_i128_at(data, word_start)?);
    }
    Some(out)
}

fn word_u128_at_offset(data: &[u8], byte_offset: usize) -> Option<u128> {
    let bytes = data.get(byte_offset..byte_offset.checked_add(32)?)?;
    let mut out = [0u8; 16];
    out.copy_from_slice(&bytes[16..32]);
    Some(u128::from_be_bytes(out))
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::{Mutex, OnceLock},
        time::SystemTime,
    };

    use alloy::primitives::{Address, Bytes, B256};

    use crate::{
        graph::GraphSnapshot,
        types::{
            AmmKind, PoolAdmissionStatus, PoolHealth, PoolSpecificState, PoolState, PoolStatePatch,
            TokenBehavior, TokenInfo, V2PoolState,
        },
    };

    use super::*;

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    #[test]
    fn decodes_v2_sync_as_pool_state_patch() {
        let token0 = addr(1);
        let token1 = addr(2);
        let pool_id = addr(3);
        let snapshot = GraphSnapshot::build(
            1,
            None,
            vec![token("A", token0), token("B", token1)],
            HashMap::from([(
                pool_id,
                PoolState {
                    pool_id,
                    dex_name: "v2".to_string(),
                    kind: AmmKind::UniswapV2Like,
                    token_addresses: vec![token0, token1],
                    token_symbols: vec!["A".to_string(), "B".to_string()],
                    factory: None,
                    registry: None,
                    vault: None,
                    quoter: None,
                    admission_status: PoolAdmissionStatus::Allowed,
                    health: health(),
                    state: PoolSpecificState::UniswapV2Like(V2PoolState {
                        reserve0: 1,
                        reserve1: 1,
                        fee_ppm: 3_000,
                    }),
                    last_updated_block: 1,
                    extras: HashMap::new(),
                },
            )]),
        );
        let log = RpcLog {
            address: pool_id,
            topics: vec![signature_hash("Sync(uint112,uint112)")],
            data: encode_words(&[123, 456]),
            block_number: Some(10),
            block_hash: None,
            tx_hash: None,
        };

        let Some(PoolStatePatch::UniswapV2Sync {
            reserve0,
            reserve1,
            block_number,
            ..
        }) = decode_pool_patch(&snapshot, &log)
        else {
            panic!("expected v2 sync patch");
        };
        assert_eq!(reserve0, 123);
        assert_eq!(reserve1, 456);
        assert_eq!(block_number, Some(10));
    }

    #[test]
    fn filter_watched_logs_requires_matching_address_and_topic() {
        let watched_address = addr(7);
        let watched_topic =
            signature_hash("Swap(address,address,int256,int256,uint160,uint128,int24)");
        let ignored_topic = signature_hash("Sync(uint112,uint112)");
        let logs = vec![
            RpcLog {
                address: watched_address,
                topics: vec![watched_topic],
                data: Bytes::new(),
                block_number: Some(1),
                block_hash: None,
                tx_hash: None,
            },
            RpcLog {
                address: watched_address,
                topics: vec![ignored_topic],
                data: Bytes::new(),
                block_number: Some(2),
                block_hash: None,
                tx_hash: None,
            },
            RpcLog {
                address: addr(8),
                topics: vec![watched_topic],
                data: Bytes::new(),
                block_number: Some(3),
                block_hash: None,
                tx_hash: None,
            },
        ];

        let filtered = filter_watched_logs(logs, &[watched_address], &[watched_topic]);

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].block_number, Some(1));
        assert_eq!(filtered[0].address, watched_address);
    }

    #[test]
    fn event_ingest_mode_defaults_to_address_logs_when_unset() {
        let _guard = ENV_LOCK.get_or_init(Mutex::default).lock().unwrap();
        let key = "EVENT_INGEST_MODE";
        let previous = std::env::var_os(key);
        std::env::remove_var(key);

        let mode = event_ingest_mode();

        match previous {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }

        assert!(matches!(mode, EventIngestMode::AddressLogs));
    }

    #[test]
    fn wss_modes_default_to_topic_logs_when_unset() {
        let _guard = ENV_LOCK.get_or_init(Mutex::default).lock().unwrap();
        let filter_key = "EVENT_WSS_FILTER_MODE";
        let reconcile_key = "EVENT_WSS_RECONCILE_MODE";
        let previous_filter = std::env::var_os(filter_key);
        let previous_reconcile = std::env::var_os(reconcile_key);
        std::env::remove_var(filter_key);
        std::env::remove_var(reconcile_key);

        let filter_mode = wss_filter_mode();
        let reconcile_mode = wss_reconcile_mode();

        match previous_filter {
            Some(value) => std::env::set_var(filter_key, value),
            None => std::env::remove_var(filter_key),
        }
        match previous_reconcile {
            Some(value) => std::env::set_var(reconcile_key, value),
            None => std::env::remove_var(reconcile_key),
        }

        assert!(matches!(filter_mode, WssFilterMode::TopicLogs));
        assert!(matches!(reconcile_mode, EventIngestMode::TopicLogs));
    }

    #[test]
    fn recent_wss_log_keys_prevent_duplicate_backfill_logs() {
        let log = RpcLog {
            address: addr(7),
            topics: vec![signature_hash("Sync(uint112,uint112)")],
            data: encode_words(&[123, 456]),
            block_number: Some(42),
            block_hash: None,
            tx_hash: Some(B256::from([3u8; 32])),
        };
        let mut recent = RecentLogKeys::default();
        let key = log_key(&log);

        recent.insert(key, 16);

        assert!(recent.contains(&key));
        assert_eq!(dedup_logs(vec![log.clone(), log]).len(), 1);
    }

    #[test]
    fn decodes_balancer_swap_as_pool_state_patch() {
        let pool_id = addr(9);
        let token_in = addr(10);
        let token_out = addr(11);
        let log = RpcLog {
            address: addr(12),
            topics: vec![
                signature_hash("Swap(bytes32,address,address,uint256,uint256)"),
                B256::ZERO,
                address_topic(token_in),
                address_topic(token_out),
            ],
            data: encode_words(&[1_000, 990]),
            block_number: Some(20),
            block_hash: None,
            tx_hash: None,
        };

        let Some(PoolStatePatch::BalancerSwap {
            pool_id: decoded_pool,
            token_in: decoded_in,
            token_out: decoded_out,
            amount_in,
            amount_out,
            block_number,
        }) = decode_balancer_vault_patch(pool_id, &log)
        else {
            panic!("expected balancer swap patch");
        };
        assert_eq!(decoded_pool, pool_id);
        assert_eq!(decoded_in, token_in);
        assert_eq!(decoded_out, token_out);
        assert_eq!(amount_in, 1_000);
        assert_eq!(amount_out, 990);
        assert_eq!(block_number, Some(20));
    }

    fn token(symbol: &str, address: Address) -> TokenInfo {
        TokenInfo {
            address,
            symbol: symbol.to_string(),
            decimals: 18,
            is_stable: false,
            is_cycle_anchor: false,
            flash_loan_enabled: false,
            allow_self_funded: false,
            behavior: TokenBehavior::default(),
            manual_price_usd_e8: None,
            max_position_usd_e8: None,
            max_flash_loan_usd_e8: None,
        }
    }

    fn health() -> PoolHealth {
        PoolHealth {
            stale: false,
            paused: false,
            quarantined: false,
            confidence_bps: 10_000,
            last_successful_refresh_block: 1,
            last_refresh_at: SystemTime::now(),
            recent_revert_count: 0,
        }
    }

    fn addr(byte: u8) -> Address {
        Address::from([byte; 20])
    }

    fn address_topic(address: Address) -> B256 {
        let mut bytes = [0u8; 32];
        bytes[12..32].copy_from_slice(address.as_slice());
        B256::from(bytes)
    }

    fn encode_words(values: &[u128]) -> Bytes {
        let mut bytes = Vec::with_capacity(values.len() * 32);
        for value in values {
            bytes.extend_from_slice(&[0u8; 16]);
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        bytes.into()
    }
}
