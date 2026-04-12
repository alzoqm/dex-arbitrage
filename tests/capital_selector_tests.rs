use alloy::primitives::{Address, U256};
use dex_arbitrage::{
    execution::capital_selector::CapitalSelector,
    types::{CapitalSource, ExactPlan, TokenBehavior, TokenInfo},
};

fn addr(byte: u8) -> Address {
    Address::from_slice(&[byte; 20])
}

fn token() -> TokenInfo {
    TokenInfo {
        address: addr(1),
        symbol: "USDC".to_string(),
        decimals: 6,
        is_stable: true,
        is_cycle_anchor: true,
        flash_loan_enabled: true,
        allow_self_funded: true,
        behavior: TokenBehavior::default(),
        manual_price_usd_e8: Some(100_000_000),
        max_position_usd_e8: None,
        max_flash_loan_usd_e8: None,
    }
}

fn plan(input_amount: u128, output_amount: u128) -> ExactPlan {
    let gross_profit_raw = output_amount as i128 - input_amount as i128;
    ExactPlan {
        snapshot_id: 1,
        input_token: addr(1),
        output_token: addr(1),
        input_amount,
        output_amount,
        gross_profit_raw,
        flash_premium_ppm: 900,
        flash_fee_raw: 0,
        net_profit_before_gas_raw: gross_profit_raw,
        contract_min_profit_raw: 0,
        input_value_usd_e8: 0,
        flash_loan_value_usd_e8: 0,
        gross_profit_usd_e8: 0,
        flash_fee_usd_e8: 0,
        actual_flash_fee_usd_e8: 0,
        gas_cost_usd_e8: 0,
        net_profit_usd_e8: 0,
        expected_profit: gross_profit_raw,
        gas_limit: 0,
        gas_cost_wei: U256::ZERO,
        capital_source: CapitalSource::SelfFunded,
        flash_loan_amount: 0,
        actual_flash_fee_raw: 0,
        hops: Vec::new(),
    }
}

#[test]
fn self_funded_still_uses_conservative_full_flash_fee_for_profit() {
    let plan = plan(1_000_000, 1_100_000);
    let choice = CapitalSelector::choice_from_balance_and_fees(
        &plan,
        &token(),
        true,
        1_000_000_000_000,
        0,
        900,
        0,
    )
    .unwrap();

    assert_eq!(choice.source, CapitalSource::SelfFunded);
    assert_eq!(choice.loan_amount_raw, 0);
    assert_eq!(choice.flash_fee_raw, 900);
    assert_eq!(choice.actual_flash_fee_raw, 0);
    assert_eq!(choice.net_profit_before_gas_raw, 99_100);
}

#[test]
fn partial_executor_balance_uses_mixed_flash_for_only_the_shortfall() {
    let plan = plan(1_000_000, 1_100_000);
    let choice = CapitalSelector::choice_from_balance_and_fees(
        &plan,
        &token(),
        true,
        50_000_000,
        400_000,
        900,
        360,
    )
    .unwrap();

    assert_eq!(choice.source, CapitalSource::MixedFlashLoan);
    assert_eq!(choice.loan_amount_raw, 400_000);
    assert_eq!(choice.flash_fee_raw, 900);
    assert_eq!(choice.actual_flash_fee_raw, 360);
    assert_eq!(choice.net_profit_before_gas_raw, 99_100);
}

#[test]
fn zero_executor_balance_uses_full_flash_loan() {
    let plan = plan(1_000_000, 1_100_000);
    let choice = CapitalSelector::choice_from_balance_and_fees(
        &plan,
        &token(),
        true,
        1_000_000_000_000,
        1_000_000,
        900,
        900,
    )
    .unwrap();

    assert_eq!(choice.source, CapitalSource::FlashLoan);
    assert_eq!(choice.loan_amount_raw, 1_000_000);
}

#[test]
fn flash_cap_is_checked_against_actual_loan_amount_not_total_input() {
    let plan = plan(1_000_000, 1_100_000);
    let allowed = CapitalSelector::choice_from_balance_and_fees(
        &plan,
        &token(),
        true,
        50_000_000,
        400_000,
        900,
        360,
    );
    let rejected = CapitalSelector::choice_from_balance_and_fees(
        &plan,
        &token(),
        true,
        30_000_000,
        400_000,
        900,
        360,
    );

    assert!(allowed.is_some());
    assert!(rejected.is_none());
}
