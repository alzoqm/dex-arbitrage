use std::{collections::HashMap, sync::Arc};

use alloy::primitives::Address;
use dex_arbitrage::{
    config::{
        ContractSettings, RiskSettings, RpcSettings, SearchSettings, Settings, UniversePolicy,
    },
    graph::GraphSnapshot,
    router::quantity_search::QuantitySearcher,
    types::{CandidatePath, Chain, DiscoveryKind, TokenBehavior, TokenInfo},
};
use smallvec::SmallVec;

fn addr(byte: u8) -> Address {
    Address::from_slice(&[byte; 20])
}

fn settings() -> Arc<Settings> {
    Arc::new(Settings {
        chain: Chain::Base,
        chain_id: 8453,
        native_symbol: "ETH".to_string(),
        operator_private_key: None,
        deployer_private_key: None,
        safe_owner: None,
        simulation_only: true,
        json_logs: false,
        prometheus_bind: "127.0.0.1:9898".to_string(),
        rpc: RpcSettings {
            public_rpc_url: "http://localhost:8545".to_string(),
            fallback_rpc_url: None,
            preconf_rpc_url: None,
            ws_url: None,
            protected_rpc_url: None,
            private_submit_method: "eth_sendRawTransaction".to_string(),
            simulate_method: "eth_call".to_string(),
        },
        contracts: ContractSettings {
            executor_address: Some(addr(99)),
            aave_pool: None,
            strict_target_allowlist: true,
        },
        risk: RiskSettings {
            max_hops: 4,
            screening_margin_bps: 3,
            min_net_profit: 1,
            min_net_profit_usd_e8: 10_000_000,
            min_trade_usd_e8: 50_000_000,
            poll_interval_ms: 400,
            event_backfill_blocks: 10,
            staleness_timeout_ms: 60_000,
            gas_risk_buffer_pct: 0.15,
            gas_price_ceiling_wei: 3_000_000_000,
            max_position: 1_000_000,
            max_position_usd_e8: 200_000_000,
            max_flash_loan: 1_000_000_000,
            max_flash_loan_usd_e8: 1_000_000_000_000,
            daily_loss_limit: -1_000_000,
            daily_loss_limit_usd_e8: 50_000_000_000,
            min_profit_realization_bps: 9000,
            max_concurrent_tx: 1,
            pool_health_min_bps: 9_000,
            stable_depeg_cutoff_e6: 995_000,
        },
        search: SearchSettings::default(),
        policy: UniversePolicy::default(),
        tokens: Vec::new(),
        dexes: vec![dex_arbitrage::config::DexConfig {
            name: "uniswap_v2".to_string(),
            amm_kind: dex_arbitrage::types::AmmKind::UniswapV2Like,
            discovery_kind: DiscoveryKind::FactoryAllPairs,
            factory: None,
            registry: None,
            vault: None,
            quoter: None,
            fee_ppm: 3_000,
            start_block: 0,
            enabled: true,
        }],
    })
}

fn token(price: Option<u64>, max_position_usd_e8: Option<u128>) -> TokenInfo {
    TokenInfo {
        address: addr(1),
        symbol: "USDC".to_string(),
        decimals: 6,
        is_stable: true,
        is_cycle_anchor: true,
        flash_loan_enabled: false,
        allow_self_funded: true,
        behavior: TokenBehavior::default(),
        manual_price_usd_e8: price,
        max_position_usd_e8,
        max_flash_loan_usd_e8: None,
    }
}

fn candidate() -> CandidatePath {
    CandidatePath {
        snapshot_id: 1,
        start_token: addr(1),
        start_symbol: "USDC".to_string(),
        screening_score_q32: 0,
        cycle_key: "cycle".to_string(),
        path: SmallVec::new(),
    }
}

#[test]
fn search_range_uses_usd_min_trade_and_token_specific_position_cap() {
    let snapshot = GraphSnapshot::build(
        1,
        None,
        vec![token(Some(100_000_000), Some(300_000_000))],
        HashMap::new(),
    );
    let searcher = QuantitySearcher::new(settings());

    let range = searcher.search_range(&snapshot, &candidate()).unwrap();

    assert_eq!(range, (500_000, 3_000_000));
}

#[test]
fn search_range_falls_back_to_legacy_raw_cap_when_price_missing() {
    let snapshot = GraphSnapshot::build(1, None, vec![token(None, None)], HashMap::new());
    let searcher = QuantitySearcher::new(settings());

    let range = searcher.search_range(&snapshot, &candidate()).unwrap();

    assert_eq!(range, (10_000, 1_000_000));
}

#[test]
fn refinement_points_are_positive_unique_and_within_max() {
    let searcher = QuantitySearcher::new(settings());
    let points = searcher.refinement_points(100, 125);

    assert!(points.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(points.iter().all(|amount| *amount > 0 && *amount <= 125));
    assert!(points.contains(&100));
    assert!(points.contains(&125));
}
