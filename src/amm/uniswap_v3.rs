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

    let safe_capacity_in = price_limit_capacity_in(state, zero_for_one)
        .map(|capacity| capacity.min(state.liquidity / 50))
        .unwrap_or(state.liquidity / 50);

    (
        q128_from_f64(net),
        weight_log_q32(net),
        LiquidityInfo {
            estimated_usd_e8: (state.liquidity.min(u128::from(u64::MAX)) as u64).saturating_mul(10),
            safe_capacity_in,
        },
    )
}

pub fn fallback_quote(state: &V3PoolState, zero_for_one: bool, amount_in: u128) -> u128 {
    if amount_in == 0 || state.liquidity == 0 || state.sqrt_price_x96.is_zero() {
        return 0;
    }

    let sqrt_price = u256_to_f64(state.sqrt_price_x96) / 2f64.powi(96);
    let liquidity = state.liquidity as f64;
    let amount_after_fee = (amount_in as f64) * (1.0 - (state.fee as f64 / 1_000_000.0));
    if !sqrt_price.is_finite()
        || sqrt_price <= 0.0
        || !liquidity.is_finite()
        || liquidity <= 0.0
        || !amount_after_fee.is_finite()
        || amount_after_fee <= 0.0
    {
        return 0;
    }

    let sqrt_next = if zero_for_one {
        (liquidity * sqrt_price) / (liquidity + amount_after_fee * sqrt_price)
    } else {
        sqrt_price + amount_after_fee / liquidity
    };
    if !sqrt_next.is_finite() || sqrt_next <= 0.0 {
        return 0;
    }
    if price_limit_exceeded(sqrt_price, sqrt_next, zero_for_one) {
        return 0;
    }

    let out = if zero_for_one {
        liquidity * (sqrt_price - sqrt_next)
    } else {
        liquidity * ((1.0 / sqrt_price) - (1.0 / sqrt_next))
    };
    if !out.is_finite() || out <= 0.0 {
        return 0;
    }
    out as u128
}

fn u256_to_f64(value: U256) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(0.0)
}

fn price_limit_exceeded(sqrt_price: f64, sqrt_next: f64, zero_for_one: bool) -> bool {
    let Some(bps) = price_limit_bps() else {
        return false;
    };
    let factor = bps / 10_000.0;
    if zero_for_one {
        sqrt_next < sqrt_price * (1.0 - factor)
    } else {
        sqrt_next > sqrt_price * (1.0 + factor)
    }
}

fn price_limit_capacity_in(state: &V3PoolState, zero_for_one: bool) -> Option<u128> {
    let bps = price_limit_bps()?;
    if state.liquidity == 0 || state.sqrt_price_x96.is_zero() {
        return Some(0);
    }

    let sqrt_price = u256_to_f64(state.sqrt_price_x96) / 2f64.powi(96);
    let liquidity = state.liquidity as f64;
    if !sqrt_price.is_finite() || sqrt_price <= 0.0 || !liquidity.is_finite() || liquidity <= 0.0 {
        return Some(0);
    }

    let factor = bps / 10_000.0;
    let amount_after_fee = if zero_for_one {
        let sqrt_limit = sqrt_price * (1.0 - factor);
        if sqrt_limit <= 0.0 {
            return Some(0);
        }
        liquidity * (sqrt_price - sqrt_limit) / (sqrt_price * sqrt_limit)
    } else {
        let sqrt_limit = sqrt_price * (1.0 + factor);
        liquidity * (sqrt_limit - sqrt_price)
    };
    let fee_factor = 1.0 - (state.fee as f64 / 1_000_000.0);
    if !amount_after_fee.is_finite() || amount_after_fee <= 0.0 || fee_factor <= 0.0 {
        return Some(0);
    }
    Some(f64_to_u128(amount_after_fee / fee_factor))
}

fn price_limit_bps() -> Option<f64> {
    let bps = std::env::var("V3_SQRT_PRICE_LIMIT_BPS")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| *value >= 0.0 && *value < 10_000.0)
        .unwrap_or(50.0);
    (bps > 0.0).then_some(bps)
}

fn f64_to_u128(value: f64) -> u128 {
    if !value.is_finite() || value <= 0.0 {
        0
    } else if value >= u128::MAX as f64 {
        u128::MAX
    } else {
        value as u128
    }
}
