use alloy::primitives::Address;
use dex_arbitrage::{
    config::{
        ContractSettings, RiskSettings, RpcSettings, SearchSettings, Settings, UniversePolicy,
    },
    risk::depeg_guard::DepegGuard,
    types::{AmmKind, Chain, DiscoveryKind},
};

fn addr(byte: u8) -> Address {
    Address::from_slice(&[byte; 20])
}

#[test]
fn depeg_guard_blocks_unhealthy_stable_token() {
    let settings = Settings {
        chain: Chain::Base,
        chain_id: 8453,
        native_symbol: "ETH".to_string(),
        operator_private_key: None,
        deployer_private_key: None,
        safe_owner: None,
        simulation_only: true,
        json_logs: false,
        prometheus_bind: "127.0.0.1:9898".to_string(),
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
            executor_address: Some(addr(99)),
            aave_pool: None,
            strict_target_allowlist: false,
        },
        risk: RiskSettings {
            max_hops: 4,
            screening_margin_bps: 3,
            min_net_profit: 1,
            min_net_profit_usd_e8: 10_000_000,
            min_trade_usd_e8: 1_000_000_000,
            poll_interval_ms: 400,
            event_backfill_blocks: 10,
            staleness_timeout_ms: 60_000,
            gas_risk_buffer_pct: 0.15,
            gas_price_ceiling_wei: 3_000_000_000,
            max_position: 1_000_000_000,
            max_position_usd_e8: 200_000_000_000,
            max_flash_loan: 1_000_000_000,
            max_flash_loan_usd_e8: 1_000_000_000_000,
            daily_loss_limit: -1_000_000,
            daily_loss_limit_usd_e8: 50_000_000_000,
            min_profit_realization_bps: 9000,
            max_concurrent_tx: 1,
            pool_health_min_bps: 9_000,
            stable_depeg_cutoff_e6: 995_000,
        },
        search: SearchSettings::default(),
        policy: UniversePolicy::default(),
        tokens: vec![
            dex_arbitrage::config::TokenConfig {
                symbol: "USDC".to_string(),
                address: addr(1),
                decimals: 6,
                is_stable: true,
                is_cycle_anchor: true,
                flash_loan_enabled: true,
                allow_self_funded: true,
                manual_price_usd_e8: Some(100_000_000),
                max_position_usd_e8: None,
                max_flash_loan_usd_e8: None,
            },
            dex_arbitrage::config::TokenConfig {
                symbol: "USDT".to_string(),
                address: addr(2),
                decimals: 6,
                is_stable: true,
                is_cycle_anchor: true,
                flash_loan_enabled: true,
                allow_self_funded: true,
                manual_price_usd_e8: Some(98_000_000),
                max_position_usd_e8: None,
                max_flash_loan_usd_e8: None,
            },
            dex_arbitrage::config::TokenConfig {
                symbol: "WMATIC".to_string(),
                address: addr(3),
                decimals: 18,
                is_stable: false,
                is_cycle_anchor: true,
                flash_loan_enabled: true,
                allow_self_funded: false,
                manual_price_usd_e8: Some(80_000_000),
                max_position_usd_e8: None,
                max_flash_loan_usd_e8: None,
            },
        ],
        dexes: vec![dex_arbitrage::config::DexConfig {
            name: "uniswap_v2".to_string(),
            amm_kind: AmmKind::UniswapV2Like,
            discovery_kind: DiscoveryKind::FactoryAllPairs,
            factory: None,
            registry: None,
            vault: None,
            quoter: None,
            fee_ppm: 3_000,
            start_block: 0,
            enabled: true,
        }],
    };

    let guard = DepegGuard::new(&settings);
    assert!(!guard.stable_routes_allowed());
    assert!(!guard.token_is_healthy(addr(2)));
    assert!(guard.token_is_healthy(addr(1)));
    assert!(guard.token_is_healthy(addr(3)));
}
