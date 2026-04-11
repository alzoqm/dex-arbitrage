use std::{collections::HashMap, time::SystemTime};

use alloy::primitives::Address;
use dex_arbitrage::{
    graph::GraphSnapshot,
    types::{
        AmmKind, PoolAdmissionStatus, PoolHealth, PoolSpecificState, PoolState, TokenBehavior,
        TokenInfo, V2PoolState,
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

#[test]
fn graph_snapshot_builds_pair_and_pool_indices() {
    let usdc = TokenInfo {
        address: addr(1),
        symbol: "USDC".to_string(),
        decimals: 6,
        is_stable: true,
        is_cycle_anchor: true,
        flash_loan_enabled: true,
        allow_self_funded: true,
        behavior: TokenBehavior::default(),
        manual_price_usd_e8: Some(100_000_000),
        max_position_usd_e8: None,
        max_flash_loan_usd_e8: None,
    };
    let weth = TokenInfo {
        address: addr(2),
        symbol: "WETH".to_string(),
        decimals: 18,
        is_stable: false,
        is_cycle_anchor: false,
        flash_loan_enabled: false,
        allow_self_funded: false,
        behavior: TokenBehavior::default(),
        manual_price_usd_e8: None,
        max_position_usd_e8: None,
        max_flash_loan_usd_e8: None,
    };

    let pool = PoolState {
        pool_id: addr(10),
        dex_name: "uniswap_v2".to_string(),
        kind: AmmKind::UniswapV2Like,
        token_addresses: vec![usdc.address, weth.address],
        token_symbols: vec![usdc.symbol.clone(), weth.symbol.clone()],
        factory: None,
        registry: None,
        vault: None,
        quoter: None,
        admission_status: PoolAdmissionStatus::Allowed,
        health: health(),
        state: PoolSpecificState::UniswapV2Like(V2PoolState {
            reserve0: 2_000_000_000,
            reserve1: 1_000_000_000_000_000_000,
            fee_ppm: 3_000,
        }),
        last_updated_block: 1,
        extras: HashMap::new(),
    };

    let mut pools = HashMap::new();
    pools.insert(pool.pool_id, pool);

    let snapshot = GraphSnapshot::build(7, None, vec![usdc, weth], pools);
    assert_eq!(snapshot.snapshot_id, 7);
    assert_eq!(snapshot.adjacency.len(), 2);
    assert_eq!(snapshot.pair_edges(addr(1), addr(2)).len(), 1);
    assert_eq!(snapshot.pair_edges(addr(2), addr(1)).len(), 1);
    assert!(snapshot.pool_to_edges.contains_key(&addr(10)));
}
