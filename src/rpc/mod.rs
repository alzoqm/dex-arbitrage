use std::sync::atomic::{AtomicU64, Ordering};

use alloy::primitives::{Address, B256, Bytes, U256};
use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::RpcSettings;

#[derive(Debug)]
pub struct RpcClient {
    url: String,
    http: reqwest::Client,
    request_id: AtomicU64,
}

#[derive(Debug, Clone)]
pub struct RpcClients {
    pub public: std::sync::Arc<RpcClient>,
    pub fallback: Option<std::sync::Arc<RpcClient>>,
    pub preconf: Option<std::sync::Arc<RpcClient>>,
    pub protected: Option<std::sync::Arc<RpcClient>>,
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
        Ok(Self {
            public: std::sync::Arc::new(RpcClient::new(settings.public_rpc_url.clone())?),
            fallback: settings
                .fallback_rpc_url
                .clone()
                .map(RpcClient::new)
                .transpose()?
                .map(std::sync::Arc::new),
            preconf: settings
                .preconf_rpc_url
                .clone()
                .map(RpcClient::new)
                .transpose()?
                .map(std::sync::Arc::new),
            protected: settings
                .protected_rpc_url
                .clone()
                .map(RpcClient::new)
                .transpose()?
                .map(std::sync::Arc::new),
        })
    }

    pub fn best_read(&self) -> std::sync::Arc<RpcClient> {
        self.preconf.clone().unwrap_or_else(|| self.public.clone())
    }

    pub fn best_write(&self) -> std::sync::Arc<RpcClient> {
        self.protected.clone().unwrap_or_else(|| self.public.clone())
    }
}

impl RpcClient {
    pub fn new(url: String) -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        Ok(Self {
            url,
            http: reqwest::Client::builder()
                .default_headers(headers)
                .build()
                .context("failed to build reqwest client")?,
            request_id: AtomicU64::new(1),
        })
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.request_id.fetch_add(1, Ordering::Relaxed);
        let payload = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let response = self
            .http
            .post(&self.url)
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
            anyhow::bail!("rpc error {} {} for method {}", err.code, err.message, method);
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
        let parent = parse_b256(value.get("parentHash").context("block missing parentHash")?)?;
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
            "address": address_values,
        });
        if !topics.is_empty() {
            filter["topics"] = json!([topic_values]);
        }

        let value = self.request("eth_getLogs", json!([filter])).await?;

        let logs = value.as_array().context("eth_getLogs did not return array")?;
        logs.iter().map(parse_log).collect()
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
        let value = self
            .request("eth_maxPriorityFeePerGas", json!([]))
            .await?;
        parse_hex_u128_value(&value)
    }

    pub async fn get_transaction_count(&self, address: Address, tag: &str) -> Result<u64> {
        let value = self
            .request("eth_getTransactionCount", json!([address.to_string(), tag]))
            .await?;
        parse_hex_u64_value(&value)
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

fn parse_log(value: &Value) -> Result<RpcLog> {
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
    let tx_hash = value.get("transactionHash").and_then(|v| parse_b256(v).ok());

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
    text.parse().with_context(|| format!("invalid address: {text}"))
}

pub fn parse_b256(value: &Value) -> Result<B256> {
    let text = value.as_str().context("hash must be hex string")?;
    text.parse().with_context(|| format!("invalid hash: {text}"))
}

pub fn parse_bytes(value: Value) -> Result<Bytes> {
    let text = value.as_str().context("bytes must be hex string")?;
    text.parse().with_context(|| format!("invalid bytes: {text}"))
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
