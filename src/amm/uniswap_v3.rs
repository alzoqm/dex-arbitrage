use alloy::primitives::U256;

use crate::{
    amm::{apply_fee_rate, q128_from_f64, weight_log_q32},
    types::{LiquidityInfo, V3PoolState},
};

pub fn spot_rate(state: &V3PoolState, zero_for_one: bool) -> (U256, i64, LiquidityInfo) {
    let sqrt_price = u256_to_f64(state.sqrt_price_x96);
    let q96 = 2f64.powi(96);
    let price_0_to_1 = if sqrt_price <= 0.0 {
        0.0
    } else {
        (sqrt_price / q96).powi(2)
    };
    let raw = if zero_for_one {
        price_0_to_1
    } else if price_0_to_1 == 0.0 {
        0.0
    } else {
        1.0 / price_0_to_1
    };
    let net = apply_fee_rate(raw, state.fee);

    (
        q128_from_f64(net),
        weight_log_q32(net),
        LiquidityInfo {
            estimated_usd_e8: (state.liquidity.min(u128::from(u64::MAX)) as u64).saturating_mul(10),
            safe_capacity_in: state.liquidity / 50,
        },
    )
}

pub fn fallback_quote(state: &V3PoolState, zero_for_one: bool, amount_in: u128) -> u128 {
    let (_, _, liq) = spot_rate(state, zero_for_one);
    let q128 = spot_rate(state, zero_for_one).0;
    let rate = u256_to_f64(q128) / 2f64.powi(128);
    let slippage_penalty = if liq.safe_capacity_in == 0 {
        0.5
    } else {
        let ratio = (amount_in as f64) / (liq.safe_capacity_in as f64);
        (1.0 - ratio.min(0.6)).max(0.2)
    };
    (amount_in as f64 * rate * slippage_penalty) as u128
}

fn u256_to_f64(value: U256) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(0.0)
}
