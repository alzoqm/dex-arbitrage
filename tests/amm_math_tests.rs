use dex_arbitrage::{
    amm::{balancer, curve, uniswap_v2, uniswap_v3},
    types::{BalancerPoolState, CurvePoolState, V2PoolState, V3PoolState},
};

#[test]
fn uniswap_v2_exact_in_quote_is_positive_and_bounded() {
    let state = V2PoolState {
        reserve0: 1_000_000_000,
        reserve1: 500_000_000,
        fee_ppm: 3_000,
    };

    let out = uniswap_v2::quote_exact_in(&state, true, 10_000_000).unwrap();
    assert!(out > 0);
    assert!(out < state.reserve1);
}

#[test]
fn uniswap_v2_quote_handles_zero_input_and_empty_reserves() {
    let empty = V2PoolState {
        reserve0: 0,
        reserve1: 500_000_000,
        fee_ppm: 3_000,
    };
    let funded = V2PoolState {
        reserve0: 1_000_000_000,
        reserve1: 500_000_000,
        fee_ppm: 3_000,
    };

    assert_eq!(uniswap_v2::quote_exact_in(&funded, true, 0), Some(0));
    assert_eq!(uniswap_v2::quote_exact_in(&empty, true, 1_000), None);
}

#[test]
fn uniswap_v2_quote_decreases_when_fee_increases() {
    let low_fee = V2PoolState {
        reserve0: 1_000_000_000,
        reserve1: 1_000_000_000,
        fee_ppm: 500,
    };
    let high_fee = V2PoolState {
        fee_ppm: 10_000,
        ..low_fee
    };

    let low_fee_out = uniswap_v2::quote_exact_in(&low_fee, true, 10_000_000).unwrap();
    let high_fee_out = uniswap_v2::quote_exact_in(&high_fee, true, 10_000_000).unwrap();

    assert!(low_fee_out > high_fee_out);
}

#[test]
fn uniswap_v3_fallback_quote_is_conservative() {
    let state = V3PoolState {
        sqrt_price_x96: alloy::primitives::U256::from(1u128 << 96),
        liquidity: 1_000_000_000_000,
        tick: 0,
        fee: 500,
        tick_spacing: 10,
    };

    let out = uniswap_v3::fallback_quote(&state, true, 1_000_000);
    assert!(out > 0);
    assert!(out <= 1_000_000);
}

#[test]
fn curve_fallback_quote_is_positive_for_balanced_pool() {
    let state = CurvePoolState {
        balances: vec![1_000_000_000, 1_005_000_000],
        amp: 2_000,
        fee: 40,
        supports_underlying: true,
    };

    let out = curve::fallback_quote(&state, 0, 1, 1_000_000);
    assert!(out > 0);
}

#[test]
fn curve_fallback_quote_returns_zero_for_invalid_indices() {
    let state = CurvePoolState {
        balances: vec![1_000_000_000, 1_005_000_000],
        amp: 2_000,
        fee: 40,
        supports_underlying: false,
    };

    assert_eq!(curve::fallback_quote(&state, 0, 9, 1_000_000), 0);
    assert_eq!(curve::fallback_quote(&state, 9, 0, 1_000_000), 0);
}

#[test]
fn balancer_fallback_quote_is_positive_for_weighted_pool() {
    let state = BalancerPoolState {
        pool_id: alloy::primitives::B256::ZERO,
        balances: vec![1_000_000_000_000, 2_000_000_000_000],
        normalized_weights_1e18: vec![500_000_000_000_000_000, 500_000_000_000_000_000],
        swap_fee_ppm: 3_000,
    };

    let out = balancer::fallback_quote(&state, 0, 1, 10_000_000);
    assert!(out > 0);
}

#[test]
fn balancer_fallback_quote_returns_zero_for_unusable_pool_state() {
    let state = BalancerPoolState {
        pool_id: alloy::primitives::B256::ZERO,
        balances: vec![0, 2_000_000_000_000],
        normalized_weights_1e18: vec![500_000_000_000_000_000, 500_000_000_000_000_000],
        swap_fee_ppm: 3_000,
    };

    assert_eq!(balancer::fallback_quote(&state, 0, 1, 10_000_000), 0);
}
