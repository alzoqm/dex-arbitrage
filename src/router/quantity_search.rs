use std::sync::Arc;

use alloy::primitives::U256;

use crate::{
    config::Settings, graph::GraphSnapshot, risk::valuation::usd_e8_to_amount, types::CandidatePath,
};

#[derive(Debug)]
pub struct QuantitySearcher {
    settings: Arc<Settings>,
}

impl QuantitySearcher {
    pub fn new(settings: Arc<Settings>) -> Self {
        Self { settings }
    }

    pub fn ladder(&self, snapshot: &GraphSnapshot, candidate: &CandidatePath) -> Vec<u128> {
        let Some((unit, max_position_raw)) = self.search_range(snapshot, candidate) else {
            return Vec::new();
        };

        let mut ladder = Vec::new();
        let mut current = unit;

        while current <= max_position_raw {
            ladder.push(current);
            let next = current.saturating_mul(2);
            if next == current {
                break;
            }
            current = next;
        }

        ladder
    }

    pub fn search_range(
        &self,
        snapshot: &GraphSnapshot,
        candidate: &CandidatePath,
    ) -> Option<(u128, u128)> {
        let token_idx = snapshot.token_index(candidate.start_token)?;
        let token = &snapshot.tokens[token_idx];
        let decimals = token.decimals;
        let max_position_raw = self
            .calculate_max_position_raw(token)
            .min(route_start_capacity_raw(snapshot, candidate).unwrap_or(u128::MAX));
        let min_trade_raw = usd_e8_to_amount(self.settings.risk.min_trade_usd_e8, token)
            .unwrap_or(10u128.pow(decimals as u32) / 100);

        let min_trade_raw = min_trade_raw.max(1);
        (min_trade_raw <= max_position_raw).then_some((min_trade_raw, max_position_raw))
    }

    pub fn refinement_points(&self, center: u128, max_position_raw: u128) -> Vec<u128> {
        let mut points = [
            center / 2,
            center.saturating_mul(3) / 4,
            center.saturating_mul(7) / 8,
            center,
            center.saturating_mul(9) / 8,
            center.saturating_mul(5) / 4,
            center.saturating_mul(3) / 2,
            center.saturating_mul(2),
        ]
        .into_iter()
        .filter(|amount| *amount > 0 && *amount <= max_position_raw)
        .collect::<Vec<_>>();
        points.sort_unstable();
        points.dedup();
        points
    }

    pub fn max_position_raw(&self, snapshot: &GraphSnapshot, candidate: &CandidatePath) -> u128 {
        let Some(token_idx) = snapshot.token_index(candidate.start_token) else {
            return self.settings.risk.max_position;
        };
        self.calculate_max_position_raw(&snapshot.tokens[token_idx])
    }

    /// Calculate maximum position in raw token units from USD caps
    fn calculate_max_position_raw(&self, token: &crate::types::TokenInfo) -> u128 {
        // Use token-specific USD cap if available, otherwise global cap
        let max_position_usd_e8 = token
            .max_position_usd_e8
            .unwrap_or(self.settings.risk.max_position_usd_e8);

        // Convert USD cap to raw token amount
        match usd_e8_to_amount(max_position_usd_e8, token) {
            Some(amount) => amount,
            None => {
                // Fallback to legacy raw cap if no price available
                self.settings.risk.max_position
            }
        }
    }
}

fn route_start_capacity_raw(snapshot: &GraphSnapshot, candidate: &CandidatePath) -> Option<u128> {
    if candidate.path.is_empty() {
        return None;
    }

    let mut cumulative_rate = 1.0f64;
    let mut max_start = f64::INFINITY;
    for hop in &candidate.path {
        let edge = snapshot
            .pair_edges(hop.from, hop.to)
            .into_iter()
            .filter_map(|edge_ref| snapshot.edge(edge_ref))
            .find(|edge| edge.pool_id == hop.pool_id)?;
        if edge.liquidity.safe_capacity_in == 0 || cumulative_rate <= 0.0 {
            return Some(0);
        }

        max_start = max_start.min(edge.liquidity.safe_capacity_in as f64 / cumulative_rate);
        let rate = q128_to_f64(edge.spot_rate_q128);
        if !rate.is_finite() || rate <= 0.0 {
            return Some(0);
        }
        cumulative_rate *= rate;
        if !cumulative_rate.is_finite() {
            return Some(0);
        }
    }

    Some(f64_to_u128(max_start))
}

fn q128_to_f64(value: U256) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(0.0) / 2f64.powi(128)
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
