use std::{collections::HashMap, sync::Arc};

use crate::{
    config::{Settings, TokenConfig},
    types::{PoolAdmissionStatus, PoolSpecificState, PoolState},
};

#[derive(Debug)]
pub struct AdmissionEngine {
    token_map: HashMap<alloy::primitives::Address, TokenConfig>,
}

impl AdmissionEngine {
    pub fn new(settings: Arc<Settings>) -> Self {
        Self {
            token_map: settings.token_map(),
        }
    }

    pub fn admit(&self, pool: &PoolState) -> bool {
        if pool.token_addresses.len() < 2 {
            return false;
        }
        if pool
            .token_addresses
            .iter()
            .any(|token| !self.token_map.contains_key(token))
        {
            return false;
        }
        if matches!(pool.admission_status, PoolAdmissionStatus::Excluded) {
            return false;
        }
        if pool.health.paused || pool.health.quarantined {
            return false;
        }

        estimated_liquidity_floor_ok(pool)
    }
}

fn estimated_liquidity_floor_ok(pool: &PoolState) -> bool {
    match &pool.state {
        PoolSpecificState::UniswapV2Like(state) => state.reserve0 > 10_000 && state.reserve1 > 10_000,
        PoolSpecificState::UniswapV3Like(state) => state.liquidity > 10_000,
        PoolSpecificState::CurvePlain(state) => state.balances.iter().all(|b| *b > 10_000),
        PoolSpecificState::BalancerWeighted(state) => state.balances.iter().all(|b| *b > 10_000),
    }
}
