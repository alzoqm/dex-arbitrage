//! Comprehensive Prometheus metrics for the arbitrage bot.
//!
//! Provides metrics for:
//! - Opportunity funnel (detected, validated, submitted, included, reverted)
//! - RPC usage (CU by chain/provider/method, request counts, latency)
//! - Cache hit/miss rates
//! - Quote mismatch tracking
//! - Reorg detection
//! - State lag
//! - Route latency
//! - PnL tracking
//! - Revert reason breakdown

use anyhow::{Context, Result};
use metrics_exporter_prometheus::PrometheusBuilder;

use crate::config::Settings;

pub fn install(settings: &Settings) -> Result<()> {
    let builder = PrometheusBuilder::new();
    let addr: std::net::SocketAddr = settings
        .prometheus_bind
        .parse()
        .with_context(|| format!("invalid PROMETHEUS_BIND: {}", settings.prometheus_bind))?;
    builder
        .with_http_listener(addr)
        .install()
        .context("failed to install prometheus exporter")?;
    Ok(())
}

// Convenience functions for emitting metrics

pub fn record_rpc_request(method: &str, chain: &str, status: &str, latency_secs: f64) {
    metrics::counter!("rpc_requests_total", "method" => method.to_string(), "chain" => chain.to_string(), "status" => status.to_string()).increment(1);
    metrics::histogram!("rpc_latency_seconds", "method" => method.to_string(), "chain" => chain.to_string())
        .record(latency_secs);
}

pub fn record_rpc_compute_units(method: &str, chain: &str, cu: u64) {
    metrics::counter!("rpc_cu_total", "method" => method.to_string(), "chain" => chain.to_string())
        .increment(cu);
}

pub fn record_rpc_429(chain: &str, method: &str) {
    metrics::counter!("rpc_429_total", "chain" => chain.to_string(), "method" => method.to_string()).increment(1);
}

pub fn record_rpc_error(chain: &str, method: &str, error_type: &str) {
    metrics::counter!("rpc_errors_total", "chain" => chain.to_string(), "method" => method.to_string(), "error_type" => error_type.to_string()).increment(1);
}

pub fn record_quote_rpc(chain: &str, method: &str) {
    metrics::counter!("quote_rpc_calls_total", "chain" => chain.to_string(), "method" => method.to_string()).increment(1);
}

pub fn record_quote_cache_hit(pool: &str, chain: &str) {
    metrics::counter!("quote_cache_hits_total", "pool" => pool.to_string(), "chain" => chain.to_string()).increment(1);
}

pub fn record_quote_cache_miss(pool: &str, chain: &str) {
    metrics::counter!("quote_cache_misses_total", "pool" => pool.to_string(), "chain" => chain.to_string()).increment(1);
}

pub fn record_quote_mismatch(pool: &str, chain: &str) {
    metrics::counter!("quote_mismatches_total", "pool" => pool.to_string(), "chain" => chain.to_string()).increment(1);
}

pub fn record_candidate_detected(snapshot_id: u64, chain: &str) {
    metrics::gauge!("candidates_detected", "snapshot_id" => snapshot_id.to_string(), "chain" => chain.to_string()).increment(1);
}

pub fn record_candidate_validated(snapshot_id: u64, chain: &str) {
    metrics::gauge!("candidates_validated", "snapshot_id" => snapshot_id.to_string(), "chain" => chain.to_string()).increment(1);
}

pub fn record_candidate_rejected(reason: &str, chain: &str) {
    metrics::counter!("candidates_rejected_total", "reason" => reason.to_string(), "chain" => chain.to_string()).increment(1);
}

pub fn record_opportunity_found(snapshot_id: u64, profit_usd_e8: i64, chain: &str) {
    metrics::counter!("opportunities_found_total", "snapshot_id" => snapshot_id.to_string(), "chain" => chain.to_string()).increment(1);
    metrics::gauge!("opportunity_profit_usd_e8", "chain" => chain.to_string())
        .set(profit_usd_e8 as f64);
}

pub fn record_transaction_submitted(tx_hash: &str, channel: &str, chain: &str) {
    metrics::counter!("transactions_submitted_total", "channel" => channel.to_string(), "chain" => chain.to_string()).increment(1);
    metrics::gauge!("latest_tx_hash", "chain" => chain.to_string())
        .set(tx_hash_low64(tx_hash) as f64);
}

pub fn record_transaction_included(_tx_hash: &str, chain: &str, gas_used: u64) {
    metrics::counter!("transactions_included_total", "chain" => chain.to_string()).increment(1);
    metrics::histogram!("transaction_gas_used", "chain" => chain.to_string())
        .record(gas_used as f64);
}

pub fn record_transaction_reverted(_tx_hash: &str, chain: &str, reason: &str) {
    metrics::counter!("transactions_reverted_total", "reason" => reason.to_string(), "chain" => chain.to_string()).increment(1);
}

pub fn record_transaction_failed(_tx_hash: &str, chain: &str, error_type: &str) {
    metrics::counter!("transactions_failed_total", "error_type" => error_type.to_string(), "chain" => chain.to_string()).increment(1);
}

pub fn record_pnl_realized(profit_usd_e8: i64, chain: &str) {
    metrics::gauge!("pnl_realized_usd_e8", "chain" => chain.to_string()).set(profit_usd_e8 as f64);
}

pub fn record_pnl_expected(expected_profit_usd_e8: i64, chain: &str) {
    metrics::gauge!("pnl_expected_usd_e8", "chain" => chain.to_string())
        .set(expected_profit_usd_e8 as f64);
}

pub fn record_reorg_detected(depth: usize, from_block: u64, to_block: u64, chain: &str) {
    metrics::counter!("reorgs_detected_total", "chain" => chain.to_string()).increment(1);
    metrics::gauge!("reorg_depth", "chain" => chain.to_string()).set(depth as f64);
    metrics::gauge!("reorg_from_block", "chain" => chain.to_string()).set(from_block as f64);
    metrics::gauge!("reorg_to_block", "chain" => chain.to_string()).set(to_block as f64);
}

pub fn record_reorg_rollback(from_block: u64, to_block: u64, chain: &str) {
    metrics::counter!("reorg_rollbacks_total", "chain" => chain.to_string()).increment(1);
    metrics::gauge!("reorg_rollback_from_block", "chain" => chain.to_string())
        .set(from_block as f64);
    metrics::gauge!("reorg_rollback_to_block", "chain" => chain.to_string()).set(to_block as f64);
}

pub fn record_state_lag(blocks_behind: u64, chain: &str) {
    metrics::gauge!("state_lag_blocks", "chain" => chain.to_string()).set(blocks_behind as f64);
}

pub fn record_cache_hit(cache_name: &str, key_type: &str) {
    metrics::counter!("cache_hits_total", "cache" => cache_name.to_string(), "key_type" => key_type.to_string()).increment(1);
}

pub fn record_cache_miss(cache_name: &str, key_type: &str) {
    metrics::counter!("cache_misses_total", "cache" => cache_name.to_string(), "key_type" => key_type.to_string()).increment(1);
}

pub fn record_cache_flush(cache_name: &str, reason: &str) {
    metrics::counter!("cache_flushes_total", "cache" => cache_name.to_string(), "reason" => reason.to_string()).increment(1);
}

pub fn record_route_latency_seconds(route_type: &str, duration_secs: f64) {
    metrics::histogram!("route_latency_seconds", "route_type" => route_type.to_string())
        .record(duration_secs);
}

pub fn record_pool_refresh(pool_id: &str, success: bool, duration_secs: f64) {
    metrics::counter!("pool_refreshes_total", "pool_id" => pool_id.to_string(), "success" => success.to_string()).increment(1);
    metrics::histogram!("pool_refresh_duration_seconds", "pool_id" => pool_id.to_string())
        .record(duration_secs);
}

pub fn record_pool_health(pool_id: &str, health_status: &str) {
    metrics::gauge!("pool_health", "pool_id" => pool_id.to_string(), "status" => health_status.to_string())
        .set(if health_status == "healthy" { 1.0 } else { 0.0 });
}

pub fn record_event_ingestion(source: &str, events_count: usize, chain: &str) {
    metrics::counter!("events_ingested_total", "source" => source.to_string(), "chain" => chain.to_string()).increment(events_count as u64);
}

pub fn record_snapshot_update(
    snapshot_id: u64,
    pools_refreshed: usize,
    changed_edges: usize,
    chain: &str,
) {
    metrics::gauge!("snapshot_id", "chain" => chain.to_string()).set(snapshot_id as f64);
    metrics::counter!("pools_refreshed_total", "chain" => chain.to_string())
        .increment(pools_refreshed as u64);
    metrics::counter!("changed_edges_total", "chain" => chain.to_string())
        .increment(changed_edges as u64);
}

pub fn record_nonce_manager_state(
    pending_count: usize,
    reserved_count: usize,
    dropped_count: usize,
) {
    metrics::gauge!("nonce_pending").set(pending_count as f64);
    metrics::gauge!("nonce_reserved").set(reserved_count as f64);
    metrics::gauge!("nonce_dropped").set(dropped_count as f64);
}

pub fn record_nonce_replaced(original_nonce: u64, replacement_nonce: u64) {
    metrics::counter!("nonce_replacements_total").increment(1);
    metrics::gauge!("nonce_replaced_from").set(original_nonce as f64);
    metrics::gauge!("nonce_replaced_to").set(replacement_nonce as f64);
}

pub fn record_stable_token_depeg(token: &str, price_usd_e8: u64, cutoff_e8: u64, chain: &str) {
    metrics::counter!("stable_token_depegs_total", "token" => token.to_string(), "chain" => chain.to_string()).increment(1);
    metrics::gauge!("stable_token_price_usd_e8", "token" => token.to_string(), "chain" => chain.to_string())
        .set(price_usd_e8 as f64);
    metrics::gauge!("stable_token_depeg_cutoff_e8", "token" => token.to_string(), "chain" => chain.to_string())
        .set(cutoff_e8 as f64);
}

pub fn record_gas_estimate(estimated: u64, actual: u64, chain: &str) {
    metrics::gauge!("gas_estimate", "chain" => chain.to_string()).set(estimated as f64);
    metrics::gauge!("gas_actual", "chain" => chain.to_string()).set(actual as f64);
    if estimated > 0 {
        let ratio = actual as f64 / estimated as f64;
        metrics::gauge!("gas_estimate_ratio", "chain" => chain.to_string()).set(ratio);
    }
}

pub fn record_simulation_latency(duration_secs: f64, success: bool) {
    metrics::histogram!("simulation_latency_seconds").record(duration_secs);
    metrics::counter!("simulations_total", "success" => success.to_string()).increment(1);
}

fn tx_hash_low64(tx_hash: &str) -> u64 {
    let hex = tx_hash.trim_start_matches("0x");
    let start = hex.len().saturating_sub(16);
    u64::from_str_radix(&hex[start..], 16).unwrap_or(0)
}
