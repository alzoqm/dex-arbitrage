use alloy::primitives::U256;

use crate::{
    amm::{q128_from_f64, weight_log_q32},
    types::{LiquidityInfo, TraderJoeLbPoolState},
};

pub fn quote_exact_in(
    state: &TraderJoeLbPoolState,
    swap_for_y: bool,
    amount_in: u128,
) -> Option<u128> {
    if amount_in == 0 {
        return Some(0);
    }
    let (reserve_in, reserve_out) = if swap_for_y {
        (state.reserve_x, state.reserve_y)
    } else {
        (state.reserve_y, state.reserve_x)
    };
    if reserve_in == 0 || reserve_out == 0 {
        return None;
    }
    let amount_in_with_fee = amount_in.saturating_mul(997_000);
    let numerator = amount_in_with_fee.saturating_mul(reserve_out);
    let denominator = reserve_in
        .saturating_mul(1_000_000)
        .saturating_add(amount_in_with_fee);
    Some(numerator / denominator)
}

pub fn spot_rate(state: &TraderJoeLbPoolState, swap_for_y: bool) -> (U256, i64, LiquidityInfo) {
    let active_price = active_bin_price(state).unwrap_or_else(|| {
        if state.reserve_x == 0 {
            0.0
        } else {
            state.reserve_y as f64 / state.reserve_x as f64
        }
    });
    let raw = if swap_for_y {
        active_price
    } else if active_price > 0.0 {
        1.0 / active_price
    } else {
        0.0
    };
    let (reserve_in, reserve_out) = if swap_for_y {
        (state.reserve_x, state.reserve_y)
    } else {
        (state.reserve_y, state.reserve_x)
    };
    let capacity = reserve_in / 10;
    let estimated_usd_e8 = (reserve_out.min(reserve_in) as u64).saturating_mul(100);
    (
        q128_from_f64(raw),
        weight_log_q32(raw),
        LiquidityInfo {
            estimated_usd_e8,
            safe_capacity_in: capacity,
        },
    )
}

fn active_bin_price(state: &TraderJoeLbPoolState) -> Option<f64> {
    if state.active_id == 0 {
        return None;
    }
    let exponent = state.active_id as i64 - 8_388_608_i64;
    let base = 1.0 + (state.bin_step as f64 / 10_000.0);
    let price = base.powi(exponent as i32);
    price.is_finite().then_some(price)
}
