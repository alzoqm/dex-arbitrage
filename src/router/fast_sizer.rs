use alloy::primitives::Address;

use crate::{
    graph::GraphSnapshot,
    types::{CandidatePath, PoolSpecificState, V2PoolState},
};

const PPM: f64 = 1_000_000.0;

#[derive(Debug, Clone)]
pub struct FastSizer;

impl FastSizer {
    pub fn new() -> Self {
        Self
    }

    pub fn suggest_input_amounts(
        &self,
        snapshot: &GraphSnapshot,
        candidate: &CandidatePath,
        min_amount: u128,
        max_amount: u128,
        flash_premium_ppm: u128,
    ) -> Vec<u128> {
        let Some(curve) = v2_path_curve(snapshot, candidate) else {
            return Vec::new();
        };
        let marginal_cost = marginal_input_cost(snapshot, candidate, flash_premium_ppm);
        let Some(optimal) = curve.optimal_input(marginal_cost) else {
            return Vec::new();
        };
        let target = optimal.clamp(min_amount as f64, max_amount as f64);
        let expected_profit = curve.output(target) - marginal_cost * target;
        if !expected_profit.is_finite() || expected_profit <= 0.0 {
            return Vec::new();
        }

        let mut amounts = Vec::new();
        for multiplier in [0.50, 0.75, 0.875, 1.0, 1.125, 1.25, 1.50, 2.0] {
            push_amount(&mut amounts, target * multiplier, min_amount, max_amount);
        }
        push_amount(
            &mut amounts,
            target - target.sqrt().max(1.0),
            min_amount,
            max_amount,
        );
        push_amount(
            &mut amounts,
            target + target.sqrt().max(1.0),
            min_amount,
            max_amount,
        );
        push_amount(&mut amounts, min_amount as f64, min_amount, max_amount);
        push_amount(&mut amounts, max_amount as f64, min_amount, max_amount);

        amounts.sort_unstable();
        amounts.dedup();
        amounts
    }
}

fn marginal_input_cost(
    snapshot: &GraphSnapshot,
    candidate: &CandidatePath,
    flash_premium_ppm: u128,
) -> f64 {
    let full_flash = snapshot
        .token_index(candidate.start_token)
        .and_then(|idx| snapshot.tokens.get(idx))
        .map(|token| token.flash_loan_enabled && !token.allow_self_funded)
        .unwrap_or(false);
    if full_flash {
        1.0 + flash_premium_ppm as f64 / PPM
    } else {
        1.0
    }
}

fn v2_path_curve(snapshot: &GraphSnapshot, candidate: &CandidatePath) -> Option<RationalSwap> {
    let mut curve = RationalSwap::identity();
    for hop in &candidate.path {
        let pool = snapshot.pool(hop.pool_id)?;
        let state = match &pool.state {
            PoolSpecificState::UniswapV2Like(state) => state,
            _ => return None,
        };
        let leg = v2_leg(state, pool.token_addresses.as_slice(), hop.from, hop.to)?;
        curve = curve.then(leg)?;
    }
    Some(curve)
}

fn v2_leg(
    state: &V2PoolState,
    tokens: &[Address],
    token_in: Address,
    token_out: Address,
) -> Option<RationalSwap> {
    let zero_for_one = match (tokens.first().copied(), tokens.get(1).copied()) {
        (Some(token0), Some(token1)) if token0 == token_in && token1 == token_out => true,
        (Some(token0), Some(token1)) if token1 == token_in && token0 == token_out => false,
        _ => return None,
    };
    let (reserve_in, reserve_out) = if zero_for_one {
        (state.reserve0, state.reserve1)
    } else {
        (state.reserve1, state.reserve0)
    };
    RationalSwap::from_v2_reserves(reserve_in, reserve_out, state.fee_ppm)
}

fn push_amount(amounts: &mut Vec<u128>, amount: f64, min_amount: u128, max_amount: u128) {
    if !amount.is_finite() || amount <= 0.0 {
        return;
    }
    let amount = amount.round().clamp(min_amount as f64, max_amount as f64);
    if amount < 1.0 {
        return;
    }
    amounts.push(f64_to_u128_saturating(amount));
}

fn f64_to_u128_saturating(value: f64) -> u128 {
    if !value.is_finite() || value <= 0.0 {
        0
    } else if value >= u128::MAX as f64 {
        u128::MAX
    } else {
        value as u128
    }
}

#[derive(Debug, Clone, Copy)]
struct RationalSwap {
    numerator: f64,
    denominator_const: f64,
    denominator_slope: f64,
}

impl RationalSwap {
    fn identity() -> Self {
        Self {
            numerator: 1.0,
            denominator_const: 1.0,
            denominator_slope: 0.0,
        }
    }

    fn from_v2_reserves(reserve_in: u128, reserve_out: u128, fee_ppm: u32) -> Option<Self> {
        if reserve_in == 0 || reserve_out == 0 || fee_ppm as u128 >= 1_000_000 {
            return None;
        }
        let fee_factor = PPM - fee_ppm as f64;
        Some(
            Self {
                numerator: fee_factor * reserve_out as f64,
                denominator_const: reserve_in as f64 * PPM,
                denominator_slope: fee_factor,
            }
            .normalized()?,
        )
    }

    fn then(self, next: Self) -> Option<Self> {
        let numerator = next.numerator * self.numerator;
        let denominator_const = next.denominator_const * self.denominator_const;
        let denominator_slope = next.denominator_const * self.denominator_slope
            + next.denominator_slope * self.numerator;
        Self {
            numerator,
            denominator_const,
            denominator_slope,
        }
        .normalized()
    }

    fn output(&self, amount_in: f64) -> f64 {
        self.numerator * amount_in / (self.denominator_const + self.denominator_slope * amount_in)
    }

    fn optimal_input(&self, marginal_cost: f64) -> Option<f64> {
        if marginal_cost <= 0.0 || self.denominator_slope <= 0.0 {
            return None;
        }
        let radicand = self.numerator * self.denominator_const / marginal_cost;
        if !radicand.is_finite() || radicand <= 0.0 {
            return None;
        }
        let input = (radicand.sqrt() - self.denominator_const) / self.denominator_slope;
        (input.is_finite() && input > 0.0).then_some(input)
    }

    fn normalized(self) -> Option<Self> {
        if !self.numerator.is_finite()
            || !self.denominator_const.is_finite()
            || !self.denominator_slope.is_finite()
            || self.numerator <= 0.0
            || self.denominator_const <= 0.0
            || self.denominator_slope < 0.0
        {
            return None;
        }
        let scale = self
            .numerator
            .abs()
            .max(self.denominator_const.abs())
            .max(self.denominator_slope.abs());
        if !(1.0e-120..=1.0e120).contains(&scale) {
            Some(Self {
                numerator: self.numerator / scale,
                denominator_const: self.denominator_const / scale,
                denominator_slope: self.denominator_slope / scale,
            })
        } else {
            Some(self)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use alloy::primitives::Address;
    use smallvec::smallvec;

    use crate::{
        graph::GraphSnapshot,
        types::{
            AmmKind, CandidateHop, CandidatePath, PoolAdmissionStatus, PoolHealth,
            PoolSpecificState, PoolState, TokenBehavior, TokenInfo, V2PoolState,
        },
    };

    use super::{FastSizer, RationalSwap};

    fn addr(byte: u8) -> Address {
        Address::from_slice(&[byte; 20])
    }

    fn token(address: Address, symbol: &str) -> TokenInfo {
        TokenInfo {
            address,
            symbol: symbol.to_string(),
            decimals: 6,
            is_stable: false,
            is_cycle_anchor: false,
            flash_loan_enabled: false,
            allow_self_funded: true,
            behavior: TokenBehavior::default(),
            manual_price_usd_e8: Some(100_000_000),
            max_position_usd_e8: None,
            max_flash_loan_usd_e8: None,
        }
    }

    fn v2_pool(
        pool_id: Address,
        token0: Address,
        token1: Address,
        reserve0: u128,
        reserve1: u128,
    ) -> PoolState {
        PoolState {
            pool_id,
            dex_name: "test_v2".to_string(),
            kind: AmmKind::UniswapV2Like,
            token_addresses: vec![token0, token1],
            token_symbols: vec!["A".to_string(), "B".to_string()],
            factory: None,
            registry: None,
            vault: None,
            quoter: None,
            admission_status: PoolAdmissionStatus::Allowed,
            health: PoolHealth::default(),
            state: PoolSpecificState::UniswapV2Like(V2PoolState {
                reserve0,
                reserve1,
                fee_ppm: 0,
            }),
            last_updated_block: 1,
            extras: HashMap::new(),
        }
    }

    #[test]
    fn v2_closed_form_optimum_matches_bruteforce() {
        let first = RationalSwap::from_v2_reserves(1_000, 2_000, 0).unwrap();
        let second = RationalSwap::from_v2_reserves(1_000, 700, 0).unwrap();
        let curve = first.then(second).unwrap();

        let closed_form = curve.optimal_input(1.0).unwrap().round() as u128;
        let brute = (1u128..=500)
            .max_by_key(|amount| {
                let profit = curve.output(*amount as f64) - *amount as f64;
                (profit * 1_000_000.0) as i128
            })
            .unwrap();

        assert!(
            closed_form.abs_diff(brute) <= 1,
            "closed-form={closed_form}, brute={brute}"
        );
    }

    #[test]
    fn suggests_amounts_for_v2_cycle() {
        let token_a = addr(1);
        let token_b = addr(2);
        let pool_ab = addr(10);
        let pool_ba = addr(11);
        let pools = HashMap::from([
            (pool_ab, v2_pool(pool_ab, token_a, token_b, 1_000, 2_000)),
            (pool_ba, v2_pool(pool_ba, token_b, token_a, 1_000, 700)),
        ]);
        let snapshot = GraphSnapshot::build(
            1,
            None,
            vec![token(token_a, "A"), token(token_b, "B")],
            pools,
        );
        let candidate = CandidatePath {
            snapshot_id: 1,
            start_token: token_a,
            start_symbol: "A".to_string(),
            screening_score_q32: 0,
            cycle_key: "A-B-A".to_string(),
            path: smallvec![
                CandidateHop {
                    from: token_a,
                    to: token_b,
                    pool_id: pool_ab,
                    amm_kind: AmmKind::UniswapV2Like,
                    dex_name: "test_v2".to_string(),
                },
                CandidateHop {
                    from: token_b,
                    to: token_a,
                    pool_id: pool_ba,
                    amm_kind: AmmKind::UniswapV2Like,
                    dex_name: "test_v2".to_string(),
                },
            ],
        };

        let amounts = FastSizer::new().suggest_input_amounts(&snapshot, &candidate, 1, 500, 0);

        assert!(!amounts.is_empty());
        assert!(amounts.iter().all(|amount| (1..=500).contains(amount)));
    }
}
