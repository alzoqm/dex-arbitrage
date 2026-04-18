use alloy::primitives::U256;

use crate::{
    amm::{apply_fee_rate, q128_from_f64, weight_log_q32},
    types::{AerodromeV2PoolState, LiquidityInfo},
};

const ONE_E18: u128 = 1_000_000_000_000_000_000;

pub fn quote_exact_in(
    state: &AerodromeV2PoolState,
    zero_for_one: bool,
    amount_in: u128,
) -> Option<u128> {
    if amount_in == 0 {
        return Some(0);
    }

    let amount_in = amount_in_after_fee(amount_in, state.fee_ppm)?;
    if amount_in == 0 {
        return Some(0);
    }

    let (reserve_in, reserve_out, decimals_in, decimals_out) =
        oriented_reserves(state, zero_for_one);
    if reserve_in == 0 || reserve_out == 0 {
        return None;
    }

    if state.stable {
        stable_quote(
            amount_in,
            reserve_in,
            reserve_out,
            decimals_in,
            decimals_out,
        )
    } else {
        Some((amount_in.saturating_mul(reserve_out)) / reserve_in.saturating_add(amount_in))
    }
}

pub fn spot_rate(state: &AerodromeV2PoolState, zero_for_one: bool) -> (U256, i64, LiquidityInfo) {
    let (reserve_in, reserve_out, _, _) = oriented_reserves(state, zero_for_one);
    let raw = if reserve_in == 0 {
        0.0
    } else {
        reserve_out as f64 / reserve_in as f64
    };
    let net = apply_fee_rate(raw, state.fee_ppm);
    let capacity = reserve_in / if state.stable { 5 } else { 10 };
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

fn oriented_reserves(state: &AerodromeV2PoolState, zero_for_one: bool) -> (u128, u128, u128, u128) {
    if zero_for_one {
        (
            state.reserve0,
            state.reserve1,
            state.decimals0,
            state.decimals1,
        )
    } else {
        (
            state.reserve1,
            state.reserve0,
            state.decimals1,
            state.decimals0,
        )
    }
}

fn amount_in_after_fee(amount_in: u128, fee_ppm: u32) -> Option<u128> {
    if fee_ppm as u128 >= 1_000_000 {
        return None;
    }
    Some(amount_in.saturating_mul(1_000_000u128.saturating_sub(fee_ppm as u128)) / 1_000_000)
}

fn stable_quote(
    amount_in: u128,
    reserve_in: u128,
    reserve_out: u128,
    decimals_in: u128,
    decimals_out: u128,
) -> Option<u128> {
    if decimals_in == 0 || decimals_out == 0 {
        return None;
    }

    let reserve_in = U256::from(reserve_in);
    let reserve_out = U256::from(reserve_out);
    let amount_in = U256::from(amount_in);
    let decimals_in = U256::from(decimals_in);
    let decimals_out = U256::from(decimals_out);
    let one = U256::from(ONE_E18);

    let xy = stable_k(reserve_in, reserve_out, decimals_in, decimals_out)?;
    let reserve_in_scaled = reserve_in.checked_mul(one)?.checked_div(decimals_in)?;
    let reserve_out_scaled = reserve_out.checked_mul(one)?.checked_div(decimals_out)?;
    let amount_in_scaled = amount_in.checked_mul(one)?.checked_div(decimals_in)?;
    let y = solve_stable_y(
        reserve_in_scaled.checked_add(amount_in_scaled)?,
        xy,
        reserve_out_scaled,
    )?;
    let dy_scaled = reserve_out_scaled.checked_sub(y)?;
    u256_to_u128(dy_scaled.checked_mul(decimals_out)?.checked_div(one)?)
}

fn stable_k(x: U256, y: U256, decimals0: U256, decimals1: U256) -> Option<U256> {
    let one = U256::from(ONE_E18);
    let x = x.checked_mul(one)?.checked_div(decimals0)?;
    let y = y.checked_mul(one)?.checked_div(decimals1)?;
    stable_f(x, y)
}

fn stable_f(x0: U256, y: U256) -> Option<U256> {
    let one = U256::from(ONE_E18);
    let a = x0.checked_mul(y)?.checked_div(one)?;
    let x2 = x0.checked_mul(x0)?.checked_div(one)?;
    let y2 = y.checked_mul(y)?.checked_div(one)?;
    let b = x2.checked_add(y2)?;
    a.checked_mul(b)?.checked_div(one)
}

fn stable_d(x0: U256, y: U256) -> Option<U256> {
    let one = U256::from(ONE_E18);
    let y2 = y.checked_mul(y)?.checked_div(one)?;
    let first = U256::from(3)
        .checked_mul(x0)?
        .checked_mul(y2)?
        .checked_div(one)?;
    let x2 = x0.checked_mul(x0)?.checked_div(one)?;
    let x3 = x2.checked_mul(x0)?.checked_div(one)?;
    first.checked_add(x3)
}

fn solve_stable_y(x0: U256, xy: U256, mut y: U256) -> Option<U256> {
    let one = U256::from(ONE_E18);
    let one_unit = U256::from(1);
    for _ in 0..255 {
        let k = stable_f(x0, y)?;
        if k < xy {
            let denom = stable_d(x0, y)?;
            if denom.is_zero() {
                return None;
            }
            let mut dy = xy.checked_sub(k)?.checked_mul(one)?.checked_div(denom)?;
            if dy.is_zero() {
                if k == xy {
                    return Some(y);
                }
                if stable_f(x0, y.checked_add(one_unit)?)? > xy {
                    return y.checked_add(one_unit);
                }
                dy = one_unit;
            }
            y = y.checked_add(dy)?;
        } else {
            let denom = stable_d(x0, y)?;
            if denom.is_zero() {
                return None;
            }
            let mut dy = k.checked_sub(xy)?.checked_mul(one)?.checked_div(denom)?;
            if dy.is_zero() {
                if k == xy
                    || y.checked_sub(one_unit)
                        .and_then(|next| stable_f(x0, next))
                        .map(|v| v < xy)?
                {
                    return Some(y);
                }
                dy = one_unit;
            }
            y = y.checked_sub(dy)?;
        }
    }
    None
}

fn u256_to_u128(value: U256) -> Option<u128> {
    value.to_string().parse::<u128>().ok()
}
