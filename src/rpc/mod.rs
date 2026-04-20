pub mod scheduler;

use alloy::primitives::{Address, Bytes, B256, U256};
use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::atomic::{AtomicU64, Ordering},
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
use tokio::time::sleep;
use tracing::info;

use crate::config::RpcSettings;
use crate::types::Chain;

#[derive(Debug)]
pub struct RpcClient {
    url: String,
    fallback_urls: Vec<String>,
    http: reqwest::Client,
    request_id: AtomicU64,
    endpoint_status: parking_lot::Mutex<HashMap<String, EndpointStatus>>,
}

#[derive(Debug, Clone, Copy, Default)]
struct EndpointStatus {
    consecutive_failures: u64,
    open_until: Option<Instant>,
}

#[derive(Debug)]
struct RpcCuLimiter {
    capacity: u64,
    refill_rate: u64,
    tokens: AtomicU64,
    last_refill: parking_lot::Mutex<Instant>,
}

#[derive(Debug)]
struct RpcUsageAccumulator {
    since: Instant,
    by_method_provider: HashMap<String, RpcMethodUsage>,
}

#[derive(Debug, Clone, Copy, Default)]
struct RpcMethodUsage {
    requests: u64,
    cu: u64,
}

impl RpcUsageAccumulator {
    fn new() -> Self {
        Self {
            since: Instant::now(),
            by_method_provider: HashMap::new(),
        }
    }
}

impl RpcCuLimiter {
    fn new(capacity: u64, refill_rate: u64) -> Self {
        Self {
            capacity,
            refill_rate,
            tokens: AtomicU64::new(capacity),
            last_refill: parking_lot::Mutex::new(Instant::now()),
        }
    }

    fn try_consume(&self, amount: u64) -> bool {
        if amount == 0 {
            return true;
        }

        self.refill();
        let mut current = self.tokens.load(Ordering::SeqCst);
        loop {
            if current < amount {
                return false;
            }
            match self.tokens.compare_exchange_weak(
                current,
                current - amount,
                Ordering::SeqCst,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(actual) => current = actual,
            }
        }
    }

    fn wait_duration(&self, amount: u64) -> Duration {
        if amount == 0 {
            return Duration::ZERO;
        }
        let current = self.tokens.load(Ordering::SeqCst);
        if current >= amount {
            return Duration::ZERO;
        }
        let missing = amount - current;
        Duration::from_secs_f64((missing as f64 / self.refill_rate.max(1) as f64).max(0.01))
    }

    fn refill(&self) {
        let mut last = self.last_refill.lock();
        let elapsed = last.elapsed().as_secs_f64();
        let refill = (elapsed * self.refill_rate as f64) as u64;
        if refill == 0 {
            return;
        }
        let current = self.tokens.load(Ordering::SeqCst);
        self.tokens.store(
            current.saturating_add(refill).min(self.capacity),
            Ordering::SeqCst,
        );
        *last = Instant::now();
    }
}

static RPC_CU_LIMITER: OnceLock<RpcCuLimiter> = OnceLock::new();
static RPC_USAGE_ACCUMULATOR: OnceLock<parking_lot::Mutex<RpcUsageAccumulator>> = OnceLock::new();

#[derive(Debug, Clone)]
pub struct RpcClients {
    pub public: Arc<RpcClient>,
    pub fallback: Option<Arc<RpcClient>>,
    pub preconf: Option<Arc<RpcClient>>,
    pub protected: Option<Arc<RpcClient>>,
}

#[derive(Debug, Clone)]
pub struct RpcLog {
    pub address: Address,
    pub topics: Vec<B256>,
    pub data: Bytes,
    pub block_number: Option<u64>,
    pub block_hash: Option<B256>,
    pub tx_hash: Option<B256>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CallRequest {
    pub to: Address,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<Address>,
    pub data: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RpcResponse {
    result: Option<Value>,
    error: Option<RpcError>,
}

#[derive(Debug, Deserialize)]
struct RpcError {
    code: i64,
    message: String,
}

impl RpcClients {
    pub fn from_settings(settings: &RpcSettings) -> Result<Self> {
        let public_read_fallbacks = compact_unique_urls([
            settings.preconf_rpc_url.clone(),
            settings.fallback_rpc_url.clone(),
        ]);
        let fallback_read_fallbacks = compact_unique_urls([
            Some(settings.public_rpc_url.clone()),
            settings.preconf_rpc_url.clone(),
        ]);
        let preconf_read_fallbacks = compact_unique_urls([
            Some(settings.public_rpc_url.clone()),
            settings.fallback_rpc_url.clone(),
        ]);

        Ok(Self {
            public: Arc::new(RpcClient::with_fallbacks(
                settings.public_rpc_url.clone(),
                public_read_fallbacks,
            )?),
            fallback: settings
                .fallback_rpc_url
                .clone()
                .map(|url| RpcClient::with_fallbacks(url, fallback_read_fallbacks))
                .transpose()?
                .map(Arc::new),
            preconf: settings
                .preconf_rpc_url
                .clone()
                .map(|url| RpcClient::with_fallbacks(url, preconf_read_fallbacks))
                .transpose()?
                .map(Arc::new),
            protected: settings
                .protected_rpc_url
                .clone()
                .map(RpcClient::new)
                .transpose()?
                .map(Arc::new),
        })
    }

    pub fn best_read(&self) -> Arc<RpcClient> {
        self.preconf.clone().unwrap_or_else(|| self.public.clone())
    }

    pub fn best_write(&self) -> Arc<RpcClient> {
        self.protected
            .clone()
            .unwrap_or_else(|| self.public.clone())
    }
}

impl RpcClient {
    pub fn new(url: String) -> Result<Self> {
        Self::with_fallbacks(url, Vec::new())
    }

    pub fn with_fallbacks(url: String, fallback_urls: Vec<String>) -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        Ok(Self {
            url,
            fallback_urls,
            http: reqwest::Client::builder()
                .default_headers(headers)
                .build()
                .context("failed to build reqwest client")?,
            request_id: AtomicU64::new(1),
            endpoint_status: parking_lot::Mutex::new(HashMap::new()),
        })
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let max_retries = rpc_max_retries(method);
        let mut last_error = None;

        for attempt in 0..=max_retries {
            for endpoint in self.endpoints_for_method(method) {
                if self.endpoint_is_open(&endpoint, method) {
                    metrics::counter!(
                        "rpc_provider_skipped_total",
                        "method" => method.to_string(),
                        "provider" => provider_label(&endpoint)
                    )
                    .increment(1);
                    continue;
                }

                acquire_rpc_budget(method).await;
                let started = Instant::now();
                let provider = provider_label(&endpoint);
                let cu = rpc_compute_units(method);
                metrics::counter!(
                    "rpc_requests_total",
                    "method" => method.to_string(),
                    "provider" => provider.clone()
                )
                .increment(1);
                metrics::counter!(
                    "rpc_compute_units_total",
                    "method" => method.to_string(),
                    "provider" => provider.clone()
                )
                .increment(cu);
                record_rpc_usage_attempt(method, &provider, cu);

                match tokio::time::timeout(
                    rpc_timeout(method),
                    self.request_once(&endpoint, method, params.clone()),
                )
                .await
                {
                    Ok(Ok(value)) => {
                        self.record_endpoint_success(&endpoint, method);
                        metrics::histogram!(
                            "rpc_request_duration_seconds",
                            "method" => method.to_string(),
                            "provider" => provider.clone()
                        )
                        .record(started.elapsed().as_secs_f64());
                        return Ok(value);
                    }
                    Ok(Err(err)) => {
                        let endpoint_failure = should_record_endpoint_failure(method, &err);
                        if endpoint_failure {
                            self.record_endpoint_failure(&endpoint, method);
                        }
                        metrics::counter!(
                            "rpc_errors_total",
                            "method" => method.to_string(),
                            "provider" => provider.clone()
                        )
                        .increment(1);
                        if !endpoint_failure {
                            return Err(err);
                        }
                        last_error = Some(err);
                    }
                    Err(_) => {
                        self.record_endpoint_failure(&endpoint, method);
                        metrics::counter!(
                            "rpc_errors_total",
                            "method" => method.to_string(),
                            "provider" => provider.clone()
                        )
                        .increment(1);
                        last_error = Some(anyhow::anyhow!(
                            "rpc timeout after {:?} for method {}",
                            rpc_timeout(method),
                            method
                        ));
                    }
                }
            }

            if attempt < max_retries {
                sleep(rpc_backoff(method, attempt)).await;
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("rpc request failed for {method}")))
    }

    fn endpoints_for_method(&self, method: &str) -> Vec<String> {
        let mut endpoints = vec![self.url.clone()];
        if rpc_is_idempotent(method) {
            for url in &self.fallback_urls {
                if !endpoints.iter().any(|existing| existing == url) {
                    endpoints.push(url.clone());
                }
            }
        }
        endpoints
    }

    fn endpoint_is_open(&self, endpoint: &str, method: &str) -> bool {
        let key = endpoint_status_key(endpoint, method);
        let mut status = self.endpoint_status.lock();
        let Some(endpoint_status) = status.get_mut(&key) else {
            return false;
        };
        match endpoint_status.open_until {
            Some(until) if Instant::now() < until => true,
            Some(_) => {
                endpoint_status.open_until = None;
                false
            }
            None => false,
        }
    }

    fn record_endpoint_success(&self, endpoint: &str, method: &str) {
        self.endpoint_status.lock().insert(
            endpoint_status_key(endpoint, method),
            EndpointStatus::default(),
        );
    }

    fn record_endpoint_failure(&self, endpoint: &str, method: &str) {
        let threshold = env_u64("RPC_PROVIDER_FAILURE_THRESHOLD", 3);
        let open_ms = env_u64("RPC_PROVIDER_CIRCUIT_OPEN_MS", 30_000);
        let mut status = self.endpoint_status.lock();
        let entry = status
            .entry(endpoint_status_key(endpoint, method))
            .or_default();
        entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
        if entry.consecutive_failures >= threshold {
            entry.open_until = Some(Instant::now() + Duration::from_millis(open_ms));
        }
    }

    async fn request_once(&self, endpoint: &str, method: &str, params: Value) -> Result<Value> {
        let id = self.request_id.fetch_add(1, Ordering::Relaxed);
        let payload = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let response = self
            .http
            .post(endpoint)
            .json(&payload)
            .send()
            .await
            .with_context(|| format!("rpc request failed for method {method}"))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .context("failed to read rpc response body")?;

        if !status.is_success() {
            anyhow::bail!("rpc http error {} for method {}: {}", status, method, body);
        }

        let parsed: RpcResponse = serde_json::from_str(&body)
            .with_context(|| format!("invalid rpc response body: {body}"))?;
        if let Some(err) = parsed.error {
            anyhow::bail!(
                "rpc error {} {} for method {}",
                err.code,
                err.message,
                method
            );
        }

        parsed.result.context("missing rpc result")
    }

    pub async fn block_number(&self) -> Result<u64> {
        let value = self.request("eth_blockNumber", json!([])).await?;
        parse_hex_u64_value(&value)
    }

    pub async fn get_block_by_number(&self, tag: &str) -> Result<(u64, B256, B256)> {
        let value = self
            .request("eth_getBlockByNumber", json!([tag, false]))
            .await?;
        let number = parse_hex_u64(value.get("number").context("block missing number")?)?;
        let hash = parse_b256(value.get("hash").context("block missing hash")?)?;
        let parent = parse_b256(
            value
                .get("parentHash")
                .context("block missing parentHash")?,
        )?;
        Ok((number, hash, parent))
    }

    pub async fn get_logs(
        &self,
        from_block: u64,
        to_block: u64,
        addresses: &[Address],
        topics: &[B256],
    ) -> Result<Vec<RpcLog>> {
        let address_values = addresses
            .iter()
            .map(|addr| Value::String(addr.to_string()))
            .collect::<Vec<_>>();
        let topic_values = topics
            .iter()
            .map(|topic| Value::String(topic.to_string()))
            .collect::<Vec<_>>();

        let mut filter = json!({
            "fromBlock": format!("0x{:x}", from_block),
            "toBlock": format!("0x{:x}", to_block),
        });
        if !addresses.is_empty() {
            filter["address"] = json!(address_values);
        }
        if !topics.is_empty() {
            filter["topics"] = json!([topic_values]);
        }

        let value = self.request("eth_getLogs", json!([filter])).await?;

        let logs = value
            .as_array()
            .context("eth_getLogs did not return array")?;
        logs.iter().map(parse_log).collect()
    }

    pub async fn get_logs_by_tag(
        &self,
        from_block: &str,
        to_block: &str,
        addresses: &[Address],
        topics: &[B256],
    ) -> Result<Vec<RpcLog>> {
        let address_values = addresses
            .iter()
            .map(|addr| Value::String(addr.to_string()))
            .collect::<Vec<_>>();
        let topic_values = topics
            .iter()
            .map(|topic| Value::String(topic.to_string()))
            .collect::<Vec<_>>();

        let mut filter = json!({
            "fromBlock": from_block,
            "toBlock": to_block,
        });
        if !addresses.is_empty() {
            filter["address"] = json!(address_values);
        }
        if !topics.is_empty() {
            filter["topics"] = json!([topic_values]);
        }

        let value = self.request("eth_getLogs", json!([filter])).await?;
        let logs = value
            .as_array()
            .context("eth_getLogs did not return array")?;
        logs.iter().map(parse_log).collect()
    }

    pub async fn get_block_receipts_logs(&self, block_number: u64) -> Result<Vec<RpcLog>> {
        let value = self
            .request(
                "eth_getBlockReceipts",
                json!([format!("0x{:x}", block_number)]),
            )
            .await?;
        let receipts = value
            .as_array()
            .context("eth_getBlockReceipts did not return array")?;
        let mut out = Vec::new();
        for receipt in receipts {
            let Some(logs) = receipt.get("logs").and_then(Value::as_array) else {
                continue;
            };
            for log in logs {
                out.push(parse_log(log)?);
            }
        }
        Ok(out)
    }

    pub async fn eth_call(
        &self,
        to: Address,
        from: Option<Address>,
        data: Bytes,
        block_tag: &str,
    ) -> Result<Bytes> {
        let call = CallRequest {
            to,
            from,
            data: data.to_string(),
            value: None,
        };
        let value = self.request("eth_call", json!([call, block_tag])).await?;
        parse_bytes(value)
    }

    pub async fn estimate_gas(
        &self,
        to: Address,
        from: Option<Address>,
        data: Bytes,
    ) -> Result<u64> {
        let call = CallRequest {
            to,
            from,
            data: data.to_string(),
            value: None,
        };
        let value = self.request("eth_estimateGas", json!([call])).await?;
        parse_hex_u64_value(&value)
    }

    pub async fn gas_price(&self) -> Result<u128> {
        let value = self.request("eth_gasPrice", json!([])).await?;
        parse_hex_u128_value(&value)
    }

    pub async fn max_priority_fee_per_gas(&self) -> Result<u128> {
        let value = self.request("eth_maxPriorityFeePerGas", json!([])).await?;
        parse_hex_u128_value(&value)
    }

    pub async fn get_transaction_count(&self, address: Address, tag: &str) -> Result<u64> {
        let value = self
            .request("eth_getTransactionCount", json!([address.to_string(), tag]))
            .await?;
        parse_hex_u64_value(&value)
    }

    pub async fn get_transaction_receipt_status(&self, tx_hash: B256) -> Result<Option<bool>> {
        let value = self
            .request("eth_getTransactionReceipt", json!([tx_hash.to_string()]))
            .await?;
        parse_receipt_status(&value)
    }

    pub async fn send_raw_transaction_with_method(
        &self,
        method: &str,
        raw_tx: &str,
    ) -> Result<B256> {
        let params = match method {
            "eth_sendPrivateTransaction" => json!([{ "tx": raw_tx }]),
            _ => json!([raw_tx]),
        };
        let value = self.request(method, params).await?;
        parse_tx_hash_result(&value)
    }

    pub async fn get_code(&self, address: Address, tag: &str) -> Result<Bytes> {
        let value = self
            .request("eth_getCode", json!([address.to_string(), tag]))
            .await?;
        parse_bytes(value)
    }

    pub async fn custom_simulate(&self, method: &str, params: Value) -> Result<Value> {
        self.request(method, params).await
    }
}

pub fn parse_log(value: &Value) -> Result<RpcLog> {
    let address = parse_address(value.get("address").context("log missing address")?)?;
    let topics = value
        .get("topics")
        .and_then(Value::as_array)
        .context("log missing topics")?
        .iter()
        .map(parse_b256)
        .collect::<Result<Vec<_>>>()?;
    let data = parse_bytes(value.get("data").context("log missing data")?.clone())?;
    let block_number = value.get("blockNumber").and_then(|v| parse_hex_u64(v).ok());
    let block_hash = value.get("blockHash").and_then(|v| parse_b256(v).ok());
    let tx_hash = value
        .get("transactionHash")
        .and_then(|v| parse_b256(v).ok());

    Ok(RpcLog {
        address,
        topics,
        data,
        block_number,
        block_hash,
        tx_hash,
    })
}

pub fn parse_address(value: &Value) -> Result<Address> {
    let text = value.as_str().context("address must be hex string")?;
    text.parse()
        .with_context(|| format!("invalid address: {text}"))
}

pub fn parse_b256(value: &Value) -> Result<B256> {
    let text = value.as_str().context("hash must be hex string")?;
    text.parse()
        .with_context(|| format!("invalid hash: {text}"))
}

pub fn parse_bytes(value: Value) -> Result<Bytes> {
    let text = value.as_str().context("bytes must be hex string")?;
    text.parse()
        .with_context(|| format!("invalid bytes: {text}"))
}

pub fn parse_hex_u64_value(value: &Value) -> Result<u64> {
    parse_hex_u64(value)
}

pub fn parse_hex_u128_value(value: &Value) -> Result<u128> {
    parse_hex_u128(value)
}

pub fn parse_hex_u64(value: &Value) -> Result<u64> {
    let text = value.as_str().context("u64 hex value must be string")?;
    let stripped = text.trim_start_matches("0x");
    if stripped.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(stripped, 16).with_context(|| format!("invalid hex u64: {text}"))
}

pub fn parse_hex_u128(value: &Value) -> Result<u128> {
    let text = value.as_str().context("u128 hex value must be string")?;
    let stripped = text.trim_start_matches("0x");
    if stripped.is_empty() {
        return Ok(0);
    }
    u128::from_str_radix(stripped, 16).with_context(|| format!("invalid hex u128: {text}"))
}

pub fn parse_hex_u256(value: &Value) -> Result<U256> {
    let text = value.as_str().context("u256 hex value must be string")?;
    U256::from_str_radix(text.trim_start_matches("0x"), 16)
        .with_context(|| format!("invalid hex u256: {text}"))
}

fn parse_tx_hash_result(value: &Value) -> Result<B256> {
    if value.is_string() {
        return parse_b256(value);
    }
    if let Some(hash) = value.get("txHash").or_else(|| value.get("transactionHash")) {
        return parse_b256(hash);
    }
    anyhow::bail!("submit result did not contain tx hash: {value}")
}

fn parse_receipt_status(value: &Value) -> Result<Option<bool>> {
    if value.is_null() {
        return Ok(None);
    }
    let status = value
        .get("status")
        .map(parse_hex_u64)
        .transpose()?
        .unwrap_or(1);
    Ok(Some(status == 1))
}

pub fn rpc_compute_units(method: &str) -> u64 {
    match method {
        "net_version" | "eth_chainId" | "eth_syncing" | "eth_protocolVersion" | "net_listening" => {
            0
        }
        "eth_blockNumber"
        | "eth_subscribe"
        | "eth_unsubscribe"
        | "eth_feeHistory"
        | "eth_maxPriorityFeePerGas"
        | "eth_blobBaseFee"
        | "eth_createAccessList" => 10,
        "eth_getTransactionReceipt"
        | "eth_getBlockByNumber"
        | "eth_getBlockByHash"
        | "eth_getBlockReceipts"
        | "eth_getTransactionCount"
        | "eth_getCode"
        | "eth_gasPrice"
        | "eth_estimateGas"
        | "eth_getStorageAt"
        | "eth_getTransactionByHash"
        | "eth_getRawTransactionByHash" => 20,
        "eth_call" => 26,
        "eth_simulateV1" | "eth_callBundle" | "eth_sendRawTransaction" => 40,
        "eth_getLogs" | "eth_getFilterLogs" => 60,
        "eth_callMany" => 20,
        _ => 20,
    }
}

/// Get chain-aware eth_getLogs maximum block range for Pay As You Go
pub fn max_log_range_for_chain(chain: Chain) -> u64 {
    match chain {
        Chain::Base => 10_000, // Alchemy Base eth_getLogs is capped at 10,000 blocks.
        Chain::Polygon => 2_000, // Polygon Pay As You Go limited to 2000 blocks
    }
}

/// Adaptive log chunk size based on chain and provider mode
pub fn adapt_log_chunk_size(chain: Chain, provider_mode: &str, current_size: u64) -> u64 {
    let max_allowed = max_log_range_for_chain(chain);
    let adaptive_max = match provider_mode.to_lowercase().as_str() {
        "payg" | "payasyougo" | "free" => max_allowed,
        "dedicated" | "growth" | "scale" => {
            // Higher tiers may support larger ranges
            std::cmp::max(max_allowed, 10_000)
        }
        _ => max_allowed,
    };

    // Start with current size, but cap at adaptive maximum
    // If current_size is 0, use a reasonable default
    if current_size == 0 {
        std::cmp::min(5_000, adaptive_max)
    } else {
        std::cmp::min(current_size, adaptive_max)
    }
}

/// Check if a log range error indicates we should reduce chunk size
pub fn should_reduce_chunk_size(error: &str) -> bool {
    let error_lower = error.to_lowercase();
    error_lower.contains("query returned more than")
        || error_lower.contains("exceeds range limit")
        || error_lower.contains("block range too large")
        || error_lower.contains("payload too large")
        || error_lower.contains("limited to a 10,000 range")
        || error_lower.contains("too many results")
        || error_lower.contains("timeout")
        || error_lower.contains("timed out")
        || error_lower.contains("413")
}

async fn acquire_rpc_budget(method: &str) {
    let cu = rpc_compute_units(method);
    let limiter = rpc_cu_limiter();
    while !limiter.try_consume(cu) {
        sleep(limiter.wait_duration(cu)).await;
    }
}

fn rpc_cu_limiter() -> &'static RpcCuLimiter {
    RPC_CU_LIMITER.get_or_init(|| {
        let refill_rate = env_u64("RPC_CU_PER_SECOND", 10_000).max(1);
        let capacity = env_u64("RPC_CU_BURST", refill_rate).max(refill_rate);
        RpcCuLimiter::new(capacity, refill_rate)
    })
}

fn record_rpc_usage_attempt(method: &str, provider: &str, cu: u64) {
    let interval = rpc_usage_log_interval();
    if interval.is_zero() {
        return;
    }
    let mut usage = RPC_USAGE_ACCUMULATOR
        .get_or_init(|| parking_lot::Mutex::new(RpcUsageAccumulator::new()))
        .lock();
    let key = format!("{method}@{provider}");
    let entry = usage.by_method_provider.entry(key).or_default();
    entry.requests = entry.requests.saturating_add(1);
    entry.cu = entry.cu.saturating_add(cu);

    if usage.since.elapsed() < interval {
        return;
    }

    let total_requests = usage
        .by_method_provider
        .values()
        .map(|entry| entry.requests)
        .sum::<u64>();
    let total_cu = usage
        .by_method_provider
        .values()
        .map(|entry| entry.cu)
        .sum::<u64>();
    let mut methods = usage
        .by_method_provider
        .iter()
        .map(|(key, entry)| format!("{key}:requests={},cu={}", entry.requests, entry.cu))
        .collect::<Vec<_>>();
    methods.sort();
    info!(
        interval_secs = usage.since.elapsed().as_secs(),
        total_requests,
        total_cu,
        methods = %methods.join(","),
        "rpc usage summary"
    );

    usage.by_method_provider.clear();
    usage.since = Instant::now();
}

fn rpc_usage_log_interval() -> Duration {
    Duration::from_secs(
        std::env::var("RPC_USAGE_LOG_INTERVAL_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(30),
    )
}

fn rpc_max_retries(method: &str) -> usize {
    if matches!(
        method,
        "eth_sendRawTransaction" | "eth_sendPrivateTransaction" | "flashbots_sendBundle"
    ) {
        return env_usize("RPC_SUBMIT_MAX_RETRIES", 0);
    }
    match method {
        "eth_getLogs" | "eth_getBlockReceipts" => env_usize("RPC_LOG_MAX_RETRIES", 3),
        "eth_call" | "eth_estimateGas" => env_usize("RPC_SIMULATION_MAX_RETRIES", 2),
        "eth_getTransactionReceipt" | "eth_getTransactionByHash" => {
            env_usize("RPC_RECEIPT_MAX_RETRIES", 2)
        }
        _ => env_usize("RPC_MAX_RETRIES", 1),
    }
}

fn rpc_is_idempotent(method: &str) -> bool {
    !matches!(
        method,
        "eth_sendRawTransaction" | "eth_sendPrivateTransaction" | "flashbots_sendBundle"
    )
}

fn rpc_timeout(method: &str) -> Duration {
    let default_ms = match method {
        "eth_getLogs" | "eth_getBlockReceipts" => 60_000,
        "eth_call" | "eth_estimateGas" => 30_000,
        "eth_sendRawTransaction" | "eth_sendPrivateTransaction" => 10_000,
        _ => 20_000,
    };
    Duration::from_millis(env_u64("RPC_REQUEST_TIMEOUT_MS", default_ms))
}

fn rpc_backoff(method: &str, attempt: usize) -> Duration {
    let base = env_u64("RPC_RETRY_BASE_DELAY_MS", 100);
    let exp = 2_u64.saturating_pow(attempt as u32);
    let jitter = method.bytes().fold(attempt as u64, |acc, byte| {
        acc.wrapping_mul(31) ^ u64::from(byte)
    }) % 50;
    Duration::from_millis(base.saturating_mul(exp).saturating_add(jitter))
}

fn endpoint_status_key(endpoint: &str, method: &str) -> String {
    format!("{method} {endpoint}")
}

fn should_record_endpoint_failure(method: &str, err: &anyhow::Error) -> bool {
    if !matches!(method, "eth_call" | "eth_estimateGas") {
        return true;
    }

    let message = err.to_string().to_ascii_lowercase();
    if message.starts_with("rpc error") {
        return message.contains("rate limit")
            || message.contains("too many")
            || message.contains("compute unit")
            || message.contains("capacity")
            || message.contains("temporarily unavailable");
    }

    true
}

fn compact_unique_urls<const N: usize>(urls: [Option<String>; N]) -> Vec<String> {
    let mut out = Vec::new();
    for url in urls.into_iter().flatten() {
        if !out.iter().any(|existing| existing == &url) {
            out.push(url);
        }
    }
    out
}

fn provider_label(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| "unknown".to_string())
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use alloy::primitives::B256;
    use serde_json::{json, Value};

    use super::{parse_receipt_status, parse_tx_hash_result};

    #[test]
    fn parses_string_submit_tx_hash() {
        let hash = B256::from([7u8; 32]);

        assert_eq!(
            parse_tx_hash_result(&json!(hash.to_string())).unwrap(),
            hash
        );
    }

    #[test]
    fn parses_object_submit_tx_hash() {
        let hash = B256::from([8u8; 32]);

        assert_eq!(
            parse_tx_hash_result(&json!({ "txHash": hash.to_string() })).unwrap(),
            hash
        );
    }

    #[test]
    fn parses_transaction_hash_submit_tx_hash() {
        let hash = B256::from([9u8; 32]);

        assert_eq!(
            parse_tx_hash_result(&json!({ "transactionHash": hash.to_string() })).unwrap(),
            hash
        );
    }

    #[test]
    fn rejects_missing_submit_tx_hash() {
        assert!(parse_tx_hash_result(&json!({ "status": "0x1" })).is_err());
    }

    #[test]
    fn parses_receipt_status_null_as_pending() {
        assert_eq!(parse_receipt_status(&Value::Null).unwrap(), None);
    }

    #[test]
    fn parses_receipt_status_with_default_success_when_missing_status() {
        assert_eq!(parse_receipt_status(&json!({})).unwrap(), Some(true));
    }

    #[test]
    fn parses_receipt_status_reverted_and_included() {
        assert_eq!(
            parse_receipt_status(&json!({ "status": "0x0" })).unwrap(),
            Some(false)
        );
        assert_eq!(
            parse_receipt_status(&json!({ "status": "0x1" })).unwrap(),
            Some(true)
        );
    }
}
