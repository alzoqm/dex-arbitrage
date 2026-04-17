use alloy::primitives::Address;

use crate::{
    graph::GraphSnapshot,
    types::{EdgeRef, PoolSpecificState, V2PoolState},
};

const PPM: f64 = 1_000_000.0;

pub(crate) fn optimal_v2_allocations(
    snapshot: &GraphSnapshot,
    edge_refs: &[EdgeRef],
    token_in: Address,
    token_out: Address,
    total_in: u128,
) -> Option<Vec<u128>> {
    if total_in == 0 || edge_refs.is_empty() {
        return None;
    }

    let mut legs = Vec::with_capacity(edge_refs.len());
    for edge_ref in edge_refs {
        let edge = snapshot.edge(*edge_ref)?;
        let pool = snapshot.pool(edge.pool_id)?;
        let state = match &pool.state {
            PoolSpecificState::UniswapV2Like(state) => state,
            _ => return None,
        };
        let mut leg = V2AllocationLeg::from_state(
            state,
            pool.token_addresses.as_slice(),
            token_in,
            token_out,
        )?;
        leg.capacity =
            (edge.liquidity.safe_capacity_in > 0).then_some(edge.liquidity.safe_capacity_in);
        legs.push(leg);
    }

    solve_allocations(&legs, total_in)
}

#[derive(Debug, Clone, Copy)]
struct V2AllocationLeg {
    numerator: f64,
    denominator_const: f64,
    denominator_slope: f64,
    capacity: Option<u128>,
}

impl V2AllocationLeg {
    fn from_state(
        state: &V2PoolState,
        tokens: &[Address],
        token_in: Address,
        token_out: Address,
    ) -> Option<Self> {
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
        if reserve_in == 0 || reserve_out == 0 || state.fee_ppm as u128 >= 1_000_000 {
            return None;
        }
        let fee_factor = PPM - state.fee_ppm as f64;
        Some(Self {
            numerator: fee_factor * reserve_out as f64,
            denominator_const: reserve_in as f64 * PPM,
            denominator_slope: fee_factor,
            capacity: None,
        })
    }

    fn allocation_at_marginal(&self, marginal: f64) -> f64 {
        if marginal <= 0.0 {
            return self.capacity.map(|cap| cap as f64).unwrap_or(f64::MAX);
        }
        let initial = self.marginal_at(0.0);
        if marginal >= initial {
            return 0.0;
        }
        let amount = ((self.numerator * self.denominator_const / marginal).sqrt()
            - self.denominator_const)
            / self.denominator_slope;
        let amount = amount.max(0.0);
        if let Some(capacity) = self.capacity {
            amount.min(capacity as f64)
        } else {
            amount
        }
    }

    fn marginal_at(&self, amount: f64) -> f64 {
        self.numerator * self.denominator_const
            / (self.denominator_const + self.denominator_slope * amount).powi(2)
    }

    fn capacity_remaining(&self, allocated: u128) -> u128 {
        self.capacity
            .map(|capacity| capacity.saturating_sub(allocated))
            .unwrap_or(u128::MAX)
    }
}

fn solve_allocations(legs: &[V2AllocationLeg], total_in: u128) -> Option<Vec<u128>> {
    if legs.is_empty() {
        return None;
    }
    if legs.len() == 1 {
        if legs[0]
            .capacity
            .map(|capacity| total_in > capacity)
            .unwrap_or(false)
        {
            return None;
        }
        return Some(vec![total_in]);
    }

    let finite_capacity_sum = legs.iter().try_fold(0u128, |acc, leg| {
        leg.capacity.map(|cap| acc.saturating_add(cap))
    });
    if finite_capacity_sum
        .map(|capacity| total_in > capacity)
        .unwrap_or(false)
    {
        return None;
    }

    let mut high = legs
        .iter()
        .map(|leg| leg.marginal_at(0.0))
        .fold(0.0_f64, f64::max);
    if !high.is_finite() || high <= 0.0 {
        return None;
    }
    let mut low = 0.0_f64;
    for _ in 0..80 {
        let mid = (low + high) / 2.0;
        let sum = legs
            .iter()
            .map(|leg| leg.allocation_at_marginal(mid))
            .sum::<f64>();
        if sum > total_in as f64 {
            low = mid;
        } else {
            high = mid;
        }
    }

    let mut allocations = legs
        .iter()
        .map(|leg| {
            let amount = leg.allocation_at_marginal(high).round();
            f64_to_u128_saturating(amount).min(leg.capacity.unwrap_or(u128::MAX))
        })
        .collect::<Vec<_>>();

    rebalance_to_total(legs, &mut allocations, total_in)?;
    allocations
        .iter()
        .any(|amount| *amount > 0)
        .then_some(allocations)
}

fn rebalance_to_total(
    legs: &[V2AllocationLeg],
    allocations: &mut [u128],
    total_in: u128,
) -> Option<()> {
    let mut allocated = allocations.iter().copied().sum::<u128>();
    if allocated > total_in {
        let mut order = (0..allocations.len()).collect::<Vec<_>>();
        order.sort_by(|&a, &b| {
            legs[a]
                .marginal_at(allocations[a] as f64)
                .partial_cmp(&legs[b].marginal_at(allocations[b] as f64))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut excess = allocated - total_in;
        for idx in order {
            if excess == 0 {
                break;
            }
            let removed = allocations[idx].min(excess);
            allocations[idx] -= removed;
            excess -= removed;
        }
        allocated = allocations.iter().copied().sum::<u128>();
    }

    if allocated < total_in {
        let mut order = (0..allocations.len()).collect::<Vec<_>>();
        order.sort_by(|&a, &b| {
            legs[b]
                .marginal_at(allocations[b] as f64)
                .partial_cmp(&legs[a].marginal_at(allocations[a] as f64))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut remaining = total_in - allocated;
        for idx in order {
            if remaining == 0 {
                break;
            }
            let added = legs[idx]
                .capacity_remaining(allocations[idx])
                .min(remaining);
            allocations[idx] = allocations[idx].saturating_add(added);
            remaining -= added;
        }
    }

    (allocations.iter().copied().sum::<u128>() == total_in).then_some(())
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

#[cfg(test)]
mod tests {
    use super::{solve_allocations, V2AllocationLeg};

    fn leg(reserve_in: u128, reserve_out: u128) -> V2AllocationLeg {
        let fee_factor = 1_000_000.0;
        V2AllocationLeg {
            numerator: fee_factor * reserve_out as f64,
            denominator_const: reserve_in as f64 * 1_000_000.0,
            denominator_slope: fee_factor,
            capacity: None,
        }
    }

    #[test]
    fn waterfills_v2_pools_by_marginal_output() {
        let legs = [leg(1_000, 1_100), leg(10_000, 10_500)];

        let allocations = solve_allocations(&legs, 1_000).unwrap();

        assert_eq!(allocations.iter().sum::<u128>(), 1_000);
        assert!(allocations[0] > 0);
        assert!(allocations[1] > 0);

        let m0 = legs[0].marginal_at(allocations[0] as f64);
        let m1 = legs[1].marginal_at(allocations[1] as f64);
        let relative_gap = (m0 - m1).abs() / m0.max(m1);
        assert!(relative_gap < 0.01, "m0={m0}, m1={m1}");
    }

    #[test]
    fn respects_capacity_caps() {
        let mut capped = leg(1_000, 1_100);
        capped.capacity = Some(100);
        let legs = [capped, leg(10_000, 10_500)];

        let allocations = solve_allocations(&legs, 1_000).unwrap();

        assert_eq!(allocations.iter().sum::<u128>(), 1_000);
        assert!(allocations[0] <= 100);
    }
}
