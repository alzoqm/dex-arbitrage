use std::sync::Arc;

use crate::{
    config::Settings,
    types::{PoolAdmissionStatus, PoolState},
};

#[derive(Debug)]
pub struct AdmissionEngine;

impl AdmissionEngine {
    pub fn new(_settings: Arc<Settings>) -> Self {
        Self
    }

    pub fn admit(&self, pool: &PoolState) -> bool {
        if pool.token_addresses.len() < 2 {
            return false;
        }
        if matches!(pool.admission_status, PoolAdmissionStatus::Excluded) {
            return false;
        }
        if pool.health.paused || pool.health.quarantined {
            return false;
        }

        true
    }
}
