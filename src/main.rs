
use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use dex_arbitrage::{
    config::Settings,
    engine::run_chain,
    monitoring::{logger, metrics},
    types::Chain,
};

#[derive(Debug, Parser)]
#[command(author, version, about = "DEX N-Hop cyclic arbitrage bot")]
struct Cli {
    #[arg(long, value_parser = ["base", "polygon"])]
    chain: String,
    #[arg(long, default_value_t = false)]
    once: bool,
    #[arg(long, default_value_t = false)]
    simulate_only: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let cli = Cli::parse();
    let chain: Chain = cli.chain.parse()?;
    let settings = Arc::new(Settings::load(chain)?);

    logger::init(&settings)?;
    metrics::install(&settings)?;

    run_chain(settings, cli.once, cli.simulate_only).await
}
