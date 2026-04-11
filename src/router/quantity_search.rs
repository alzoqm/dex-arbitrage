use std::sync::Arc;

use crate::{config::Settings, graph::GraphSnapshot, types::CandidatePath};

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
        let decimals = snapshot.tokens[token_idx].decimals;
        let unit = 10u128.saturating_pow(decimals.saturating_sub(2) as u32).max(1);
        let mut ladder = Vec::new();
        let mut current = unit;
        while current <= self.settings.risk.max_position {
            ladder.push(current);
            current = current.saturating_mul(2);
            if current == 0 {
                break;
            }
        }
        if ladder.is_empty() {
            ladder.push(unit);
        }
        ladder
    }

    pub fn refinement_points(&self, center: u128) -> Vec<u128> {
        [center.saturating_mul(3) / 4, center, center.saturating_mul(5) / 4]
            .into_iter()
            .filter(|amount| *amount > 0 && *amount <= self.settings.risk.max_position)
            .collect()
    }
}
