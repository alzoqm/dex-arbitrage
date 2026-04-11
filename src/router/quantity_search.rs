use std::sync::Arc;

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
        let token_idx = match snapshot.token_index(candidate.start_token) {
            Some(idx) => idx,
            None => return vec![1],
        };
        let token = &snapshot.tokens[token_idx];
        let decimals = token.decimals;

        // Calculate max position in raw token units from USD cap
        let max_position_raw = self.calculate_max_position_raw(token);

        // Calculate min trade amount in raw units
        let min_trade_raw = usd_e8_to_amount(self.settings.risk.min_trade_usd_e8, token)
            .unwrap_or(10u128.pow(decimals as u32) / 100); // Default: 0.01 token

        let unit = min_trade_raw.max(1);
        if unit > max_position_raw {
            return Vec::new();
        }

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

    pub fn refinement_points(&self, center: u128, max_position_raw: u128) -> Vec<u128> {
        [
            center.saturating_mul(3) / 4,
            center,
            center.saturating_mul(5) / 4,
        ]
        .into_iter()
        .filter(|amount| *amount > 0 && *amount <= max_position_raw)
        .collect()
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
