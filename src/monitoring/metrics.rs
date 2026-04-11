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
