use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Instant, SystemTime},
};

use alloy::primitives::{Address, U256};
use dex_arbitrage::{
    config::{
        ContractSettings, ExecutionSettings, RiskSettings, RpcSettings, Settings, TokenConfig,
        UniversePolicy,
    },
    detector::{pruning, Detector},
    graph::{DistanceCache, GraphSnapshot},
    types::{AmmKind, Chain, Edge, EdgeRef, LiquidityInfo, PoolHealth, TokenBehavior, TokenInfo},
};

fn main() {
    if std::env::args().any(|arg| arg == "--polygon-scale") {
        run_polygon_scale();
        return;
    }

    let scenario = build_parallel_pool_scenario(8, 4);
    let detector = Detector::new(settings(4, 3));
    let distance_cache = DistanceCache::recompute(&scenario.snapshot);

    run_case(
        "hot_path_one_changed_pool_edge",
        &scenario.snapshot,
        &scenario.hot_changed_edges,
        &distance_cache,
        detector,
        4,
    );

    let detector = Detector::new(settings(4, 3));
    run_case(
        "cold_start_all_edges_changed",
        &scenario.snapshot,
        &scenario.all_edges,
        &distance_cache,
        detector,
        4,
    );
}

fn run_polygon_scale() {
    let scenario = build_polygon_scale_scenario(PolygonScaleConfig {
        anchors: 21,
        tokens: 8_000,
        undirected_pools: 144_068,
        hot_changed_pairs: 24,
        seed: 0x5eed_2026,
    });
    let legacy_visit_limit = std::env::var("LEGACY_VISIT_LIMIT")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(100_000);
    let detector = Detector::new(settings(5, 3));
    let distance_cache = DistanceCache::recompute(&scenario.snapshot);

    run_limited_case(
        "polygon_scale_hot_path_24_changed_pairs",
        &scenario.snapshot,
        &scenario.hot_changed_edges,
        &distance_cache,
        detector,
        5,
        legacy_visit_limit,
    );

    let detector = Detector::new(settings(5, 3));
    let started = Instant::now();
    let new_candidates = detector.detect(&scenario.snapshot, &scenario.all_edges, &distance_cache);
    println!(
        "polygon_scale_cold_start_new_only: changed_edges={} anchors={} tokens={} directed_edges={} candidates={} elapsed_ms={}",
        scenario.all_edges.len(),
        scenario.snapshot.cycle_anchor_indices.len(),
        scenario.snapshot.tokens.len(),
        scenario.snapshot.adjacency.iter().map(Vec::len).sum::<usize>(),
        new_candidates.len(),
        started.elapsed().as_millis()
    );
}

fn run_case(
    name: &str,
    snapshot: &GraphSnapshot,
    changed_edges: &[EdgeRef],
    distance_cache: &DistanceCache,
    detector: Detector,
    max_hops: usize,
) {
    let started = Instant::now();
    let legacy = legacy_detect(snapshot, changed_edges, distance_cache, max_hops);
    let legacy_elapsed = started.elapsed();

    let started = Instant::now();
    let new_candidates = detector.detect(snapshot, changed_edges, distance_cache);
    let new_elapsed = started.elapsed();

    let new_unique_token_cycles = new_candidates
        .iter()
        .map(|candidate| candidate.cycle_key.clone())
        .collect::<HashSet<_>>()
        .len();

    println!(
        "{name}: changed_edges={} anchors={} tokens={} directed_edges={}",
        changed_edges.len(),
        snapshot.cycle_anchor_indices.len(),
        snapshot.tokens.len(),
        snapshot.adjacency.iter().map(Vec::len).sum::<usize>()
    );
    println!(
        "  legacy_pool_path_dfs: candidates={} unique_token_cycles={} edge_visits={} elapsed_us={}",
        legacy.candidates,
        legacy.unique_token_cycles,
        legacy.edge_visits,
        legacy_elapsed.as_micros()
    );
    println!(
        "  changed_edge_topk: candidates={} unique_token_cycles={} elapsed_us={}",
        new_candidates.len(),
        new_unique_token_cycles,
        new_elapsed.as_micros()
    );
    if !new_candidates.is_empty() {
        println!(
            "  reduction: candidates={:.2}x edge_visit_proxy={:.2}x",
            legacy.candidates as f64 / new_candidates.len() as f64,
            legacy.edge_visits as f64 / new_candidates.len() as f64
        );
    }
}

struct PolygonScaleConfig {
    anchors: usize,
    tokens: usize,
    undirected_pools: usize,
    hot_changed_pairs: usize,
    seed: u64,
}

fn build_polygon_scale_scenario(config: PolygonScaleConfig) -> Scenario {
    assert!(config.tokens > config.anchors + 4);
    assert!(config.undirected_pools > config.hot_changed_pairs * 4);

    let hub_a = config.anchors;
    let hub_b = config.anchors + 1;
    let hub_c = config.anchors + 2;

    let mut tokens = Vec::with_capacity(config.tokens);
    for idx in 0..config.anchors {
        tokens.push(token(&format!("ANCHOR{idx}"), idx, true));
    }
    for idx in config.anchors..config.tokens {
        tokens.push(token(&format!("T{idx}"), idx, false));
    }

    let mut adjacency = vec![Vec::new(); tokens.len()];
    let mut reverse_adj = vec![Vec::new(); tokens.len()];
    let mut pair_to_edges = HashMap::new();
    let mut pool_to_edges = HashMap::new();
    let mut edge_nonce = 10_000_000u64;
    let mut hot_changed_edges = Vec::new();
    let mut undirected_used = 0usize;

    for anchor in 0..config.anchors {
        add_undirected_pool(
            &mut adjacency,
            &mut reverse_adj,
            &mut pair_to_edges,
            &mut pool_to_edges,
            &mut edge_nonce,
            anchor,
            hub_a,
        );
        add_undirected_pool(
            &mut adjacency,
            &mut reverse_adj,
            &mut pair_to_edges,
            &mut pool_to_edges,
            &mut edge_nonce,
            hub_c,
            anchor,
        );
        undirected_used += 2;
    }

    for idx in 0..config.hot_changed_pairs {
        let side = config.anchors + 3 + idx;
        add_undirected_pool(
            &mut adjacency,
            &mut reverse_adj,
            &mut pair_to_edges,
            &mut pool_to_edges,
            &mut edge_nonce,
            hub_a,
            side,
        );
        let changed = add_directed_edge(
            &mut adjacency,
            &mut reverse_adj,
            &mut pair_to_edges,
            &mut pool_to_edges,
            &mut edge_nonce,
            side,
            hub_b,
        );
        add_directed_edge(
            &mut adjacency,
            &mut reverse_adj,
            &mut pair_to_edges,
            &mut pool_to_edges,
            &mut edge_nonce,
            hub_b,
            side,
        );
        add_undirected_pool(
            &mut adjacency,
            &mut reverse_adj,
            &mut pair_to_edges,
            &mut pool_to_edges,
            &mut edge_nonce,
            hub_b,
            hub_c,
        );
        hot_changed_edges.push(changed);
        undirected_used += 3;
    }

    let mut rng = XorShift64::new(config.seed);
    while undirected_used < config.undirected_pools {
        let from = rng.next_usize(config.tokens);
        let mut to = rng.next_usize(config.tokens);
        if from == to {
            to = (to + 1) % config.tokens;
        }
        add_undirected_pool(
            &mut adjacency,
            &mut reverse_adj,
            &mut pair_to_edges,
            &mut pool_to_edges,
            &mut edge_nonce,
            from,
            to,
        );
        undirected_used += 1;
    }

    let all_edges = adjacency
        .iter()
        .enumerate()
        .flat_map(|(from, edges)| (0..edges.len()).map(move |edge_idx| EdgeRef { from, edge_idx }))
        .collect::<Vec<_>>();

    let snapshot = GraphSnapshot {
        snapshot_id: 1,
        block_ref: None,
        stable_token_indices: Vec::new(),
        cycle_anchor_indices: (0..config.anchors).collect(),
        token_to_index: tokens
            .iter()
            .enumerate()
            .map(|(idx, token)| (token.address, idx))
            .collect(),
        tokens,
        adjacency,
        reverse_adj,
        pools: HashMap::new(),
        pool_to_edges,
        pair_to_edges,
    };

    Scenario {
        snapshot,
        hot_changed_edges,
        all_edges,
    }
}

#[derive(Debug)]
struct LegacyStats {
    candidates: usize,
    unique_token_cycles: usize,
    edge_visits: usize,
    hit_limit: bool,
}

fn legacy_detect(
    snapshot: &GraphSnapshot,
    changed_edges: &[EdgeRef],
    distance_cache: &DistanceCache,
    max_hops: usize,
) -> LegacyStats {
    let changed = changed_edges.iter().copied().collect::<HashSet<_>>();
    let mut stats = LegacyStats {
        candidates: 0,
        unique_token_cycles: 0,
        edge_visits: 0,
        hit_limit: false,
    };
    let mut pool_dedup = HashSet::new();
    let mut token_dedup = HashSet::new();
    let mut path = Vec::new();

    for &anchor in &snapshot.cycle_anchor_indices {
        legacy_dfs(
            snapshot,
            distance_cache,
            &changed,
            max_hops,
            anchor,
            anchor,
            0,
            false,
            &mut path,
            &mut pool_dedup,
            &mut token_dedup,
            &mut stats,
            usize::MAX,
        );
    }

    stats.unique_token_cycles = token_dedup.len();
    stats
}

fn run_limited_case(
    name: &str,
    snapshot: &GraphSnapshot,
    changed_edges: &[EdgeRef],
    distance_cache: &DistanceCache,
    detector: Detector,
    max_hops: usize,
    legacy_edge_visit_limit: usize,
) {
    let started = Instant::now();
    let new_candidates = detector.detect(snapshot, changed_edges, distance_cache);
    let new_elapsed = started.elapsed();

    let new_unique_token_cycles = new_candidates
        .iter()
        .map(|candidate| candidate.cycle_key.clone())
        .collect::<HashSet<_>>()
        .len();

    println!(
        "{name}: changed_edges={} anchors={} tokens={} directed_edges={} legacy_limit={}",
        changed_edges.len(),
        snapshot.cycle_anchor_indices.len(),
        snapshot.tokens.len(),
        snapshot.adjacency.iter().map(Vec::len).sum::<usize>(),
        legacy_edge_visit_limit
    );
    println!(
        "  changed_edge_topk: candidates={} unique_token_cycles={} elapsed_ms={}",
        new_candidates.len(),
        new_unique_token_cycles,
        new_elapsed.as_millis()
    );
    let started = Instant::now();
    let legacy = legacy_detect_with_limit(
        snapshot,
        changed_edges,
        distance_cache,
        max_hops,
        legacy_edge_visit_limit,
    );
    let legacy_elapsed = started.elapsed();
    println!(
        "  legacy_pool_path_dfs_limited: candidates={} unique_token_cycles={} edge_visits={} hit_limit={} elapsed_ms={}",
        legacy.candidates,
        legacy.unique_token_cycles,
        legacy.edge_visits,
        legacy.hit_limit,
        legacy_elapsed.as_millis()
    );
    if !new_candidates.is_empty() {
        println!(
            "  reduction_before_legacy_limit: candidates={:.2}x edge_visit_proxy={:.2}x",
            legacy.candidates as f64 / new_candidates.len() as f64,
            legacy.edge_visits as f64 / new_candidates.len() as f64
        );
    }
}

fn legacy_detect_with_limit(
    snapshot: &GraphSnapshot,
    changed_edges: &[EdgeRef],
    distance_cache: &DistanceCache,
    max_hops: usize,
    edge_visit_limit: usize,
) -> LegacyStats {
    let changed = changed_edges.iter().copied().collect::<HashSet<_>>();
    let mut stats = LegacyStats {
        candidates: 0,
        unique_token_cycles: 0,
        edge_visits: 0,
        hit_limit: false,
    };
    let mut pool_dedup = HashSet::new();
    let mut token_dedup = HashSet::new();
    let mut path = Vec::new();

    for &anchor in &snapshot.cycle_anchor_indices {
        legacy_dfs(
            snapshot,
            distance_cache,
            &changed,
            max_hops,
            anchor,
            anchor,
            0,
            false,
            &mut path,
            &mut pool_dedup,
            &mut token_dedup,
            &mut stats,
            edge_visit_limit,
        );
        if stats.hit_limit {
            break;
        }
    }

    stats.unique_token_cycles = token_dedup.len();
    stats
}

#[allow(clippy::too_many_arguments)]
fn legacy_dfs(
    snapshot: &GraphSnapshot,
    distance_cache: &DistanceCache,
    changed: &HashSet<EdgeRef>,
    max_hops: usize,
    start: usize,
    current: usize,
    depth: usize,
    touched_changed: bool,
    path: &mut Vec<EdgeRef>,
    pool_dedup: &mut HashSet<String>,
    token_dedup: &mut HashSet<String>,
    stats: &mut LegacyStats,
    edge_visit_limit: usize,
) {
    if stats.edge_visits >= edge_visit_limit {
        stats.hit_limit = true;
        return;
    }
    if depth >= max_hops {
        return;
    }

    for edge_ref in pruning::ranked_outgoing(snapshot, current) {
        if stats.edge_visits >= edge_visit_limit {
            stats.hit_limit = true;
            return;
        }
        stats.edge_visits += 1;
        let Some(edge) = snapshot.edge(edge_ref) else {
            continue;
        };
        if !distance_cache.reachable_to_anchor[edge.to] {
            continue;
        }

        let is_cycle = edge.to == start && depth >= 1;
        let next_touched_changed = touched_changed || changed.contains(&edge_ref);
        path.push(edge_ref);

        if is_cycle && next_touched_changed {
            let pool_key = path
                .iter()
                .filter_map(|edge_ref| snapshot.edge(*edge_ref))
                .map(|edge| format!("{}:{}:{}", edge.from, edge.to, edge.pool_id))
                .collect::<Vec<_>>()
                .join("|");
            if pool_dedup.insert(pool_key) {
                stats.candidates += 1;
                token_dedup.insert(token_key(snapshot, path));
            }
        }

        if !is_cycle {
            legacy_dfs(
                snapshot,
                distance_cache,
                changed,
                max_hops,
                start,
                edge.to,
                depth + 1,
                next_touched_changed,
                path,
                pool_dedup,
                token_dedup,
                stats,
                edge_visit_limit,
            );
        }

        path.pop();
    }
}

fn token_key(snapshot: &GraphSnapshot, path: &[EdgeRef]) -> String {
    let mut tokens = Vec::new();
    if let Some(first) = path.first().and_then(|edge_ref| snapshot.edge(*edge_ref)) {
        tokens.push(snapshot.tokens[first.from].address.to_string());
    }
    for edge_ref in path {
        if let Some(edge) = snapshot.edge(*edge_ref) {
            tokens.push(snapshot.tokens[edge.to].address.to_string());
        }
    }
    tokens.join(">")
}

struct Scenario {
    snapshot: GraphSnapshot,
    hot_changed_edges: Vec<EdgeRef>,
    all_edges: Vec<EdgeRef>,
}

fn build_parallel_pool_scenario(anchor_count: usize, parallel_pools: usize) -> Scenario {
    let x = anchor_count;
    let y = anchor_count + 1;
    let z = anchor_count + 2;
    let mut tokens = Vec::new();
    for idx in 0..anchor_count {
        tokens.push(token(&format!("A{idx}"), idx, true));
    }
    tokens.push(token("X", x, false));
    tokens.push(token("Y", y, false));
    tokens.push(token("Z", z, false));

    let mut adjacency = vec![Vec::new(); tokens.len()];
    let mut reverse_adj = vec![Vec::new(); tokens.len()];
    let mut pair_to_edges = HashMap::new();
    let mut pool_to_edges = HashMap::new();
    let mut edge_nonce = 1u64;
    let mut hot_changed_edges = Vec::new();

    for anchor in 0..anchor_count {
        for _ in 0..parallel_pools {
            add_edge(
                &mut adjacency,
                &mut reverse_adj,
                &mut pair_to_edges,
                &mut pool_to_edges,
                &mut edge_nonce,
                anchor,
                x,
            );
            add_edge(
                &mut adjacency,
                &mut reverse_adj,
                &mut pair_to_edges,
                &mut pool_to_edges,
                &mut edge_nonce,
                z,
                anchor,
            );
        }
    }

    for pool_idx in 0..parallel_pools {
        let xy = add_edge(
            &mut adjacency,
            &mut reverse_adj,
            &mut pair_to_edges,
            &mut pool_to_edges,
            &mut edge_nonce,
            x,
            y,
        );
        if pool_idx == 0 {
            hot_changed_edges.push(xy);
        }
        add_edge(
            &mut adjacency,
            &mut reverse_adj,
            &mut pair_to_edges,
            &mut pool_to_edges,
            &mut edge_nonce,
            y,
            z,
        );
    }

    let all_edges = adjacency
        .iter()
        .enumerate()
        .flat_map(|(from, edges)| (0..edges.len()).map(move |edge_idx| EdgeRef { from, edge_idx }))
        .collect::<Vec<_>>();

    let snapshot = GraphSnapshot {
        snapshot_id: 1,
        block_ref: None,
        stable_token_indices: Vec::new(),
        cycle_anchor_indices: (0..anchor_count).collect(),
        token_to_index: tokens
            .iter()
            .enumerate()
            .map(|(idx, token)| (token.address, idx))
            .collect(),
        tokens,
        adjacency,
        reverse_adj,
        pools: HashMap::new(),
        pool_to_edges,
        pair_to_edges,
    };

    Scenario {
        snapshot,
        hot_changed_edges,
        all_edges,
    }
}

#[allow(clippy::too_many_arguments)]
fn add_undirected_pool(
    adjacency: &mut [Vec<Edge>],
    reverse_adj: &mut [Vec<EdgeRef>],
    pair_to_edges: &mut HashMap<(usize, usize), smallvec::SmallVec<[EdgeRef; 8]>>,
    pool_to_edges: &mut HashMap<Address, smallvec::SmallVec<[EdgeRef; 8]>>,
    edge_nonce: &mut u64,
    a: usize,
    b: usize,
) {
    add_directed_edge(
        adjacency,
        reverse_adj,
        pair_to_edges,
        pool_to_edges,
        edge_nonce,
        a,
        b,
    );
    add_directed_edge(
        adjacency,
        reverse_adj,
        pair_to_edges,
        pool_to_edges,
        edge_nonce,
        b,
        a,
    );
}

#[allow(clippy::too_many_arguments)]
fn add_edge(
    adjacency: &mut [Vec<Edge>],
    reverse_adj: &mut [Vec<EdgeRef>],
    pair_to_edges: &mut HashMap<(usize, usize), smallvec::SmallVec<[EdgeRef; 8]>>,
    pool_to_edges: &mut HashMap<Address, smallvec::SmallVec<[EdgeRef; 8]>>,
    edge_nonce: &mut u64,
    from: usize,
    to: usize,
) -> EdgeRef {
    add_directed_edge(
        adjacency,
        reverse_adj,
        pair_to_edges,
        pool_to_edges,
        edge_nonce,
        from,
        to,
    )
}

#[allow(clippy::too_many_arguments)]
fn add_directed_edge(
    adjacency: &mut [Vec<Edge>],
    reverse_adj: &mut [Vec<EdgeRef>],
    pair_to_edges: &mut HashMap<(usize, usize), smallvec::SmallVec<[EdgeRef; 8]>>,
    pool_to_edges: &mut HashMap<Address, smallvec::SmallVec<[EdgeRef; 8]>>,
    edge_nonce: &mut u64,
    from: usize,
    to: usize,
) -> EdgeRef {
    let pool_id = addr_from_u64(*edge_nonce);
    *edge_nonce += 1;
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
        dex_name: "synthetic_v2".to_string(),
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

struct XorShift64 {
    state: u64,
}

impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self { state: seed.max(1) }
    }

    fn next(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    fn next_usize(&mut self, upper: usize) -> usize {
        (self.next() as usize) % upper
    }
}

fn token(symbol: &str, idx: usize, anchor: bool) -> TokenInfo {
    TokenInfo {
        address: addr_from_u64(idx as u64 + 1_000),
        symbol: symbol.to_string(),
        decimals: 18,
        is_stable: false,
        is_cycle_anchor: anchor,
        flash_loan_enabled: anchor,
        allow_self_funded: false,
        behavior: TokenBehavior::default(),
        manual_price_usd_e8: Some(100_000_000),
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

fn settings(max_hops: usize, screening_margin_bps: u32) -> Arc<Settings> {
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
            executor_address: Some(addr_from_u64(99)),
            aave_pool: Some(addr_from_u64(100)),
            strict_target_allowlist: false,
        },
        risk: RiskSettings {
            max_hops,
            screening_margin_bps,
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
        search: dex_arbitrage::config::SearchSettings::default(),
        execution: ExecutionSettings::default(),
        policy: UniversePolicy::default(),
        tokens: Vec::<TokenConfig>::new(),
        dexes: Vec::new(),
    })
}

fn addr_from_u64(value: u64) -> Address {
    let mut bytes = [0u8; 20];
    bytes[12..20].copy_from_slice(&value.to_be_bytes());
    Address::from_slice(&bytes)
}
