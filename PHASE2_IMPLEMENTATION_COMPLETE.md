# Phase 2 Implementation Complete: Production-Safe Flash-Loan Anchors

## Summary

Phase 2 has been successfully implemented, making non-stable anchors (WETH, DAI, WMATIC, etc.) safe to enable by fixing valuation, token-aware limits, flash-loan eligibility, contract min-profit units, and depeg policy.

## Key Changes

### 1. USD Valuation System (`src/risk/valuation.rs`)
**New module** with helper functions for token/USD conversions:
- `amount_to_usd_e8()`: Convert raw token amount to USD (e8 precision)
- `usd_e8_to_amount()`: Convert USD to raw token amount  
- `native_gas_to_usd_e8()`: Convert gas cost in wei to USD
- `calculate_net_profit_before_gas_raw()`: Calculate profit before gas
- `calculate_contract_min_profit_raw()`: Calculate contract minProfit using integer arithmetic

**Features:**
- Uses U256 internally to prevent overflow
- Works with any token decimals (6, 18, etc.)
- Requires token price configuration (`manual_price_usd_e8`)

### 2. Enhanced ExactPlan Structure (`src/types.rs`)
**Added new fields to track both raw and USD values:**
```rust
// Raw input-token units (for contract)
gross_profit_raw: i128
flash_fee_raw: u128
net_profit_before_gas_raw: i128
contract_min_profit_raw: u128

// USD e8 decision units (for off-chain risk checks)
input_value_usd_e8: u128
gross_profit_usd_e8: i128
flash_fee_usd_e8: i128
gas_cost_usd_e8: i128
net_profit_usd_e8: i128

// Legacy field for backward compatibility
expected_profit: i128
```

**Added CapitalChoice struct:**
```rust
pub struct CapitalChoice {
    source: CapitalSource,
    flash_fee_raw: u128,
    net_profit_before_gas_raw: i128,
}
```

**Added cycle_key to CandidatePath** for better deduplication

### 3. Configuration Updates (`src/config.rs`)
**New RiskSettings fields:**
```rust
min_net_profit_usd_e8: u128       // $0.10 default
min_trade_usd_e8: u128            // $10.00 default
max_position_usd_e8: u128         // $2,000 default
max_flash_loan_usd_e8: u128       // $10,000 default
daily_loss_limit_usd_e8: u128     // $500 default
min_profit_realization_bps: u32   // 9000 (90%) default
```

**Backward Compatibility:**
- Old raw fields still supported for migration
- USD fields default to sensible values
- Can override via environment variables

### 4. Quantity Search (`src/router/quantity_search.rs`)
**Now uses USD-aware caps:**
- Converts `max_position_usd_e8` to raw token units
- Calculates `min_trade_usd_e8` in raw units
- Per-token USD caps supported
- Falls back to legacy raw caps if no price configured

**Example:**
```
WETH ($3000, 18 decimals):
  max_position_usd_e8 = $2000
  → max_position_raw = 0.666 WETH

USDC ($1, 6 decimals):
  max_position_usd_e8 = $2000
  → max_position_raw = 2,000 USDC
```

### 5. Capital Selection (`src/execution/capital_selector.rs`)
**Completely redesigned:**
- Returns `Option<CapitalChoice>` instead of tuple
- Proper flash loan eligibility checks:
  - `token.flash_loan_enabled` must be true
  - Per-token flash cap check (USD)
  - Premium lookup failure → reject (not fallback to zero)
- Self-funded check requires `token.allow_self_funded` AND sufficient balance
- Returns `None` when no valid capital source (prevents invalid simulations)

### 6. Validator (`src/execution/validator.rs`)
**Major rewrite with comprehensive USD profitability checks:**

**Flow:**
1. Capital selection (self-funded vs flash loan)
2. Set `flash_fee_raw` and `net_profit_before_gas_raw`
3. Calculate `contract_min_profit_raw`
4. Estimate gas
5. **Convert all values to USD**
6. **Check `net_profit_usd_e8 >= min_net_profit_usd_e8`**
7. Simulation
8. Return executable plan

**Key Features:**
- Requires `GraphSnapshot` for token metadata
- Converts gas cost to USD using native token price
- Rejects if USD profit below threshold
- Proper debug logging for rejection reasons

### 7. Transaction Builder (`src/execution/tx_builder.rs`)
**Fixed contract minProfit:**
- OLD: `minProfit = (expected_profit as f64 * 0.90) as u128`
- NEW: `minProfit = contract_min_profit_raw` (integer arithmetic)

**Benefits:**
- No floating point conversion
- Correct raw token units
- Works with any token decimals

### 8. Risk Manager (`src/risk/limits.rs`)
**USD-denominated checks:**

- `input_value_usd_e8 <= max_position_usd_e8`
- `flash_value_usd_e8 <= max_flash_loan_usd_e8`
- `net_profit_usd_e8 >= min_profit_usd_e8`
- `daily_pnl_usd_e8 > daily_loss_limit_usd_e8`

**Important:**
- Raw amount backstops were removed from execution risk checks because a single raw cap cannot safely compare 6-decimal and 18-decimal anchors.
- Legacy raw config fields remain available only as migration/fallback inputs where a raw fallback is explicitly needed.

**Daily PnL now tracks USD values** for multi-token compatibility

### 9. Depeg Guard (`src/engine.rs`)
**Moved from global to candidate-level:**

**OLD:**
```rust
if !depeg_guard.stable_routes_allowed() {
    return Ok(()); // Blocks ALL routes
}
```

**NEW:**
```rust
for candidate in candidates {
    if !candidate_stables_healthy(&candidate, depeg_guard) {
        continue; // Only blocks routes touching unhealthy stables
    }
    // Process candidate...
}
```

**Impact:**
- WETH→cbETH→WETH can proceed even if USDC is unhealthy
- USDC→WETH→USDC blocked if USDC unhealthy
- Route-scoped filtering
- Non-stable manually priced tokens are not compared against the stable depeg cutoff

### 10. Enhanced Logging
**Simulation logs now include:**
```
anchor_symbol
input_token
gross_profit_raw
gross_profit_usd_e8
flash_fee_raw
flash_fee_usd_e8
gas_cost_usd_e8
net_profit_usd_e8
contract_min_profit_raw
```

**Benefits:**
- Clear visibility into USD profitability
- Easy debugging
- Multi-token comparison possible

### 11. Flash Loan Handling
**CapitalSelector now enforces:**
- Premium lookup errors reject the flash-loan choice
- Per-token flash caps are checked in USD e8
- Tokens without USD valuation cannot be selected for flash-loan sizing
- Self-funded and flash-loan choices are compared in the same input token's raw units before gas

### 12. Test Updates
**Updated all test files:**
- `tests/detector_tests.rs`: Added new USD fields to RiskSettings
- `tests/risk_tests.rs`: Added new USD fields
- Existing tests prove backward compatibility with USDC/USDT
- `detector_can_start_from_non_stable_cycle_anchor` test validates WETH anchor

## Configuration Guidance

### Base Chain Example
```toml
[[tokens]]
symbol = "WETH"
address_env = "BASE_WETH"
decimals = 18
is_stable = false
is_cycle_anchor = true
flash_loan_enabled = true
allow_self_funded = false
price_env = "BASE_WETH_PRICE_E8"  # e.g., 300_000_000_000 for $3000
max_position_usd_e8 = 200_000_000_000  # $2000
max_flash_loan_usd_e8 = 1_000_000_000_000  # $10000
```

### Required Environment Variables
```bash
# Token prices (e8 precision)
BASE_WETH_PRICE_E8=300000000000  # $3000
BASE_DAI_PRICE_E8=100000000      # $1.00

# Risk thresholds (optional, have defaults)
BASE_MIN_NET_PROFIT_USD_E8=10000000      # $0.10
BASE_MAX_POSITION_USD_E8=200000000000    # $2000
BASE_MAX_FLASH_LOAN_USD_E8=1000000000000 # $10000
```

## Breaking Changes

### ExactPlan Structure
- New fields added, old `expected_profit` kept for compatibility
- Code creating ExactPlan must fill new fields

### Validator.prepare() Signature
- OLD: `prepare(plan: ExactPlan)`
- NEW: `prepare(plan: ExactPlan, snapshot: &GraphSnapshot)`
- Requires snapshot for token metadata

### RiskManager.mark_finalized()
- OLD: `mark_finalized(realized_profit: i128)` (raw units)
- NEW: `mark_finalized(realized_profit_usd_e8: i128)` (USD e8)

### CapitalSelector.choose()
- OLD: `choose(plan: &ExactPlan) -> Result<Option<(CapitalSource, i128)>>`
- NEW: `choose(plan: &ExactPlan, token: &TokenInfo) -> Result<Option<CapitalChoice>>`
- Requires token metadata

## Migration Path

### Step 1: Deploy with Current Config
- Existing USDC/USDT config works unchanged
- USD defaults provide reasonable behavior

### Step 2: Add Token Prices
```bash
BASE_WETH_PRICE_E8=300000000000
POLYGON_WMATIC_PRICE_E8=100000000
```

### Step 3: Enable New Anchors
```toml
[[tokens]]
symbol = "WETH"
is_cycle_anchor = true
flash_loan_enabled = true
price_env = "BASE_WETH_PRICE_E8"
```

### Step 4: Test with `--simulate-only`
```bash
cargo run -- --chain base --simulate-only
```

### Step 5: Monitor Logs
Watch for:
```
anchor_symbol=WETH
net_profit_usd_e8=...
gross_profit_usd_e8=...
```

### Step 6: Enable Gradually
Start with small caps:
```toml
max_position_usd_e8 = 50_000_000_000  # $500
```

Increase after validation.

## Testing Checklist

- [x] Unit tests pass (valuation.rs)
- [x] Detector tests pass (detector_tests.rs)
- [x] Risk tests pass (risk_tests.rs)
- [x] Backward compatibility with USDC/USDT maintained
- [x] Non-stable anchors (WETH) can produce candidates
- [ ] Fork test: WETH flash loan cycle on Base
- [ ] Fork test: WMATIC flash loan cycle on Polygon
- [ ] Live test: Small size simulation
- [ ] Live test: Monitor USD profitability accuracy

## Known Limitations

1. **Native Token Price**: Currently hardcoded
   - ETH: $3000 default
   - MATIC: $1.00 default
   - Should be configurable in future

2. **Aave Reserve Availability**: Not checked
   - Future enhancement: Query Aave to confirm token is borrowable

3. **Oracle Prices**: Not implemented
   - Uses manual config prices only
   - Consider Chainlink integration later

## Next Steps

1. **Run simulation tests** with new anchors enabled
2. **Fork testing** for WETH/WMATIC flash loan cycles
3. **Monitor gas costs** vs USD profit accuracy
4. **Consider oracle integration** for dynamic prices
5. **Add Aave availability checks** before flash loan selection

## Files Changed

### New Files
- `src/risk/valuation.rs`

### Modified Files
- `src/types.rs` (ExactPlan, CapitalChoice, CandidatePath)
- `src/config.rs` (RiskSettings, FileConfig)
- `src/router/mod.rs` (ExactPlan creation)
- `src/router/quantity_search.rs` (USD-aware ladder)
- `src/execution/capital_selector.rs` (CapitalChoice, flash checks)
- `src/execution/validator.rs` (USD profitability, snapshot param)
- `src/execution/tx_builder.rs` (contract_min_profit_raw)
- `src/risk/limits.rs` (USD checks)
- `src/engine.rs` (candidate-level depeg, enhanced logging)
- `src/detector/path_finder.rs` (cycle_key field)
- `tests/detector_tests.rs` (new RiskSettings fields)
- `tests/risk_tests.rs` (new RiskSettings fields)

## Conclusion

Phase 2 is **complete and ready for testing**. The implementation:

✅ Separates raw contract units from USD decision units  
✅ Uses token-aware quantity limits  
✅ Enforces USD-normalized profitability  
✅ Properly handles flash loan eligibility  
✅ Route-scoped depeg filtering  
✅ Backward compatible with existing USDC/USDT config  
✅ Comprehensive logging for debugging  
✅ Test coverage for flexible anchors  

The bot can now safely support WETH, DAI, WMATIC, and other non-stable anchors while maintaining correct risk accounting across different token decimals and values.
