use alloy::primitives::U256;

use crate::types::TokenInfo;

/// Convert raw token amount to USD value in e8 precision.
///
/// Returns None if the token has no price configured.
///
/// Formula: amount_raw * price_usd_e8 / 10^decimals
pub fn amount_to_usd_e8(amount_raw: u128, token: &TokenInfo) -> Option<u128> {
    let price_e8 = token.manual_price_usd_e8?;
    if price_e8 == 0 {
        return None;
    }

    // Use U256 to avoid overflow: amount_raw * price_e8 / 10^decimals
    let amount = U256::from(amount_raw);
    let price = U256::from(price_e8);
    let divisor = U256::from(10u64).pow(U256::from(token.decimals));

    if divisor.is_zero() {
        return None;
    }

    let value = amount.checked_mul(price)?.checked_div(divisor)?;

    // Check if result fits in u128
    if value > U256::from(u128::MAX) {
        return None;
    }

    Some(value.to())
}

/// Convert USD value in e8 precision to raw token amount.
///
/// Returns None if the token has no price configured.
///
/// Formula: usd_e8 * 10^decimals / price_usd_e8
pub fn usd_e8_to_amount(usd_e8: u128, token: &TokenInfo) -> Option<u128> {
    let price_e8 = token.manual_price_usd_e8?;

    if price_e8 == 0 {
        return None;
    }

    // Use U256 to avoid overflow: usd_e8 * 10^decimals / price_e8
    let usd = U256::from(usd_e8);
    let divisor = U256::from(price_e8);
    let multiplier = U256::from(10u64).pow(U256::from(token.decimals));

    let value = usd.checked_mul(multiplier)?.checked_div(divisor)?;

    // Check if result fits in u128
    if value > U256::from(u128::MAX) {
        return None;
    }

    Some(value.to())
}

/// Convert native gas cost in wei to USD value in e8 precision.
///
/// Requires the native token price in USD e8.
pub fn native_gas_to_usd_e8(
    gas_cost_wei: U256,
    native_price_usd_e8: u64,
    native_decimals: u8,
) -> Option<u128> {
    if native_price_usd_e8 == 0 {
        return None;
    }

    let price_e8 = U256::from(native_price_usd_e8);
    let divisor = U256::from(10u64).pow(U256::from(native_decimals));

    // gas_cost_wei * native_price_usd_e8 / 10^native_decimals
    let value = gas_cost_wei.checked_mul(price_e8)?.checked_div(divisor)?;

    // Check if result fits in u128
    if value > U256::from(u128::MAX) {
        return None;
    }

    Some(value.to())
}

/// Calculate raw token profit before gas deduction.
///
/// For self-funded: gross_profit_raw = output_amount - input_amount
/// For flash loan: net_profit_before_gas_raw = output_amount - input_amount - flash_fee
pub fn calculate_net_profit_before_gas_raw(
    input_amount: u128,
    output_amount: u128,
    flash_fee_raw: u128,
) -> i128 {
    let gross = output_amount as i128 - input_amount as i128;
    gross - flash_fee_raw as i128
}

/// Calculate contract minProfit in raw token units.
///
/// Uses integer basis points to avoid floating point conversion.
///
/// Formula: floor(net_profit_before_gas_raw * min_profit_realization_bps / 10_000)
///
/// Returns 0 if net profit is negative or zero.
pub fn calculate_contract_min_profit_raw(
    net_profit_before_gas_raw: i128,
    min_profit_realization_bps: u32,
) -> u128 {
    if net_profit_before_gas_raw <= 0 {
        return 0;
    }

    let numerator = net_profit_before_gas_raw.saturating_mul(min_profit_realization_bps as i128);
    let result = numerator / 10_000;

    result.max(0) as u128
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TokenBehavior;

    fn make_token(decimals: u8, price_e8: Option<u64>) -> TokenInfo {
        TokenInfo {
            address: Default::default(),
            symbol: "TEST".to_string(),
            decimals,
            is_stable: false,
            is_cycle_anchor: false,
            flash_loan_enabled: false,
            allow_self_funded: false,
            behavior: TokenBehavior::default(),
            manual_price_usd_e8: price_e8,
            max_position_usd_e8: None,
            max_flash_loan_usd_e8: None,
        }
    }

    #[test]
    fn test_usdc_to_usd() {
        let token = make_token(6, Some(100_000_000)); // $1.00
        let amount_raw = 1_000_000u128; // 1 USDC
        let usd = amount_to_usd_e8(amount_raw, &token).unwrap();
        assert_eq!(usd, 100_000_000); // $1.00 in e8
    }

    #[test]
    fn test_weth_to_usd() {
        let token = make_token(18, Some(300_000_000_000u64)); // $3000
        let amount_raw = 1_000_000_000_000_000_000u128; // 1 WETH
        let usd = amount_to_usd_e8(amount_raw, &token).unwrap();
        assert_eq!(usd, 300_000_000_000u128); // $3000 in e8
    }

    #[test]
    fn test_usd_to_usdc() {
        let token = make_token(6, Some(100_000_000)); // $1.00
        let usd_e8 = 10_000_000u128; // $0.10
        let amount = usd_e8_to_amount(usd_e8, &token).unwrap();
        assert_eq!(amount, 100_000); // 0.1 USDC
    }

    #[test]
    fn test_usd_to_weth() {
        let token = make_token(18, Some(300_000_000_000u64)); // $3000
        let usd_e8 = 300_000_000_000u128; // $3000
        let amount = usd_e8_to_amount(usd_e8, &token).unwrap();
        assert_eq!(amount, 1_000_000_000_000_000_000u128); // 1 WETH
    }

    #[test]
    fn test_no_price_returns_none() {
        let token = make_token(18, None);
        assert!(amount_to_usd_e8(1_000_000_000_000_000_000u128, &token).is_none());
        assert!(usd_e8_to_amount(100_000_000u128, &token).is_none());
    }

    #[test]
    fn test_contract_min_profit() {
        // 90% of 1000 = 900
        let profit = calculate_contract_min_profit_raw(1000, 9000);
        assert_eq!(profit, 900);
    }

    #[test]
    fn test_contract_min_profit_negative() {
        let profit = calculate_contract_min_profit_raw(-100, 9000);
        assert_eq!(profit, 0);
    }

    #[test]
    fn test_net_profit_self_funded() {
        let net = calculate_net_profit_before_gas_raw(1_000_000, 1_100_000, 0);
        assert_eq!(net, 100_000);
    }

    #[test]
    fn test_net_profit_flash_loan() {
        let flash_fee = 900; // 0.09%
        let net = calculate_net_profit_before_gas_raw(1_000_000, 1_100_000, flash_fee);
        assert_eq!(net, 99_100);
    }
}
