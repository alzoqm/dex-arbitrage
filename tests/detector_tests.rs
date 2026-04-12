use std::{collections::HashMap, sync::Arc, time::SystemTime};

use alloy::primitives::{Address, U256};
use dex_arbitrage::{
    config::{
        ContractSettings, DexConfig, RiskSettings, RpcSettings, Settings, TokenConfig,
        UniversePolicy,
    },
    detector::Detector,
    graph::{DistanceCache, GraphSnapshot},
    types::{
        AmmKind, Chain, Edge, EdgeRef, LiquidityInfo, PoolAdmissionStatus, PoolHealth,
        PoolSpecificState, PoolState, TokenBehavior, TokenInfo, V2PoolState,
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
    token_with_anchor(symbol, address, decimals, stable, stable)
}

fn token_with_anchor(
    symbol: &str,
    address: Address,
    decimals: u8,
    stable: bool,
    is_cycle_anchor: bool,
) -> TokenInfo {
    TokenInfo {
        address,
        symbol: symbol.to_string(),
        decimals,
        is_stable: stable,
        is_cycle_anchor,
        flash_loan_enabled: is_cycle_anchor,
        allow_self_funded: stable,
        behavior: TokenBehavior::default(),
        manual_price_usd_e8: stable.then_some(100_000_000),
        max_position_usd_e8: None,
        max_flash_loan_usd_e8: None,
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
            min_net_profit_usd_e8: 10_000_000,
            min_trade_usd_e8: 1_000_000_000,
            poll_interval_ms: 400,
            event_backfill_blocks: 10,
            staleness_timeout_ms: 60_000,
            gas_risk_buffer_pct: 0.15,
            gas_price_ceiling_wei: 3_000_000_000,
            max_position: 1_000_000_000,
            max_position_usd_e8: 200_000_000_000,
            max_flash_loan: 1_000_000_000,
            max_flash_loan_usd_e8: 1_000_000_000_000,
            daily_loss_limit: -1_000_000,
            daily_loss_limit_usd_e8: 50_000_000_000,
            min_profit_realization_bps: 9000,
            max_concurrent_tx: 1,
            pool_health_min_bps: 9_000,
            stable_depeg_cutoff_e6: 995_000,
        },
        policy: UniversePolicy::default(),
        tokens: vec![
            TokenConfig {
                symbol: "USDC".to_string(),
                address: addr(1),
                decimals: 6,
                is_stable: true,
                is_cycle_anchor: true,
                flash_loan_enabled: true,
                allow_self_funded: true,
                manual_price_usd_e8: Some(100_000_000),
                max_position_usd_e8: None,
                max_flash_loan_usd_e8: None,
            },
            TokenConfig {
                symbol: "WETH".to_string(),
                address: addr(2),
                decimals: 18,
                is_stable: false,
                is_cycle_anchor: false,
                flash_loan_enabled: false,
                allow_self_funded: false,
                manual_price_usd_e8: None,
                max_position_usd_e8: None,
                max_flash_loan_usd_e8: None,
            },
            TokenConfig {
                symbol: "DAI".to_string(),
                address: addr(3),
                decimals: 18,
                is_stable: true,
                is_cycle_anchor: true,
                flash_loan_enabled: true,
                allow_self_funded: true,
                manual_price_usd_e8: Some(100_000_000),
                max_position_usd_e8: None,
                max_flash_loan_usd_e8: None,
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

    let p1 = pool(
        addr(11),
        usdc.address,
        weth.address,
        1_000_000_000,
        1_000_000_000_000_000_000,
    );
    let p2 = pool(
        addr(12),
        weth.address,
        dai.address,
        1_000_000_000_000_000_000,
        1_200_000_000_000_000_000_000,
    );
    let p3 = pool(
        addr(13),
        dai.address,
        usdc.address,
        1_000_000_000_000_000_000_000,
        1_100_000_000,
    );

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

#[test]
fn detector_can_start_from_non_stable_cycle_anchor() {
    let usdc = token("USDC", addr(1), 6, true);
    let weth = token_with_anchor("WETH", addr(2), 18, false, true);
    let dai = token("DAI", addr(3), 18, true);

    let p1 = pool(
        addr(11),
        usdc.address,
        weth.address,
        1_000_000_000,
        1_000_000_000_000_000_000,
    );
    let p2 = pool(
        addr(12),
        weth.address,
        dai.address,
        1_000_000_000_000_000_000,
        1_200_000_000_000_000_000_000,
    );
    let p3 = pool(
        addr(13),
        dai.address,
        usdc.address,
        1_000_000_000_000_000_000_000,
        1_100_000_000,
    );

    let mut pools = HashMap::new();
    pools.insert(p1.pool_id, p1);
    pools.insert(p2.pool_id, p2);
    pools.insert(p3.pool_id, p3);

    let snapshot = GraphSnapshot::build(1, None, vec![usdc, weth.clone(), dai], pools);
    let distance_cache = DistanceCache::recompute(&snapshot);
    let detector = Detector::new(settings());

    let changed_edges = snapshot
        .adjacency
        .iter()
        .enumerate()
        .flat_map(|(from, edges)| (0..edges.len()).map(move |edge_idx| EdgeRef { from, edge_idx }))
        .collect::<Vec<_>>();

    let candidates = detector.detect(&snapshot, &changed_edges, &distance_cache);
    assert!(candidates
        .iter()
        .any(|candidate| candidate.start_token == weth.address));
}

#[test]
fn detector_deduplicates_parallel_pools_by_token_cycle() {
    let (snapshot, changed_edges) = manual_parallel_snapshot(4);
    let distance_cache = DistanceCache::recompute(&snapshot);
    let detector = Detector::new(settings());

    let candidates = detector.detect(&snapshot, &changed_edges, &distance_cache);

    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].path.len(), 4);
}

fn manual_parallel_snapshot(parallel_pools: usize) -> (GraphSnapshot, Vec<EdgeRef>) {
    let tokens = vec![
        token_with_anchor("A", addr(50), 18, false, true),
        token("X", addr(51), 18, false),
        token("Y", addr(52), 18, false),
        token("Z", addr(53), 18, false),
    ];
    let mut adjacency = vec![Vec::new(); tokens.len()];
    let mut reverse_adj = vec![Vec::new(); tokens.len()];
    let mut pair_to_edges = HashMap::new();
    let mut pool_to_edges = HashMap::new();
    let mut pool_nonce = 200u8;

    for _ in 0..parallel_pools {
        add_manual_edge(
            &mut adjacency,
            &mut reverse_adj,
            &mut pair_to_edges,
            &mut pool_to_edges,
            &mut pool_nonce,
            0,
            1,
        );
        add_manual_edge(
            &mut adjacency,
            &mut reverse_adj,
            &mut pair_to_edges,
            &mut pool_to_edges,
            &mut pool_nonce,
            1,
            2,
        );
        add_manual_edge(
            &mut adjacency,
            &mut reverse_adj,
            &mut pair_to_edges,
            &mut pool_to_edges,
            &mut pool_nonce,
            2,
            3,
        );
        add_manual_edge(
            &mut adjacency,
            &mut reverse_adj,
            &mut pair_to_edges,
            &mut pool_to_edges,
            &mut pool_nonce,
            3,
            0,
        );
    }

    let changed_edges = adjacency
        .iter()
        .enumerate()
        .flat_map(|(from, edges)| (0..edges.len()).map(move |edge_idx| EdgeRef { from, edge_idx }))
        .collect::<Vec<_>>();
    let token_to_index = tokens
        .iter()
        .enumerate()
        .map(|(idx, token)| (token.address, idx))
        .collect();

    (
        GraphSnapshot {
            snapshot_id: 1,
            block_ref: None,
            tokens,
            token_to_index,
            stable_token_indices: Vec::new(),
            cycle_anchor_indices: vec![0],
            adjacency,
            reverse_adj,
            pools: HashMap::new(),
            pool_to_edges,
            pair_to_edges,
        },
        changed_edges,
    )
}

#[allow(clippy::too_many_arguments)]
fn add_manual_edge(
    adjacency: &mut [Vec<Edge>],
    reverse_adj: &mut [Vec<EdgeRef>],
    pair_to_edges: &mut HashMap<(usize, usize), smallvec::SmallVec<[EdgeRef; 8]>>,
    pool_to_edges: &mut HashMap<Address, smallvec::SmallVec<[EdgeRef; 8]>>,
    pool_nonce: &mut u8,
    from: usize,
    to: usize,
) -> EdgeRef {
    let pool_id = addr(*pool_nonce);
    *pool_nonce = pool_nonce.saturating_add(1);
    adjacency[from].push(Edge {
        from,
        to,
        pool_id,
        amm_kind: AmmKind::UniswapV2Like,
        fee_ppm: 3_000,
        weight_log_q32: -1_000_000,
        spot_rate_q128: U256::from(1u8),
        liquidity: LiquidityInfo {
            estimated_usd_e8: 100_000_000,
            safe_capacity_in: 1_000_000,
        },
        pool_health: health(),
        dex_name: "manual".to_string(),
    });
    let edge_ref = EdgeRef {
        from,
        edge_idx: adjacency[from].len() - 1,
    };
    reverse_adj[to].push(edge_ref);
    pair_to_edges.entry((from, to)).or_default().push(edge_ref);
    pool_to_edges.entry(pool_id).or_default().push(edge_ref);
    edge_ref
}
