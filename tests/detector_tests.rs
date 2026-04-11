use std::{collections::HashMap, sync::Arc, time::SystemTime};

use alloy::primitives::Address;
use dex_arbitrage::{
    config::{ContractSettings, DexConfig, RiskSettings, RpcSettings, Settings, TokenConfig},
    detector::Detector,
    graph::{DistanceCache, GraphSnapshot},
    types::{
        AmmKind, Chain, EdgeRef, PoolAdmissionStatus, PoolHealth, PoolSpecificState, PoolState,
        TokenBehavior, TokenInfo, V2PoolState,
    },
};

fn addr(byte: u8) -> Address {
    Address::from_slice(&[byte; 20])
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

fn token(symbol: &str, address: Address, decimals: u8, stable: bool) -> TokenInfo {
    TokenInfo {
        address,
        symbol: symbol.to_string(),
        decimals,
        is_stable: stable,
        behavior: TokenBehavior::default(),
        manual_price_usd_e8: stable.then_some(100_000_000),
    }
}

fn pool(pool_id: Address, a: Address, b: Address, ra: u128, rb: u128) -> PoolState {
    PoolState {
        pool_id,
        dex_name: "uniswap_v2".to_string(),
        kind: AmmKind::UniswapV2Like,
        token_addresses: vec![a, b],
        token_symbols: vec!["A".to_string(), "B".to_string()],
        factory: None,
        registry: None,
        vault: None,
        quoter: None,
        admission_status: PoolAdmissionStatus::Allowed,
        health: health(),
        state: PoolSpecificState::UniswapV2Like(V2PoolState {
            reserve0: ra,
            reserve1: rb,
            fee_ppm: 3_000,
        }),
        last_updated_block: 1,
        extras: HashMap::new(),
    }
}

fn settings() -> Arc<Settings> {
    Arc::new(Settings {
        chain: Chain::Base,
        chain_id: 8453,
        native_symbol: "ETH".to_string(),
        operator_private_key: Some("0x01".to_string()),
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
            executor_address: Some(addr(100)),
            aave_pool: None,
            strict_target_allowlist: false,
        },
        risk: RiskSettings {
            max_hops: 4,
            screening_margin_bps: 3,
            min_net_profit: 1,
            poll_interval_ms: 400,
            event_backfill_blocks: 10,
            staleness_timeout_ms: 60_000,
            gas_risk_buffer_pct: 0.15,
            gas_price_ceiling_wei: 3_000_000_000,
            max_position: 1_000_000_000,
            max_flash_loan: 1_000_000_000,
            daily_loss_limit: -1_000_000,
            max_concurrent_tx: 1,
            pool_health_min_bps: 9_000,
            stable_depeg_cutoff_e6: 995_000,
        },
        tokens: vec![
            TokenConfig {
                symbol: "USDC".to_string(),
                address: addr(1),
                decimals: 6,
                is_stable: true,
                manual_price_usd_e8: Some(100_000_000),
            },
            TokenConfig {
                symbol: "WETH".to_string(),
                address: addr(2),
                decimals: 18,
                is_stable: false,
                manual_price_usd_e8: None,
            },
            TokenConfig {
                symbol: "DAI".to_string(),
                address: addr(3),
                decimals: 18,
                is_stable: true,
                manual_price_usd_e8: Some(100_000_000),
            },
        ],
        dexes: vec![DexConfig {
            name: "uniswap_v2".to_string(),
            amm_kind: AmmKind::UniswapV2Like,
            discovery_kind: dex_arbitrage::types::DiscoveryKind::FactoryAllPairs,
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

#[test]
fn detector_finds_profitable_cycle_touching_changed_edges() {
    let usdc = token("USDC", addr(1), 6, true);
    let weth = token("WETH", addr(2), 18, false);
    let dai = token("DAI", addr(3), 18, true);

    let p1 = pool(addr(11), usdc.address, weth.address, 1_000_000_000, 1_000_000_000_000_000_000);
    let p2 = pool(addr(12), weth.address, dai.address, 1_000_000_000_000_000_000, 1_200_000_000_000_000_000_000);
    let p3 = pool(addr(13), dai.address, usdc.address, 1_000_000_000_000_000_000_000, 1_100_000_000);

    let mut pools = HashMap::new();
    pools.insert(p1.pool_id, p1);
    pools.insert(p2.pool_id, p2);
    pools.insert(p3.pool_id, p3);

    let snapshot = GraphSnapshot::build(1, None, vec![usdc, weth, dai], pools);
    let distance_cache = DistanceCache::recompute(&snapshot);
    let detector = Detector::new(settings());

    let changed_edges = snapshot
        .adjacency
        .iter()
        .enumerate()
        .flat_map(|(from, edges)| (0..edges.len()).map(move |edge_idx| EdgeRef { from, edge_idx }))
        .collect::<Vec<_>>();

    let candidates = detector.detect(&snapshot, &changed_edges, &distance_cache);
    assert!(!candidates.is_empty());
    assert!(candidates.iter().any(|candidate| candidate.path.len() >= 2));
}
