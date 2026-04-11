use alloy::primitives::U256;

use crate::{amm::{apply_fee_rate, q128_from_f64, weight_log_q32}, types::{LiquidityInfo, V2PoolState}};

pub fn quote_exact_in(state: &V2PoolState, zero_for_one: bool, amount_in: u128) -> Option<u128> {
    if amount_in == 0 {
        return Some(0);
    }

    let (reserve_in, reserve_out) = if zero_for_one {
        (state.reserve0, state.reserve1)
    } else {
        (state.reserve1, state.reserve0)
    };

    if reserve_in == 0 || reserve_out == 0 {
        return None;
    }

    let fee_factor = 1_000_000u128.saturating_sub(state.fee_ppm as u128);
    let amount_in_with_fee = amount_in.saturating_mul(fee_factor);
    let numerator = amount_in_with_fee.saturating_mul(reserve_out);
    let denominator = reserve_in
        .saturating_mul(1_000_000)
        .saturating_add(amount_in_with_fee);
    Some(numerator / denominator)
}

pub fn spot_rate(state: &V2PoolState, zero_for_one: bool) -> (U256, i64, LiquidityInfo) {
    let (reserve_in, reserve_out) = if zero_for_one {
        (state.reserve0, state.reserve1)
    } else {
        (state.reserve1, state.reserve0)
    };

    let raw = if reserve_in == 0 { 0.0 } else { reserve_out as f64 / reserve_in as f64 };
    let net = apply_fee_rate(raw, state.fee_ppm);
    let capacity = reserve_in / 10;
    let estimated_usd_e8 = (reserve_out.min(reserve_in) as u64).saturating_mul(100);

    (
        q128_from_f64(net),
        weight_log_q32(net),
        LiquidityInfo {
            estimated_usd_e8,
            safe_capacity_in: capacity,
        },
    )
}
