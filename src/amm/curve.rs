use alloy::primitives::U256;

use crate::{amm::{q128_from_f64, weight_log_q32}, types::{CurvePoolState, LiquidityInfo}};

pub fn spot_rate(state: &CurvePoolState, i: usize, j: usize) -> (U256, i64, LiquidityInfo) {
    let balance_in = state.balances.get(i).copied().unwrap_or(0);
    let balance_out = state.balances.get(j).copied().unwrap_or(0);
    let amp_boost = 1.0 + (state.amp as f64 / 10_000.0).min(3.0);
    let raw = if balance_in == 0 {
        0.0
    } else {
        (balance_out as f64 / balance_in as f64).min(1.0) * amp_boost
    };
    let fee_factor = 1.0 - (state.fee as f64 / 1_000_000.0);
    let net = raw * fee_factor;

    (
        q128_from_f64(net),
        weight_log_q32(net),
        LiquidityInfo {
            estimated_usd_e8: (balance_in.min(balance_out) as u64).saturating_mul(100),
            safe_capacity_in: balance_in / 8,
        },
    )
}

pub fn fallback_quote(state: &CurvePoolState, i: usize, j: usize, amount_in: u128) -> u128 {
    let (spot_q128, _, liq) = spot_rate(state, i, j);
    let rate = spot_q128.to_string().parse::<f64>().unwrap_or(0.0) / 2f64.powi(128);
    let penalty = if liq.safe_capacity_in == 0 {
        0.4
    } else {
        (1.0 - ((amount_in as f64) / (liq.safe_capacity_in as f64)).min(0.3)).max(0.4)
    };
    (amount_in as f64 * rate * penalty) as u128
}
