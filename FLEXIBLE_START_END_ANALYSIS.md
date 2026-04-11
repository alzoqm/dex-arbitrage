# DEX N-Hop Arbitrage: Flexible Start/End Token Analysis

## 1. Current Architecture Summary

### Pipeline Overview

```
Discovery -> Graph Build -> Event Stream -> Detection -> Routing -> Execution
```

1. **Discovery** (`src/discovery/`): Scans all DEX factories/registries, fetches pool states, builds initial graph
2. **Graph Model** (`src/graph/`): Weighted directed multigraph where vertices = tokens, edges = pool swap directions
3. **Detection** (`src/detector/`): DFS from stable tokens to find negative-weight cycles
4. **Routing** (`src/router/`): Exact quotes + split optimization + quantity ladder search
5. **Execution** (`src/execution/`): Capital selection (self-funded vs flash loan) -> tx build -> submit

### Current Token Constraint: Hard-coded Stable-only Start/End

The system currently requires **all arbitrage paths to start AND end at a "stable" token** (USDC or USDT). This constraint is enforced at **6 layers**:

```
Layer 1: config/base.toml, config/polygon.toml
         is_stable = true   (only USDC, USDT)

Layer 2: src/config.rs:277  Settings::stable_tokens()
         filters by is_stable flag

Layer 3: src/graph/model.rs:39-43  GraphSnapshot::build()
         populates stable_token_indices from is_stable tokens

Layer 4: src/graph/distance_cache.rs  DistanceCache::recompute()
         BFS reachability FROM and TO stable_token_indices only

Layer 5: src/detector/path_finder.rs:44  PathFinder::find_candidates()
         DFS starts only from stable_token_indices

Layer 6: src/risk/depeg_guard.rs  DepegGuard::stable_routes_allowed()
         blocks ALL routes if any stable token depegs
```

### Flash Loan Flow (Current)

```
CapitalSelector::choose()
  ├── Self-funded: output - input >= profit threshold
  └── Flash Loan:  output - input - flash_fee >= profit threshold
        (uses Aave V3 flashLoanSimple)
```

Flash loans are already implemented and work with any input token - the `flashLoanSimple()` function accepts any ERC-20 asset. The **only** reason start/end is limited to USDC/USDT is the detector design, not a flash loan limitation.

---

## 2. Why Stable-Only Was Designed This Way

The original design constrains to stablecoins because:

1. **Profit measurement is unambiguous**: `output - input` in the same stablecoin = clear USD profit
2. **Self-funded capital is held in stables**: executor contract holds USDC/USDT as working capital
3. **Depeg guard makes sense**: if USDC/USDT depegs, stop all trading
4. **Simpler risk management**: one price reference (USD) for all profit calculations

---

## 3. Why Flash Loans Remove the Stable-Only Constraint

With flash loans, the bot borrows the input token on-chain, executes the swap path, and repays in the same token. Key insight:

> **Flash loans do not require the executor to hold the input token beforehand.** The capital is borrowed and returned atomically within one transaction. Therefore, ANY token with sufficient Aave V3 liquidity can be a start/end token.

This means arbitrage opportunities like:
- `WETH -> USDC -> DAI -> WETH` (WETH cycle)
- `DAI -> WETH -> USDC -> DAI` (DAI cycle)
- `WMATIC -> WETH -> USDC -> WMATIC` (WMATIC cycle on Polygon)

are all valid flash loan candidates, but are **currently invisible** to the detector because it only starts DFS from stable tokens.

---

## 4. Required Changes to Support Flexible Start/End Tokens

### 4.1 Configuration Changes

**File: `config/base.toml` and `config/polygon.toml`**

Add a new concept: `is_cycle_anchor` (or expand the meaning of `is_stable`).

**Option A: Rename `is_stable` to `is_cycle_anchor`**

Replace `is_stable = true/false` with `is_cycle_anchor = true/false` on each token. Any token with `is_cycle_anchor = true` can serve as a cycle start/end point.

```toml
[[tokens]]
symbol = "USDC"
is_cycle_anchor = true    # can start/end cycles

[[tokens]]
symbol = "USDT"
is_cycle_anchor = true

[[tokens]]
symbol = "WETH"
is_cycle_anchor = true    # NEW: WETH can now start/end cycles

[[tokens]]
symbol = "DAI"
is_cycle_anchor = true    # NEW: DAI can now start/end cycles
```

**Option B: Add separate `is_cycle_anchor` field, keep `is_stable` for depeg guard**

This is the cleaner approach. `is_stable` remains for the depeg guard, `is_cycle_anchor` controls path finding.

```toml
[[tokens]]
symbol = "USDC"
is_stable = true
is_cycle_anchor = true

[[tokens]]
symbol = "WETH"
is_stable = false
is_cycle_anchor = true    # can start/end cycles, but not subject to depeg guard
```

### 4.2 Code Changes (7 files)

#### File 1: `src/config.rs`

**Changes:**
- Add `is_cycle_anchor: bool` to `TokenConfig` struct (line 65-71)
- Add `is_cycle_anchor` to `FileTokenConfig` (line 110-117)
- Add `cycle_anchor_tokens()` method similar to `stable_tokens()`
- Parse `is_cycle_anchor` from TOML (default to value of `is_stable` for backward compat)

```rust
// In TokenConfig:
pub is_cycle_anchor: bool,

// In FileTokenConfig:
is_cycle_anchor: Option<bool>,  // default: same as is_stable

// New method:
pub fn cycle_anchor_tokens(&self) -> Vec<Address> {
    self.tokens
        .iter()
        .filter(|token| token.is_cycle_anchor)
        .map(|token| token.address)
        .collect()
}
```

#### File 2: `src/types.rs`

**Changes:**
- Add `is_cycle_anchor: bool` to `TokenInfo` struct (line 162-170)

```rust
pub struct TokenInfo {
    pub address: Address,
    pub symbol: String,
    pub decimals: u8,
    pub is_stable: bool,
    pub is_cycle_anchor: bool,    // NEW
    pub behavior: TokenBehavior,
    pub manual_price_usd_e8: Option<u64>,
}
```

#### File 3: `src/graph/model.rs`

**Changes:**
- Rename or add `cycle_anchor_indices` alongside `stable_token_indices` (line 19)
- Populate `cycle_anchor_indices` from `is_cycle_anchor` instead of `is_stable` (line 39-43)

```rust
pub struct GraphSnapshot {
    // ...
    pub stable_token_indices: Vec<usize>,      // keep for depeg guard
    pub cycle_anchor_indices: Vec<usize>,      // NEW: for path finding
}

// In build():
let cycle_anchor_indices = tokens
    .iter()
    .enumerate()
    .filter_map(|(idx, token)| token.is_cycle_anchor.then_some(idx))
    .collect::<Vec<_>>();
```

#### File 4: `src/graph/distance_cache.rs`

**Changes:**
- Use `cycle_anchor_indices` instead of `stable_token_indices` for reachability BFS (line 18-19, 32-33)

```rust
// Change:
for &stable_idx in &snapshot.stable_token_indices {
// To:
for &anchor_idx in &snapshot.cycle_anchor_indices {
```

#### File 5: `src/detector/path_finder.rs`

**Changes:**
- Use `cycle_anchor_indices` instead of `stable_token_indices` for DFS start points (line 44)

```rust
// Change:
for &stable_idx in &snapshot.stable_token_indices {
// To:
for &anchor_idx in &snapshot.cycle_anchor_indices {
```

#### File 6: `src/risk/depeg_guard.rs`

**Changes:**
- Keep using `is_stable` (NOT `is_cycle_anchor`). The depeg guard should only watch stablecoins.
- No changes needed if we keep `stable_tokens` separate from cycle anchors.

#### File 7: `src/execution/capital_selector.rs`

**Changes:**
- When the cycle anchor is NOT a stable token and executor has no balance, **force flash loan** (not fallback to self-funded with 0 balance).
- Adjust profit calculation: for non-stable anchors, the profit is still `output - input` in anchor token units, but needs USD conversion for `min_net_profit` comparison.

```rust
pub async fn choose(&self, plan: &ExactPlan) -> Result<(CapitalSource, i128)> {
    let self_balance = self.executor_balance(plan.input_token).await.unwrap_or(0);
    let flash_fee = self.flash.fee_for_amount(plan.input_amount).await.unwrap_or(0);

    let self_profit = plan.output_amount as i128 - plan.input_amount as i128;
    let flash_profit = self_profit - flash_fee as i128;

    // If self-funded is viable and more profitable, use it
    if self_balance >= plan.input_amount && self_profit >= flash_profit {
        Ok((CapitalSource::SelfFunded, self_profit))
    } else if self.settings.contracts.aave_pool.is_some()
        && plan.input_amount <= self.settings.risk.max_flash_loan
    {
        // Flash loan works for ANY token, not just stables
        Ok((CapitalSource::FlashLoan, flash_profit))
    } else {
        Ok((CapitalSource::SelfFunded, self_profit))
    }
}
```

This logic already works correctly for any token. No change needed here functionally.

#### File 8: `src/execution/validator.rs`

**Changes:**
- Profit threshold check (`min_net_profit`) currently compares raw token units. For non-stable anchors (e.g., WETH with 18 decimals), the raw profit number means something different than for USDC (6 decimals).
- **Option A**: Convert profit to USD using `manual_price_usd_e8` before comparing to `min_net_profit`.
- **Option B**: Add per-token `min_profit_override` in config.
- **Option C**: Define `min_net_profit` in USD terms and convert at validation time.

```rust
// Example: convert raw profit to USD for threshold comparison
fn profit_in_usd(profit_raw: i128, token: &TokenInfo) -> i128 {
    let price_e8 = token.manual_price_usd_e8.unwrap_or(0) as i128;
    // profit_usd = profit_raw * price_e8 / 10^decimals / 10^8
    let decimals_factor = 10i128.pow(token.decimals as u32);
    profit_raw * price_e8 / decimals_factor / 100_000_000
}
```

### 4.3 Smart Contract Changes

**File: `contracts/ArbitrageExecutor.sol`**

The contract already handles any ERC-20 token:
- `executeFlashLoan()` calls `flashLoanSimple(asset, amount, ...)` which works for any Aave-listed asset
- `executeSelfFunded()` checks `IERC20(inputToken).balanceOf(address(this))`
- The cyclic check `require(currentToken == params.inputToken, "NOT_CYCLIC")` is token-agnostic

**No contract changes needed.** The Solidity code is already token-agnostic.

---

## 5. Risk Considerations

### 5.1 Increased DFS Search Space

More anchor tokens = more DFS starting points = more candidates to evaluate.

Current: 2 anchors (USDC, USDT) -> 2 DFS trees
With WETH+DAI added: 4 anchors -> 4 DFS trees

**Mitigation**: The `max_candidates = 128` cap and `max_branching = 8` already limit the search. Additional anchors add at most linear overhead to detection.

### 5.2 Profit Measurement in Non-USD Terms

For a WETH-cycle path (e.g., `WETH -> USDC -> DAI -> WETH`), profit is measured in WETH wei (18 decimals), not USDC units (6 decimals). A profit of 1,000,000 raw units means:
- USDC: $1.00 profit
- WETH: ~0.000000000001 WETH = negligible

**Required**: Convert all profit checks to a common unit (USD) using `manual_price_usd_e8`.

### 5.3 Flash Loan Availability

Not all tokens have deep Aave V3 liquidity. If Aave doesn't list a token, `flashLoanSimple()` will revert.

**Mitigation**: In `CapitalSelector::choose()`, if `aave_pool` is configured but the flash loan for that specific token fails at simulation, the validator's `eth_call` simulation will catch it before submission.

### 5.4 Depeg Guard Scope

The depeg guard should **only** watch stablecoins. If WETH is an anchor but USDC depegs, we should still allow WETH-cycle arbitrage (or not, depending on risk tolerance).

**Current behavior**: depeg guard blocks ALL routes if any stable depegs.
**Recommended**: Keep this behavior. A USDC depeg affects all DEX pricing regardless of the cycle anchor.

---

## 6. Summary of Changes

| File | Change | Complexity |
|------|--------|------------|
| `config/base.toml` | Add `is_cycle_anchor` field to tokens | Trivial |
| `config/polygon.toml` | Add `is_cycle_anchor` field to tokens | Trivial |
| `src/config.rs` | Parse `is_cycle_anchor`, add `cycle_anchor_tokens()` | Low |
| `src/types.rs` | Add `is_cycle_anchor` to `TokenInfo` | Trivial |
| `src/graph/model.rs` | Add `cycle_anchor_indices`, populate from `is_cycle_anchor` | Low |
| `src/graph/distance_cache.rs` | Use `cycle_anchor_indices` instead of `stable_token_indices` | Trivial (2 line changes) |
| `src/detector/path_finder.rs` | Use `cycle_anchor_indices` instead of `stable_token_indices` | Trivial (1 line change) |
| `src/execution/validator.rs` | USD-denominated profit check for non-stable anchors | Medium |
| `src/risk/depeg_guard.rs` | No change (keeps watching `is_stable`) | None |
| `contracts/ArbitrageExecutor.sol` | No change (already token-agnostic) | None |

**Total effort: ~30 lines of Rust changed + 2 TOML config updates.**

---

## 7. Recommended Implementation Order

1. Add `is_cycle_anchor` to config types and TOML files
2. Add `cycle_anchor_indices` to `GraphSnapshot`
3. Switch `DistanceCache` and `PathFinder` to use `cycle_anchor_indices`
4. Add USD-denominated profit conversion in validator
5. Test with WETH as additional anchor on Base testnet
6. Gradually add more anchors (DAI, cbETH, WMATIC)
