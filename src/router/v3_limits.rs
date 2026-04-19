use alloy::primitives::{U160, U256};

pub const MIN_SQRT_RATIO_PLUS_ONE: u64 = 4_295_128_740;
pub const MAX_SQRT_RATIO_MINUS_ONE: &str = "1461446703485210103287273052203988822378723970341";

pub fn sqrt_price_limit_x96(current: U256, zero_for_one: bool) -> U256 {
    let bps = std::env::var("V3_SQRT_PRICE_LIMIT_BPS")
        .ok()
        .and_then(|value| value.parse::<u128>().ok())
        .filter(|value| *value < 10_000)
        .unwrap_or(200);
    if bps == 0 || current.is_zero() {
        return if zero_for_one {
            U256::from(MIN_SQRT_RATIO_PLUS_ONE)
        } else {
            max_sqrt_ratio_minus_one()
        };
    }

    let denominator = U256::from(10_000u128);
    let numerator = if zero_for_one {
        U256::from(10_000u128.saturating_sub(bps))
    } else {
        U256::from(10_000u128.saturating_add(bps))
    };
    let raw = current.saturating_mul(numerator) / denominator;
    if zero_for_one {
        raw.max(U256::from(MIN_SQRT_RATIO_PLUS_ONE))
    } else {
        raw.min(max_sqrt_ratio_minus_one())
    }
}

pub fn sqrt_price_limit_u160(current: U256, zero_for_one: bool) -> U160 {
    u160_saturating_from_u256(sqrt_price_limit_x96(current, zero_for_one))
}

fn max_sqrt_ratio_minus_one() -> U256 {
    U256::from_str_radix(MAX_SQRT_RATIO_MINUS_ONE, 10).expect("valid max sqrt ratio")
}

fn u160_saturating_from_u256(value: U256) -> U160 {
    let bytes = value.to_be_bytes::<32>();
    if bytes[..12].iter().any(|byte| *byte != 0) {
        return U160::from_be_slice(&[0xff; 20]);
    }
    U160::from_be_slice(&bytes[12..])
}

#[cfg(test)]
mod tests {
    use alloy::primitives::{U160, U256};

    use super::{
        sqrt_price_limit_u160, sqrt_price_limit_x96, MAX_SQRT_RATIO_MINUS_ONE,
        MIN_SQRT_RATIO_PLUS_ONE,
    };

    #[test]
    fn default_bps_moves_limit_like_execution() {
        let current = U256::from(1_000_000u64);

        assert_eq!(
            sqrt_price_limit_u160(current, true),
            U160::from(MIN_SQRT_RATIO_PLUS_ONE)
        );
        assert_eq!(
            sqrt_price_limit_u160(U256::from(1_000_000_000_000u64), true),
            U160::from(980_000_000_000u64)
        );
        assert_eq!(
            sqrt_price_limit_u160(current, false),
            U160::from(1_020_000u64)
        );
    }

    #[test]
    fn zero_current_falls_back_to_contract_bounds() {
        assert_eq!(
            sqrt_price_limit_x96(U256::ZERO, true),
            U256::from(MIN_SQRT_RATIO_PLUS_ONE)
        );
        assert_eq!(
            sqrt_price_limit_x96(U256::ZERO, false),
            U256::from_str_radix(MAX_SQRT_RATIO_MINUS_ONE, 10).unwrap()
        );
    }
}
