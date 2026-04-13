use std::{sync::Arc, time::Duration};

use crate::{
    config::Settings,
    types::{PoolAdmissionStatus, PoolState},
};

#[derive(Debug)]
pub struct AdmissionEngine {
    min_confidence_bps: u16,
    staleness_timeout: Duration,
}

impl AdmissionEngine {
    pub fn new(settings: Arc<Settings>) -> Self {
        Self {
            min_confidence_bps: settings.risk.pool_health_min_bps,
            staleness_timeout: Duration::from_millis(settings.risk.staleness_timeout_ms),
        }
    }

    pub fn admit(&self, pool: &PoolState) -> bool {
        if pool.token_addresses.len() < 2 {
            return false;
        }
        if matches!(pool.admission_status, PoolAdmissionStatus::Excluded) {
            return false;
        }
        if !pool
            .health
            .healthy(self.min_confidence_bps, self.staleness_timeout)
        {
            return false;
        }

        true
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::SystemTime};

    use alloy::primitives::Address;

    use crate::{
        config::{
            ContractSettings, ExecutionSettings, RiskSettings, RpcSettings, SearchSettings,
            UniversePolicy,
        },
        types::{AmmKind, PoolHealth, PoolSpecificState, V2PoolState},
    };

    use super::*;

    #[test]
    fn admit_requires_two_tokens_allowed_status_and_healthy_pool() {
        let engine = AdmissionEngine::new(Arc::new(test_settings(9_000, 60_000)));
        let token0 = Address::from_slice(&[1u8; 20]);
        let token1 = Address::from_slice(&[2u8; 20]);

        let healthy_pool = test_pool(
            vec![token0, token1],
            PoolAdmissionStatus::Allowed,
            PoolHealth {
                stale: false,
                paused: false,
                quarantined: false,
                confidence_bps: 9_500,
                last_successful_refresh_block: 1,
                last_refresh_at: SystemTime::now(),
                recent_revert_count: 0,
            },
        );
        let short_pool = test_pool(
            vec![token0],
            PoolAdmissionStatus::Allowed,
            PoolHealth::default(),
        );
        let excluded_pool = test_pool(
            vec![token0, token1],
            PoolAdmissionStatus::Excluded,
            PoolHealth {
                stale: false,
                paused: false,
                quarantined: false,
                confidence_bps: 9_500,
                last_successful_refresh_block: 1,
                last_refresh_at: SystemTime::now(),
                recent_revert_count: 0,
            },
        );
        let low_confidence_pool = test_pool(
            vec![token0, token1],
            PoolAdmissionStatus::Allowed,
            PoolHealth {
                stale: false,
                paused: false,
                quarantined: false,
                confidence_bps: 8_999,
                last_successful_refresh_block: 1,
                last_refresh_at: SystemTime::now(),
                recent_revert_count: 0,
            },
        );
        let stale_pool = test_pool(
            vec![token0, token1],
            PoolAdmissionStatus::Allowed,
            PoolHealth {
                stale: false,
                paused: false,
                quarantined: false,
                confidence_bps: 9_500,
                last_successful_refresh_block: 1,
                last_refresh_at: SystemTime::UNIX_EPOCH,
                recent_revert_count: 0,
            },
        );

        assert!(engine.admit(&healthy_pool));
        assert!(!engine.admit(&short_pool));
        assert!(!engine.admit(&excluded_pool));
        assert!(!engine.admit(&low_confidence_pool));
        assert!(!engine.admit(&stale_pool));
    }

    fn test_settings(pool_health_min_bps: u16, staleness_timeout_ms: u64) -> Settings {
        Settings {
            chain: crate::types::Chain::Base,
            chain_id: 8453,
            native_symbol: "ETH".to_string(),
            operator_private_key: None,
            deployer_private_key: None,
            safe_owner: None,
            simulation_only: true,
            json_logs: false,
            prometheus_bind: "127.0.0.1:0".to_string(),
            rpc: RpcSettings {
                public_rpc_url: "http://localhost:8545".to_string(),
                fallback_rpc_url: None,
                preconf_rpc_url: None,
                ws_url: None,
                protected_rpc_url: None,
                private_submit_method: "eth_sendRawTransaction".to_string(),
                simulate_method: "eth_call".to_string(),
            },
            contracts: ContractSettings {
                executor_address: None,
                aave_pool: None,
                strict_target_allowlist: false,
            },
            risk: RiskSettings {
                max_hops: 4,
                screening_margin_bps: 0,
                min_net_profit: 0,
                min_net_profit_usd_e8: 0,
                min_trade_usd_e8: 0,
                poll_interval_ms: 1_000,
                event_backfill_blocks: 0,
                staleness_timeout_ms,
                gas_risk_buffer_pct: 0.0,
                gas_price_ceiling_wei: 0,
                max_position: 0,
                max_position_usd_e8: 0,
                max_flash_loan: 0,
                max_flash_loan_usd_e8: 0,
                daily_loss_limit: 0,
                daily_loss_limit_usd_e8: 0,
                min_profit_realization_bps: 0,
                max_concurrent_tx: 1,
                pool_health_min_bps,
                stable_depeg_cutoff_e6: 0,
            },
            search: SearchSettings::default(),
            execution: ExecutionSettings::default(),
            policy: UniversePolicy::default(),
            tokens: Vec::new(),
            dexes: Vec::new(),
        }
    }

    fn test_pool(
        token_addresses: Vec<Address>,
        admission_status: PoolAdmissionStatus,
        health: PoolHealth,
    ) -> PoolState {
        PoolState {
            pool_id: Address::from_slice(&[9u8; 20]),
            dex_name: "dex".to_string(),
            kind: AmmKind::UniswapV2Like,
            token_addresses,
            token_symbols: vec!["A".to_string(), "B".to_string()],
            factory: None,
            registry: None,
            vault: None,
            quoter: None,
            admission_status,
            health,
            state: PoolSpecificState::UniswapV2Like(V2PoolState {
                reserve0: 1,
                reserve1: 1,
                fee_ppm: 3_000,
            }),
            last_updated_block: 1,
            extras: Default::default(),
        }
    }
}
