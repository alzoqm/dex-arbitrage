pub mod balancer;
pub mod curve;
pub mod uniswap_v2;
pub mod uniswap_v3;

use alloy::primitives::U256;

pub fn weight_log_q32(rate: f64) -> i64 {
    if !rate.is_finite() || rate <= 0.0 {
        return i64::MAX / 4;
    }
    (-rate.ln() * ((1u64 << 32) as f64)) as i64
}

pub fn q128_from_f64(rate: f64) -> U256 {
    if !rate.is_finite() || rate <= 0.0 {
        return U256::ZERO;
    }
    let scaled = (rate * ((1u128 << 64) as f64) * ((1u128 << 64) as f64)) as u128;
    U256::from(scaled)
}

pub fn apply_fee_rate(rate: f64, fee_ppm: u32) -> f64 {
    let fee_factor = 1.0 - (fee_ppm as f64 / 1_000_000.0);
    rate * fee_factor
}
