use std::collections::HashMap;

use alloy::primitives::Address;

use crate::config::Settings;

#[derive(Debug, Clone)]
pub struct DepegGuard {
    stable_depeg_cutoff_e6: u32,
    manual_prices: HashMap<Address, u64>,
    stable_tokens: Vec<Address>,
}

impl DepegGuard {
    pub fn new(settings: &Settings) -> Self {
        let manual_prices = settings
            .tokens
            .iter()
            .filter_map(|token| {
                token
                    .manual_price_usd_e8
                    .map(|price| (token.address, price))
            })
            .collect();
        let stable_tokens = settings
            .tokens
            .iter()
            .filter(|token| token.is_stable)
            .map(|token| token.address)
            .collect();

        Self {
            stable_depeg_cutoff_e6: settings.risk.stable_depeg_cutoff_e6,
            manual_prices,
            stable_tokens,
        }
    }

    pub fn stable_routes_allowed(&self) -> bool {
        self.stable_tokens
            .iter()
            .all(|token| self.token_is_healthy(*token))
    }

    pub fn token_is_healthy(&self, token: Address) -> bool {
        if !self.stable_tokens.contains(&token) {
            return true;
        }

        match self.manual_prices.get(&token).copied() {
            Some(price_e8) => {
                let cutoff_e8 = (self.stable_depeg_cutoff_e6 as u64) * 100;
                price_e8 >= cutoff_e8
            }
            None => true,
        }
    }
}
