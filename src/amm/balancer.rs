use alloy::primitives::U256;

use crate::{amm::{q128_from_f64, weight_log_q32}, types::{BalancerPoolState, LiquidityInfo}};

pub fn spot_rate(state: &BalancerPoolState, i: usize, j: usize) -> (U256, i64, LiquidityInfo) {
    let bi = state.balances.get(i).copied().unwrap_or(0) as f64;
    let bj = state.balances.get(j).copied().unwrap_or(0) as f64;
    let wi = state.normalized_weights_1e18.get(i).copied().unwrap_or(1) as f64 / 1e18;
    let wj = state.normalized_weights_1e18.get(j).copied().unwrap_or(1) as f64 / 1e18;
    let raw = if bi <= 0.0 || bj <= 0.0 || wi <= 0.0 || wj <= 0.0 {
        0.0
    } else {
        (bi / wi) / (bj / wj)
    };
    let fee_factor = 1.0 - (state.swap_fee_ppm as f64 / 1_000_000.0);
    let net = if raw == 0.0 { 0.0 } else { (1.0 / raw) * fee_factor };

    (
        q128_from_f64(net),
        weight_log_q32(net),
        LiquidityInfo {
            estimated_usd_e8: state
                .balances
                .iter()
                .min()
                .copied()
                .unwrap_or(0)
                .min(u128::from(u64::MAX)) as u64 * 100,
            safe_capacity_in: state.balances.get(i).copied().unwrap_or(0) / 10,
        },
    )
}

pub fn fallback_quote(state: &BalancerPoolState, i: usize, j: usize, amount_in: u128) -> u128 {
    let bi = state.balances.get(i).copied().unwrap_or(0) as f64;
    let bo = state.balances.get(j).copied().unwrap_or(0) as f64;
    let wi = state.normalized_weights_1e18.get(i).copied().unwrap_or(1) as f64 / 1e18;
    let wo = state.normalized_weights_1e18.get(j).copied().unwrap_or(1) as f64 / 1e18;
    let fee_factor = 1.0 - (state.swap_fee_ppm as f64 / 1_000_000.0);
    let ai = amount_in as f64 * fee_factor;

    if bi <= 0.0 || bo <= 0.0 || wi <= 0.0 || wo <= 0.0 {
        return 0;
    }

    let y = bi / (bi + ai);
    let power = wi / wo;
    let factor = 1.0 - y.powf(power);
    (bo * factor.max(0.0)) as u128
}
