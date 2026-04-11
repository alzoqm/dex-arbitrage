# Flash Loan Flexible Start/End Token Plan

## 1. Executive Summary

The current project implements the `revised_dex_arb_design.md` stablecoin-centered N-hop cyclic arbitrage flow:

```text
Discovery -> GraphSnapshot -> DistanceCache -> Detector -> Router -> Validator -> Executor -> Submitter
```

The current route constraint is not hardcoded directly as string checks against `USDC` and `USDT`. It is implemented through the token config field `is_stable = true`, and the chain configs currently mark only `USDC` and `USDT` as stable anchors. Those stable tokens become `GraphSnapshot.stable_token_indices`, and the detector only starts DFS from those indices.

With flash loans, the bot does not need to own the start asset before execution. However, Aave V3 `flashLoanSimple` still borrows one ERC-20 asset and must repay that same asset plus premium in the same transaction. Therefore the correct new invariant is:

```text
Old invariant:
  start_token and end_token must be USDC or USDT.

New invariant:
  start_token and end_token must still be the same token,
  but that token can be any configured, risk-approved, flash-loan-eligible cycle anchor.
```

This means the project should support routes such as:

```text
WETH -> USDC -> DAI -> WETH
DAI -> WETH -> USDC -> DAI
WMATIC -> WETH -> USDC -> WMATIC
```

The detector can be generalized fairly cleanly, but a safe production fix must also update risk and valuation. The current `min_net_profit`, `max_position`, and `max_flash_loan` are raw token-unit values that implicitly fit 6-decimal stablecoins. They are not safe or useful for 18-decimal assets like WETH, cbETH, DAI, or WMATIC.

The recommended fix is to split the current overloaded `is_stable` concept into separate concerns:

```text
is_stable          -> depeg guard and stable-specific risk
is_cycle_anchor    -> can be used as a cyclic start/end token
flash_loan_enabled -> can be borrowed for flash-loan execution
```

Then move off-chain profitability and risk thresholds to USD-normalized accounting while keeping contract-level `minProfit` in the borrowed anchor token's raw units.

## 2. Current Architecture

### 2.1 Runtime Entry

`src/main.rs` parses `--chain base|polygon`, loads `.env`, creates `Settings`, installs logging/metrics, and calls `engine::run_chain`.

`src/engine.rs` owns the live pipeline:

1. Validate runtime prerequisites.
2. Create RPC clients.
3. Create discovery, detector, router, validator, submitter, risk manager, depeg guard, and nonce manager.
4. Bootstrap pools and the first immutable graph snapshot.
5. Process the initial full edge set.
6. Poll events, refresh changed pools, publish a new snapshot, and process changed edges.

Important logic:

```text
engine::process_refresh
  if !depeg_guard.stable_routes_allowed(): return
  distance_cache = DistanceCache::recompute(snapshot)
  candidates = detector.detect(snapshot, changed_edges, distance_cache)
  sort candidates by screening_score_q32
  for first 32 candidates:
    router.search_best_plan
    validator.prepare
    risk.pre_trade_check
    submit if not simulate_only
```

### 2.2 Configuration

The chain TOML files define:

```text
chain policy:
  max_hops
  screening_margin_bps
  min_net_profit_default
  max_position_default
  max_flash_loan_default
  gas policy
  depeg cutoff

tokens:
  symbol
  address_env
  decimals
  is_stable
  price_env

dexes:
  name
  amm
  discovery mode
  factory/registry/vault/quoter envs
  fee_ppm
  start_block
  enabled
```

Current Base tokens:

```text
USDC  is_stable = true
USDT  is_stable = true
WETH  is_stable = false
DAI   is_stable = false
cbETH is_stable = false
```

Current Polygon tokens:

```text
USDC   is_stable = true
USDT   is_stable = true
WMATIC is_stable = false
WETH   is_stable = false
DAI    is_stable = false
```

So the code does not literally say "only USDC and USDT", but the config and stable-index plumbing make that the effective behavior.

### 2.3 Discovery

`src/discovery/mod.rs` converts configured `TokenConfig` entries into runtime `TokenInfo` entries and builds the first `GraphSnapshot`.

`src/discovery/factory_scanner.rs` scans enabled DEXes:

```text
V2-like       -> allPairsLength/allPairs
V3-like       -> PoolCreated logs
Curve         -> registry pool_count/pool_list
Balancer      -> vault PoolRegistered logs
```

`src/discovery/pool_fetcher.rs` fetches pool metadata and state. It rejects pools unless every pool token exists in `settings.token_map()`. This matters for flexible anchors:

```text
A token can only appear in routes if it is configured in config/base.toml or config/polygon.toml
and its address env var is set.
```

Adding an anchor is not enough if the token is absent from the configured token universe.

`src/discovery/admission.rs` applies minimal admission checks:

```text
pool has at least 2 tokens
all tokens are configured
pool is not excluded
pool is not paused/quarantined
estimated liquidity floor passes
```

### 2.4 Graph Model

`src/graph/model.rs` builds a directed multigraph:

```text
vertex = configured token
edge   = one swap direction through one pool
```

The snapshot contains:

```text
tokens
token_to_index
stable_token_indices
adjacency
reverse_adj
pools
pool_to_edges
pair_to_edges
```

The current start/end constraint is created here:

```rust
let stable_token_indices = tokens
    .iter()
    .enumerate()
    .filter_map(|(idx, token)| token.is_stable.then_some(idx))
    .collect::<Vec<_>>();
```

Every token with `is_stable = true` becomes a DFS root. Since the configs only mark `USDC` and `USDT`, only those two can start/end cycles.

### 2.5 Distance Cache

`src/graph/distance_cache.rs` computes:

```text
reachable_from_stable
reachable_to_stable
```

Both are seeded from `snapshot.stable_token_indices`. The detector currently uses `reachable_to_stable` to prune paths that cannot return to a stable token.

Important detail: `reachable_from_stable` is computed but is not currently used by `PathFinder`. The DFS already starts from stable indices, so the practical pruning dependency is `reachable_to_stable`.

For flexible anchors, these fields should become anchor-based reachability, not stable-based reachability:

```text
reachable_from_anchor
reachable_to_anchor
```

### 2.6 Detector

`src/detector/path_finder.rs` is the main enforcement point.

Current behavior:

```rust
for &stable_idx in &snapshot.stable_token_indices {
    let start_addr = snapshot.tokens[stable_idx].address;
    let start_symbol = snapshot.tokens[stable_idx].symbol.clone();
    visited.insert(stable_idx);
    self.dfs(... stable_idx, stable_idx, start_addr, &start_symbol, ...);
}
```

During DFS:

```text
stop if depth >= max_hops
rank outgoing edges by health, liquidity, and spot weight
skip unhealthy pools
skip tokens not reachable back to stable
avoid revisiting intermediate tokens
require the path touches at least one changed edge
require the spot score to pass screening_threshold_q32
emit CandidatePath { start_token, start_symbol, path }
```

The detector emits cyclic candidates only when the final edge returns to the same `start_idx`. That cyclic invariant is correct for flash loans and should remain.

### 2.7 Router

`src/router/mod.rs` receives a `CandidatePath`.

For each candidate, it searches input sizes using `QuantitySearcher`, then quotes each hop through `SplitOptimizer`:

```text
for amount in ladder:
  next_amount = amount
  for hop in candidate.path:
    choose best/split route for hop.from -> hop.to
    next_amount = hop_output
  expected_profit = final_amount - input_amount
```

The router then creates:

```rust
ExactPlan {
    input_token: candidate.start_token,
    output_token: candidate.start_token,
    input_amount,
    output_amount: next_amount,
    expected_profit,
    capital_source: SelfFunded,
    hops,
}
```

This route model is already token-agnostic as long as the candidate is cyclic. The important limitation is not the router's path shape. The limitation is its raw-token-unit optimization and the upstream anchor selection.

### 2.8 Quantity Search

`src/router/quantity_search.rs` derives a starting unit from token decimals:

```rust
unit = 10u128.saturating_pow(decimals.saturating_sub(2) as u32).max(1)
```

Then it doubles until `settings.risk.max_position`.

This happens in raw token units. That is dangerous for flexible anchors:

```text
max_position_default = 2_000_000_000

USDC decimals 6:
  2_000_000_000 raw = 2,000 USDC

WETH decimals 18:
  2_000_000_000 raw = 0.000000002 WETH
```

For WETH, the ladder starts at `10^16` raw units, which is `0.01 WETH`. That already exceeds the current global raw `max_position`, so the later risk check rejects it. Flexible anchors require USD-based or per-token raw size limits.

### 2.9 Exact Quoter And Split Optimizer

`src/router/exact_quoter.rs` supports:

```text
Uniswap V2-like     -> local exact-in constant product
Uniswap V3-like     -> protocol quoter if configured, fallback otherwise
Curve plain         -> get_dy_underlying/get_dy
Balancer weighted   -> local fallback math
```

`src/router/split_optimizer.rs` handles multiple pools for the same pair by assigning input slices to the best marginal pool and materializing `SplitPlan`s.

These modules work with token addresses and raw amounts, not symbols. They do not care whether the start token is USDC, USDT, WETH, or another configured ERC-20. They do need correctly scaled input amounts.

### 2.10 Validator

`src/execution/validator.rs` performs:

```text
capital selection
profit threshold check
deadline
calldata build
gas estimate
gas ceiling check
eth_call/custom simulation
ExecutablePlan creation
```

Current threshold check:

```rust
if plan.expected_profit < self.settings.risk.min_net_profit {
    return Ok(None);
}
```

This compares raw token units to a single global raw threshold. It works only if every route's profit token has the same decimals and value, which is mostly true for `USDC`/`USDT`, but false for WETH, WMATIC, cbETH, and 18-decimal DAI.

Also, the current `expected_profit` does not subtract gas cost before the threshold check. The design document says net profitability should include gas and flash fee, but the current implementation only subtracts flash fee in `CapitalSelector` and separately enforces a gas-price ceiling.

### 2.11 Capital Selector And Flash Loan

`src/execution/capital_selector.rs` checks executor balance, queries flash premium, and chooses self-funded or flash loan:

```rust
self_profit = output_amount - input_amount
flash_profit = self_profit - flash_fee

if self_balance >= input_amount && self_profit >= flash_profit:
    SelfFunded
else if aave_pool configured && input_amount <= max_flash_loan:
    FlashLoan
else:
    SelfFunded
```

Flash loan mechanics are already token-agnostic in shape:

```text
loanAsset = plan.input_token
loanAmount = plan.input_amount
```

But the selector has four problems for flexible anchors:

1. It does not know whether the token is actually Aave-borrowable.
2. It uses one raw `max_flash_loan` for all tokens.
3. It treats flash premium lookup failure as zero because of `unwrap_or(0)`.
4. If self-funded is impossible and flash loan is disabled or too large, it returns `SelfFunded`, relying on simulation to reject.

For stable-only operation, those problems are survivable. For flexible anchors, they create noisy simulations and unsafe accounting.

### 2.12 Risk Manager

`src/risk/limits.rs` checks:

```text
input_amount <= max_position
if flash: input_amount <= max_flash_loan
expected_profit >= min_profit
open tx count
daily loss limit
```

Again, these are raw token units. This must change before adding 18-decimal anchors.

### 2.13 Depeg Guard

`src/risk/depeg_guard.rs` builds `stable_tokens` from `is_stable`.

`src/engine.rs` currently calls:

```rust
if !depeg_guard.stable_routes_allowed() {
    return Ok(());
}
```

This blocks the entire detector if any configured stable token is below the cutoff. For flexible anchors, a better policy is route-scoped:

```text
If a candidate path touches an unhealthy stable token, reject that candidate.
If a global panic flag is configured, optionally block all routes.
```

Otherwise, a WETH -> cbETH -> WETH route would be blocked just because USDC is unhealthy, even if it never touches USDC.

### 2.14 Solidity Executor

`contracts/ArbitrageExecutor.sol` is already mostly token-agnostic.

Self-funded:

```text
executeSelfFunded
  require balance(inputToken) >= inputAmount
  execute hops
  realizedProfit = endingBalance - startBalance
  require realizedProfit >= minProfit
```

Flash loan:

```text
executeFlashLoan
  require execution.inputToken == loanAsset
  require execution.inputAmount == loanAmount
  Aave flashLoanSimple(receiver, loanAsset, loanAmount, data, 0)

executeOperation
  require callback from Aave
  require execution.inputToken == asset
  execute hops
  amountOwed = amount + premium
  require endingBalance >= amountOwed + execution.minProfit
  approve repayment
```

Cyclic enforcement:

```solidity
require(currentToken == params.inputToken, "NOT_CYCLIC");
```

No contract change is required just to allow WETH, DAI, WMATIC, or another ERC-20 as the start/end token. Contract-level `minProfit` must remain raw units of `inputToken`; off-chain USD profitability must not be passed directly into this field.

## 3. Where The USDC/USDT Limit Exists

The effective limit exists in these layers:

| Layer | Current mechanism | Why it limits to USDC/USDT |
|---|---|---|
| Config | `is_stable = true` only for USDC/USDT | Only those tokens are marked as stable anchors |
| Config loader | `TokenConfig.is_stable` | No separate field for cycle-anchor eligibility |
| Runtime token model | `TokenInfo.is_stable` | Stable status is carried into graph state |
| Graph snapshot | `stable_token_indices` from `is_stable` | Only stable tokens become start/end roots |
| Distance cache | BFS seeded from `stable_token_indices` | Reachability pruning is stable-only |
| Detector | DFS loop over `snapshot.stable_token_indices` | Candidate generation starts only from stable tokens |
| Engine gate | global `stable_routes_allowed()` | Any unhealthy stable blocks all route processing |
| Risk | raw `min_net_profit`, `max_position`, `max_flash_loan` | Values assume stablecoin-like units |
| Docs/design | `USDT/USDC -> ... -> USDT/USDC` | Original design explicitly scoped strategy to stable loops |

The main point: do not solve this by marking WETH or WMATIC as `is_stable = true`. That would pollute depeg logic and stable-specific risk. The correct fix is a separate anchor concept.

## 4. Target Behavior

### 4.1 Route Invariant

Keep cyclic same-token routes:

```text
candidate.start_token == candidate.end_token
plan.input_token == plan.output_token
contract final currentToken == params.inputToken
```

Reason: `flashLoanSimple` lends one asset and pulls repayment in that same asset. The arbitrage must end with enough of the borrowed asset to repay principal, premium, and raw token profit.

### 4.2 Anchor Eligibility

A token can be an anchor only if:

```text
token is configured
token address is present in env
token behavior is not exotic
token is allowed as a cycle anchor
token has enough route liquidity
token has usable pricing for USD risk if non-stable
token can be self-funded or flash-loaned
```

For flash-loan routes:

```text
aave_pool is configured
token.flash_loan_enabled is true
amount <= per-token flash cap
Aave reserve can supply the amount
premium query succeeds
simulation succeeds
```

### 4.3 Profit Accounting

Use two profit representations:

```text
raw token profit:
  Used by the smart contract minProfit.
  Unit = input token smallest unit.

USD-normalized profit:
  Used by off-chain risk, ranking, threshold checks, and logs.
  Unit = USD e8, using token manual price or oracle.
```

For a same-token cycle:

```text
gross_profit_raw = output_amount - input_amount
flash_fee_raw    = input_amount * flash_premium_ppm / 1_000_000
net_profit_raw_before_gas =
  gross_profit_raw                  for self-funded
  gross_profit_raw - flash_fee_raw  for flash-loan
```

USD conversion:

```text
amount_usd_e8 = raw_amount * token_price_usd_e8 / 10^token_decimals
```

Gas conversion:

```text
gas_cost_native_raw = gas_limit * max_fee_per_gas
gas_cost_usd_e8 = gas_cost_native_raw * native_price_usd_e8 / 10^native_decimals
```

Final off-chain profitability:

```text
net_profit_usd_e8 =
  value_usd_e8(net_profit_raw_before_gas, input_token)
  - gas_cost_usd_e8
```

Contract min profit:

```text
contract_min_profit_raw =
  floor(net_profit_raw_before_gas * min_profit_realization_bps / 10_000)
```

Optional stricter contract floor:

```text
contract_min_profit_raw =
  max(
    floor(net_profit_raw_before_gas * min_profit_realization_bps / 10_000),
    usd_to_raw(min_net_profit_usd_e8, input_token)
  )
```

The contract cannot enforce gas cost because gas is paid outside ERC-20 balances. Gas must be enforced off-chain before submission.

## 5. Recommended Design

### 5.1 Config Schema

Add explicit token roles.

Recommended token fields:

```toml
[[tokens]]
symbol = "WETH"
address_env = "BASE_WETH"
decimals = 18
is_stable = false
is_cycle_anchor = true
flash_loan_enabled = true
allow_self_funded = false
price_env = "BASE_WETH_PRICE_E8"
max_position_usd_e8 = 200000000000
max_flash_loan_usd_e8 = 1000000000000
```

Field meaning:

| Field | Meaning |
|---|---|
| `is_stable` | Stablecoin health/depeg classification only |
| `is_cycle_anchor` | Detector may start/end cycles at this token |
| `flash_loan_enabled` | Capital selector may borrow this token |
| `allow_self_funded` | Capital selector may use executor balance for this token |
| `price_env` | Required for USD risk when token is an anchor |
| `max_position_usd_e8` | Optional token-specific position cap |
| `max_flash_loan_usd_e8` | Optional token-specific flash cap |

Backward-compatible defaults:

```text
is_cycle_anchor    defaults to is_stable
flash_loan_enabled defaults to is_cycle_anchor
allow_self_funded  defaults to is_stable
```

This preserves current behavior until new anchors are explicitly enabled.

### 5.2 Risk Schema

The current raw global fields should either be renamed or complemented by USD-denominated fields.

Recommended global risk fields:

```toml
min_net_profit_usd_e8 = 10000000       # $0.10
min_trade_usd_e8 = 1000000000          # $10.00
max_position_usd_e8 = 200000000000     # $2,000.00
max_flash_loan_usd_e8 = 1000000000000  # $10,000.00
min_profit_realization_bps = 9000      # contract minProfit = 90% of expected raw net before gas
```

Migration from existing stablecoin raw defaults:

```text
old BASE_MIN_NET_PROFIT = 100000 raw USDC
USDC decimals = 6
USDC price_e8 = 100000000

new min_net_profit_usd_e8 =
  100000 * 100000000 / 10^6
  = 10000000
  = $0.10
```

For compatibility, the loader can accept old fields and convert them if USD fields are absent, but new config should make units explicit.

### 5.3 Runtime Token Model

Update `src/config.rs`:

```rust
pub struct TokenConfig {
    pub symbol: String,
    pub address: Address,
    pub decimals: u8,
    pub is_stable: bool,
    pub is_cycle_anchor: bool,
    pub flash_loan_enabled: bool,
    pub allow_self_funded: bool,
    pub manual_price_usd_e8: Option<u64>,
    pub max_position_usd_e8: Option<u128>,
    pub max_flash_loan_usd_e8: Option<u128>,
}
```

Update `src/types.rs`:

```rust
pub struct TokenInfo {
    pub address: Address,
    pub symbol: String,
    pub decimals: u8,
    pub is_stable: bool,
    pub is_cycle_anchor: bool,
    pub flash_loan_enabled: bool,
    pub allow_self_funded: bool,
    pub behavior: TokenBehavior,
    pub manual_price_usd_e8: Option<u64>,
    pub max_position_usd_e8: Option<u128>,
    pub max_flash_loan_usd_e8: Option<u128>,
}
```

Add helper methods:

```rust
impl Settings {
    pub fn cycle_anchor_tokens(&self) -> Vec<Address> {
        self.tokens
            .iter()
            .filter(|token| token.is_cycle_anchor)
            .map(|token| token.address)
            .collect()
    }

    pub fn token_by_address(&self, address: Address) -> Option<&TokenConfig> {
        self.tokens.iter().find(|token| token.address == address)
    }
}
```

Add config validation:

```text
at least one cycle anchor is configured
every non-stable anchor has a price
every flash-loan-enabled token is a cycle anchor
every flash-loan-enabled token has a flash cap
stable tokens still have prices for depeg guard
```

### 5.4 Graph Model

Keep stable indices, add anchor indices:

```rust
pub struct GraphSnapshot {
    pub stable_token_indices: Vec<usize>,
    pub cycle_anchor_indices: Vec<usize>,
    ...
}
```

Build both:

```rust
let stable_token_indices = tokens
    .iter()
    .enumerate()
    .filter_map(|(idx, token)| token.is_stable.then_some(idx))
    .collect::<Vec<_>>();

let cycle_anchor_indices = tokens
    .iter()
    .enumerate()
    .filter_map(|(idx, token)| token.is_cycle_anchor.then_some(idx))
    .collect::<Vec<_>>();
```

Do not remove `stable_token_indices`. It is still useful for depeg-specific logic, stable-specific metrics, and compatibility.

### 5.5 Distance Cache

Rename fields to describe their new meaning:

```rust
pub struct DistanceCache {
    pub snapshot_id: u64,
    pub reachable_from_anchor: Vec<bool>,
    pub reachable_to_anchor: Vec<bool>,
}
```

Seed BFS from `cycle_anchor_indices`:

```rust
for &anchor_idx in &snapshot.cycle_anchor_indices {
    reachable_from_anchor[anchor_idx] = true;
    queue.push_back(anchor_idx);
}

for &anchor_idx in &snapshot.cycle_anchor_indices {
    reachable_to_anchor[anchor_idx] = true;
    queue.push_back(anchor_idx);
}
```

If minimizing code churn is preferred, keep the field names temporarily and only change the seed source. That compiles faster but leaves misleading names. For this project, renaming is worth it because the old names encode the bug.

### 5.6 Detector

Change DFS roots:

```rust
for &anchor_idx in &snapshot.cycle_anchor_indices {
    let start_addr = snapshot.tokens[anchor_idx].address;
    let start_symbol = snapshot.tokens[anchor_idx].symbol.clone();
    ...
    self.dfs(... anchor_idx, anchor_idx, start_addr, &start_symbol, ...);
}
```

Change reachability pruning:

```rust
if !distance_cache.reachable_to_anchor[edge.to] {
    continue;
}
```

Add per-anchor candidate budgeting:

```text
Current:
  max_candidates is global and early anchors can consume all slots.

Recommended:
  max_candidates_total
  max_candidates_per_anchor
```

Reason: with anchors `[USDC, USDT, WETH, DAI]`, a noisy USDC subgraph should not starve WETH candidates.

Add cycle dedup normalization:

```text
Current dedup:
  exact ordered edge sequence from the current start token

Issue:
  The same economic cycle can be emitted once per anchor if it contains multiple anchors.

Recommended:
  Keep candidates by anchor if capital source differs materially,
  but add a normalized cycle key for logging/ranking and suppress exact duplicates.
```

Suggested key:

```text
edge tuple = (from, to, pool_id)
normalize all rotations of the cycle
choose lexicographically smallest rotation as cycle_key
candidate_key = (cycle_key, start_token)
```

This keeps `WETH` and `USDC` starts distinguishable while making duplicated cycle structure visible.

### 5.7 Route-Scoped Depeg Guard

Replace the global pre-detector gate:

```rust
if !depeg_guard.stable_routes_allowed() {
    return Ok(());
}
```

with candidate-level filtering:

```rust
fn candidate_stables_healthy(candidate: &CandidatePath, depeg_guard: &DepegGuard) -> bool {
    candidate.path.iter().all(|hop| {
        depeg_guard.token_is_healthy(hop.from) &&
        depeg_guard.token_is_healthy(hop.to)
    })
}
```

Then in `process_candidate`:

```text
if candidate touches unhealthy stable:
  reject candidate
```

Optional config:

```toml
global_stable_depeg_halts_all = false
```

If `true`, keep the old global behavior for conservative operation. If `false`, only routes touching unhealthy stables are blocked.

### 5.8 Quantity Search

Replace raw global ladder limits with token-aware limits.

Add valuation helpers:

```rust
fn amount_to_usd_e8(amount_raw: u128, token: &TokenInfo) -> Option<u128>;
fn usd_e8_to_amount(usd_e8: u128, token: &TokenInfo) -> Option<u128>;
```

Use `U256` internally to avoid overflow:

```text
amount_usd_e8 = amount_raw * price_e8 / 10^decimals
amount_raw    = usd_e8 * 10^decimals / price_e8
```

New ladder:

```text
token = candidate.start_token
min_raw = usd_to_raw(min_trade_usd_e8, token)
max_raw = min(
  usd_to_raw(global max_position_usd_e8, token),
  usd_to_raw(token.max_position_usd_e8 if set, token),
  token.max_position_raw if raw override exists
)

current = max(min_raw, 1)
while current <= max_raw:
  ladder.push(current)
  current *= 2
```

For flash-loan-only anchors, optionally cap by `max_flash_loan_usd_e8` too, or let `CapitalSelector` reject later. Capping earlier reduces wasted quotes.

### 5.9 ExactPlan Shape

Keep raw contract fields separate from USD decision fields.

Recommended:

```rust
pub struct ExactPlan {
    pub snapshot_id: u64,
    pub input_token: Address,
    pub output_token: Address,
    pub input_amount: u128,
    pub output_amount: u128,

    // raw input-token units
    pub gross_profit_raw: i128,
    pub flash_fee_raw: u128,
    pub net_profit_before_gas_raw: i128,
    pub contract_min_profit_raw: u128,

    // USD e8 decision units
    pub gross_profit_usd_e8: i128,
    pub flash_fee_usd_e8: i128,
    pub gas_cost_usd_e8: i128,
    pub net_profit_usd_e8: i128,

    pub gas_limit: u64,
    pub gas_cost_wei: U256,
    pub capital_source: CapitalSource,
    pub hops: Vec<HopPlan>,
}
```

Lower-churn alternative:

```text
Keep expected_profit as raw input-token net-before-gas for now.
Add expected_profit_usd_e8 and contract_min_profit_raw.
Document expected_profit units clearly.
```

The lower-churn alternative is faster, but the explicit version prevents future mistakes.

### 5.10 Capital Selection

Change `CapitalSelector::choose` to return no choice when neither capital source is valid:

```rust
pub struct CapitalChoice {
    pub source: CapitalSource,
    pub flash_fee_raw: u128,
    pub net_profit_before_gas_raw: i128,
}

pub async fn choose(&self, plan: &ExactPlan, token: &TokenInfo) -> Result<Option<CapitalChoice>> {
    let gross_profit_raw = plan.output_amount as i128 - plan.input_amount as i128;
    if gross_profit_raw <= 0 {
        return Ok(None);
    }

    let self_funded = if token.allow_self_funded {
        let balance = self.executor_balance(plan.input_token).await.unwrap_or(0);
        (balance >= plan.input_amount).then_some(CapitalChoice {
            source: CapitalSource::SelfFunded,
            flash_fee_raw: 0,
            net_profit_before_gas_raw: gross_profit_raw,
        })
    } else {
        None
    };

    let flash = if token.flash_loan_enabled && self.settings.contracts.aave_pool.is_some() {
        let flash_fee_raw = self.flash.fee_for_amount(plan.input_amount).await?;
        let net = gross_profit_raw - flash_fee_raw as i128;
        (net > 0).then_some(CapitalChoice {
            source: CapitalSource::FlashLoan,
            flash_fee_raw,
            net_profit_before_gas_raw: net,
        })
    } else {
        None
    };

    Ok(best_choice_by_usd_or_raw(self_funded, flash))
}
```

Important changes:

```text
Do not default to SelfFunded when balance is insufficient.
Do not treat flash premium lookup failure as zero.
Do not allow flash loan unless token is enabled and within per-token cap.
Prefer final choice by USD-normalized net after gas if both modes are viable.
```

### 5.11 Flash Loan Availability

The current `FlashLoanEngine` only reads `FLASHLOAN_PREMIUM_TOTAL()`.

For flexible anchors, add an availability layer:

```rust
pub struct FlashLoanAssetStatus {
    pub token: Address,
    pub enabled: bool,
    pub available_liquidity_raw: Option<u128>,
    pub last_checked_block: u64,
}
```

Minimum implementation:

```text
Use config `flash_loan_enabled` and final eth_call simulation.
Reject if amount exceeds configured cap.
```

Better implementation:

```text
Query Aave reserve data or aToken underlying balance.
Cache availability by token.
Refresh periodically and after failed simulations.
```

Do not assume every configured anchor is Aave-listed.

### 5.12 Validator Profit Flow

The validator should check profitability after it knows capital source and gas cost.

Recommended flow:

```text
1. Router returns raw quote plan.
2. CapitalSelector chooses SelfFunded or FlashLoan and fills flash fee.
3. TxBuilder builds calldata with preliminary contract_min_profit_raw.
4. estimate_gas.
5. GasTracker gets max fee and buffered total gas cost.
6. Convert gross/flash/gas to USD e8.
7. Reject if net_profit_usd_e8 < min_net_profit_usd_e8.
8. Recompute contract_min_profit_raw if needed.
9. Rebuild calldata if contract_min_profit_raw changed.
10. Simulate.
11. Return ExecutablePlan.
```

Pseudocode:

```rust
let token = snapshot.token(plan.input_token)?;
let choice = capital_selector.choose(&plan, token).await?;
let mut plan = plan.with_capital_choice(choice);

let preliminary_deadline = now + 30;
plan.contract_min_profit_raw = min_profit_raw_from_plan(&plan, token)?;
let calldata = tx_builder.build_calldata(&plan, preliminary_deadline)?;

let gas_estimate = estimate_gas(calldata).await?;
let gas_quote = gas_tracker.quote(...).await?;

let gross_usd = amount_to_usd_e8(plan.gross_profit_raw, token)?;
let flash_usd = amount_to_usd_e8(plan.flash_fee_raw, token)?;
let gas_usd = native_gas_to_usd_e8(gas_quote.buffered_total_cost_wei)?;
plan.net_profit_usd_e8 = gross_usd - flash_usd - gas_usd;

if plan.net_profit_usd_e8 < settings.risk.min_net_profit_usd_e8 {
    return Ok(None);
}

simulate(final_calldata).await?;
```

This aligns the implementation with `revised_dex_arb_design.md`, where net profit includes gas and flash fee.

### 5.13 TxBuilder

`src/execution/tx_builder.rs` currently computes:

```rust
minProfit: (((plan.expected_profit as f64) * 0.90).max(0.0) as u128)
```

Replace it with a precomputed raw field:

```rust
minProfit: plan.contract_min_profit_raw.into()
```

Reason:

```text
expected_profit may become USD-normalized or net-after-gas.
contract minProfit must remain input-token raw units.
f64 conversion should be avoided for token amounts.
```

Use integer basis points:

```rust
contract_min_profit_raw =
    net_profit_before_gas_raw.saturating_mul(min_profit_realization_bps as i128) / 10_000;
```

Handle negative or zero values by rejecting before tx build.

### 5.14 Risk Manager

Risk should check USD-normalized values and token-specific raw caps:

```text
input_value_usd_e8 <= max_position_usd_e8
if flash: input_value_usd_e8 <= max_flash_loan_usd_e8
net_profit_usd_e8 >= min_net_profit_usd_e8
open_tx_count < max_concurrent_tx
daily_pnl_usd_e8 > daily_loss_limit_usd_e8
```

Keep optional raw caps for operational safety:

```text
input_amount <= token.max_position_raw if configured
flash_amount <= token.max_flash_loan_raw if configured
```

Daily PnL should also move to USD e8, otherwise mixed-token profits cannot be accumulated safely.

### 5.15 Logging And Metrics

Add fields to opportunity logs:

```text
anchor_symbol
anchor_token
capital_source
input_amount_raw
input_value_usd_e8
gross_profit_raw
gross_profit_usd_e8
flash_fee_raw
flash_fee_usd_e8
gas_cost_usd_e8
net_profit_usd_e8
contract_min_profit_raw
cycle_key
```

This is important after flexible anchors because raw values are no longer comparable in logs.

## 6. Concrete File Change Plan

### 6.1 `config/base.toml`

Add fields to each token.

Initial conservative Base anchors:

```toml
[[tokens]]
symbol = "USDC"
is_stable = true
is_cycle_anchor = true
flash_loan_enabled = true
allow_self_funded = true

[[tokens]]
symbol = "USDT"
is_stable = true
is_cycle_anchor = true
flash_loan_enabled = true
allow_self_funded = true

[[tokens]]
symbol = "WETH"
is_stable = false
is_cycle_anchor = true
flash_loan_enabled = true
allow_self_funded = false
price_env = "BASE_WETH_PRICE_E8"

[[tokens]]
symbol = "DAI"
is_stable = false
is_cycle_anchor = true
flash_loan_enabled = true
allow_self_funded = false
price_env = "BASE_DAI_PRICE_E8"

[[tokens]]
symbol = "cbETH"
is_stable = false
is_cycle_anchor = false
flash_loan_enabled = false
allow_self_funded = false
```

Do not enable `cbETH` as an anchor until pricing, Aave liquidity, and liquidity depth are verified.

### 6.2 `config/polygon.toml`

Initial conservative Polygon anchors:

```toml
USDC:
  is_cycle_anchor = true
  flash_loan_enabled = true
  allow_self_funded = true

USDT:
  is_cycle_anchor = true
  flash_loan_enabled = true
  allow_self_funded = true

WMATIC:
  is_cycle_anchor = true
  flash_loan_enabled = true
  allow_self_funded = false

WETH:
  is_cycle_anchor = true
  flash_loan_enabled = true
  allow_self_funded = false

DAI:
  is_cycle_anchor = true
  flash_loan_enabled = true
  allow_self_funded = false
```

Only enable anchors whose `price_env` is populated.

### 6.3 `.env.example`

Add required price guidance for anchors:

```text
BASE_WETH_PRICE_E8=...
BASE_DAI_PRICE_E8=100000000
POLYGON_WMATIC_PRICE_E8=...
POLYGON_WETH_PRICE_E8=...
POLYGON_DAI_PRICE_E8=100000000
```

Add optional new risk envs:

```text
BASE_MIN_NET_PROFIT_USD_E8=
BASE_MAX_POSITION_USD_E8=
BASE_MAX_FLASH_LOAN_USD_E8=
POLYGON_MIN_NET_PROFIT_USD_E8=
POLYGON_MAX_POSITION_USD_E8=
POLYGON_MAX_FLASH_LOAN_USD_E8=
```

### 6.4 `src/config.rs`

Changes:

```text
Add fields to TokenConfig.
Add optional fields to FileTokenConfig.
Add USD-denominated risk fields to RiskSettings/FileConfig.
Default is_cycle_anchor to is_stable.
Default allow_self_funded to is_stable.
Default flash_loan_enabled to is_cycle_anchor.
Add cycle_anchor_tokens().
Add token_by_address().
Validate anchor pricing and at least one anchor.
```

Example parsing:

```rust
let is_cycle_anchor = token.is_cycle_anchor.unwrap_or(token.is_stable);
let flash_loan_enabled = token.flash_loan_enabled.unwrap_or(is_cycle_anchor);
let allow_self_funded = token.allow_self_funded.unwrap_or(token.is_stable);
```

### 6.5 `src/types.rs`

Changes:

```text
Extend TokenInfo.
Extend ExactPlan with raw and USD profit fields.
Add CapitalChoice if selector returns richer data.
Optionally add CandidatePath.cycle_key.
```

Recommended `CandidatePath` extension:

```rust
pub struct CandidatePath {
    pub snapshot_id: u64,
    pub start_token: Address,
    pub start_symbol: String,
    pub screening_score_q32: i64,
    pub cycle_key: String,
    pub path: SmallVec<[CandidateHop; 8]>,
}
```

### 6.6 `src/discovery/mod.rs`

Pass the new token fields from `TokenConfig` to `TokenInfo`.

No change is needed in pool fetching for the basic anchor fix. Pool admission already allows any configured token as intermediate or anchor.

### 6.7 `src/graph/model.rs`

Add `cycle_anchor_indices` and populate it from `TokenInfo.is_cycle_anchor`.

Keep `stable_token_indices` for stable-specific policies.

### 6.8 `src/graph/distance_cache.rs`

Rename stable reachability to anchor reachability and seed from `cycle_anchor_indices`.

Tests should verify:

```text
stable token can be non-anchor
non-stable token can be anchor
reachable_to_anchor is true for nodes that can return to any anchor
```

### 6.9 `src/detector/path_finder.rs`

Use `cycle_anchor_indices`.

Update variable names:

```text
stable_idx    -> anchor_idx
start_symbol  -> anchor_symbol or start_symbol
reachable_to_stable -> reachable_to_anchor
```

Add per-anchor cap and normalized cycle key if implementing duplicate control in the same pass.

### 6.10 `src/router/quantity_search.rs`

Use token-aware USD limits instead of global raw `max_position`.

Minimum safe change:

```text
derive max input raw by converting max_position_usd_e8 to the start token
```

Better change:

```text
min_trade_usd_e8
max_position_usd_e8
per-token max_position_usd_e8 override
per-token raw override
pool safe_capacity_in cap
flash cap prefilter for flash-only anchors
```

### 6.11 `src/router/mod.rs`

Keep route quote logic mostly intact.

Change `expected_profit` handling:

```text
gross_profit_raw = final output - input
reject if gross_profit_raw <= 0
fill raw fields only
do not try to apply gas or flash fee here
```

Reason: capital source and gas are validator responsibilities.

### 6.12 `src/execution/capital_selector.rs`

Change selection to return `Option<CapitalChoice>`.

Required behavior:

```text
Self-funded is valid only if allow_self_funded and executor balance >= input.
Flash is valid only if flash_loan_enabled, Aave pool configured, amount within cap, and premium lookup succeeds.
If neither is valid, reject before simulation.
```

### 6.13 `src/execution/flash_loan.rs`

Required:

```text
Do not hide premium lookup errors for selected flash routes.
Cache premium_ppm for a short period to avoid repeated RPC calls.
```

Optional:

```text
Add Aave reserve availability checks by token.
```

### 6.14 `src/execution/validator.rs`

Main fix:

```text
Move min profit check from raw expected_profit to USD net profit after gas and flash fee.
```

Validator needs access to token metadata. Options:

1. Pass `&GraphSnapshot` into `prepare`.
2. Build a `token_map` in `Validator` from `Settings`.
3. Add needed token metadata directly to `ExactPlan`.

Recommended: pass snapshot or token metadata explicitly so plan valuation is tied to the same snapshot used for routing.

### 6.15 `src/execution/tx_builder.rs`

Replace f64-derived minProfit:

```rust
minProfit: plan.contract_min_profit_raw.into()
```

This avoids mixing USD and raw token units and avoids lossy float conversion.

### 6.16 `src/risk/limits.rs`

Change fields:

```text
max_position_usd_e8
max_flash_loan_usd_e8
min_profit_usd_e8
daily_loss_limit_usd_e8
```

Risk should compare:

```text
plan.exact.input_value_usd_e8
plan.exact.net_profit_usd_e8
plan.exact.capital_source
```

Keep raw caps as optional token-level backstops.

### 6.17 `src/engine.rs`

Move depeg check from global process gate to candidate route gate.

Current:

```text
if stable routes not allowed, skip all detection
```

Recommended:

```text
detect candidates
for each candidate:
  reject if it touches an unhealthy stable token
  otherwise continue
```

This lets non-stable cycles continue if configured policy allows.

### 6.18 Tests

Update all manual `TokenConfig` and `TokenInfo` construction in tests.

Add tests:

```text
config defaults preserve existing stable-only behavior
graph builds cycle_anchor_indices separately from stable_token_indices
detector can find WETH-start cycle when WETH is_cycle_anchor = true
detector does not find WETH-start cycle when WETH is_cycle_anchor = false
distance cache uses anchors, not stables
quantity ladder gives sensible raw limits for USDC and WETH under same USD cap
validator rejects non-stable anchor without price
capital selector rejects flash when token is not flash_loan_enabled
capital selector rejects instead of returning SelfFunded with insufficient balance
tx builder uses contract_min_profit_raw
route-scoped depeg blocks only paths touching unhealthy stable tokens
```

Fork/integration tests:

```text
WETH flash loan cycle simulation on Base fork
WMATIC flash loan cycle simulation on Polygon fork
Aave unavailable asset rejects before submit
gas-inclusive USD profitability rejects marginal raw-profit route
```

## 7. Implementation Order

### Phase 1: Anchor Model And Detector Refactor

Goal: separate stablecoin status from cycle-anchor eligibility, then make detection use cycle anchors while preserving the current USDC/USDT behavior by default.

Tasks:

1. Add optional config fields.
2. Default `is_cycle_anchor = is_stable`.
3. Default `flash_loan_enabled = is_cycle_anchor`.
4. Default `allow_self_funded = is_stable`.
5. Add `cycle_anchor_indices` to `GraphSnapshot`.
6. Keep `stable_token_indices` for depeg-specific logic.
7. Rename `DistanceCache` stable reachability to anchor reachability.
8. Seed distance cache from `cycle_anchor_indices`.
9. Switch `PathFinder` root loop from `stable_token_indices` to `cycle_anchor_indices`.
10. Add per-anchor candidate budgeting so one noisy anchor cannot consume all candidates.
11. Add WETH/DAI/WMATIC anchor tests without enabling those anchors in production config yet.
12. Update README/design notes to state that routes are same-token cycles, not stable-only cycles.

Expected result:

```text
cargo test passes
detector still starts only from USDC/USDT with existing configs
non-stable anchors can produce candidates in unit tests
```

Do not enable new production anchors in this phase. This phase should be a low-risk architecture refactor.

### Phase 2: Production-Safe Flash-Loan Anchors

Goal: make non-stable anchors safe to enable by fixing valuation, token-aware limits, flash-loan eligibility, contract min-profit units, and depeg policy.

Tasks:

1. Add USD valuation helpers using token price and decimals.
2. Add native gas USD price support.
3. Convert `min_net_profit`, `max_position`, `max_flash_loan`, and daily PnL checks to USD e8.
4. Keep raw token profit fields separate from USD decision fields.
5. Add `contract_min_profit_raw` and use it in `TxBuilder`.
6. Remove f64-based contract `minProfit` calculation.
7. Update quantity search to use token-aware USD caps.
8. Make `CapitalSelector` reject when no valid capital source exists.
9. Require `flash_loan_enabled` and per-token flash caps before selecting flash loan.
10. Make flash premium lookup errors reject selected flash routes.
11. Optionally cache Aave reserve availability by token.
12. Move depeg handling from global process gate to candidate-level filtering.
13. Keep optional global stable halt for conservative operation.
14. Add logs for anchor symbol, raw profit, USD profit, flash fee, gas cost, and contract min profit.
15. Enable DAI first if pricing is reliable.
16. Enable WETH/WMATIC with small USD caps.
17. Run `--simulate-only`.
18. Run fork tests.
19. Enable private/protected submit only after simulation logs are clean.
20. Increase caps gradually.

Expected result:

```text
USDC, DAI, WETH, and WMATIC can be compared by common USD thresholds
WETH is no longer rejected because of a stale 6-decimal raw max_position
flash route is selected only for configured and eligible anchor tokens
routes touching unhealthy USDC/USDT are rejected
routes not touching unhealthy stables may continue if policy allows
```

Do not enable cbETH or other correlated/volatile assets until:

```text
price feed is configured
Aave borrow liquidity is confirmed
pool liquidity is enough
slippage behavior is understood
token behavior is non-exotic
```

## 8. Key Edge Cases

### 8.1 Anchor Is Not Aave-Listed

The detector may find a valid raw cycle, but Aave may not lend the start token. The capital selector must reject early unless self-funded balance is available.

### 8.2 Anchor Has No Price

For non-stable anchors, missing price should reject the route before risk evaluation. Without price, the bot cannot compare gas or profit to USD thresholds.

### 8.3 Stable Depeg

If USDC is unhealthy:

```text
USDC -> WETH -> USDC should reject.
WETH -> USDC -> DAI -> WETH should reject because it touches USDC.
WETH -> cbETH -> WETH may continue only if global halt is disabled and both tokens are healthy.
```

### 8.4 Multiple Anchors In One Cycle

Cycle:

```text
USDC -> WETH -> DAI -> USDC
```

If USDC, WETH, and DAI are all anchors, rotations of the same cycle may be valid different candidates:

```text
USDC start
WETH start
DAI start
```

They can have different borrow amounts, flash availability, and USD net profit. Evaluate them independently, but dedup/log normalized cycle keys so the engine does not spam equivalent submissions.

### 8.5 Raw Profit Is Positive But USD Net Is Negative

Example:

```text
gross WETH profit after flash fee = 0.00001 WETH
gas cost = $2
WETH price = $3,000
gross USD = $0.03
net USD = -$1.97
```

The current implementation may accept this if raw profit exceeds raw threshold. The fixed validator must reject it.

### 8.6 Contract Min Profit Must Not Use USD Units

If `minProfit` is accidentally passed as USD e8 into the contract, a WETH route can become impossible or unsafe because the contract interprets it as WETH wei. Keep `contract_min_profit_raw` explicit.

### 8.7 Current Global Candidate Cap Starves Later Anchors

The current detector stops after `max_candidates = 128`. If it loops anchors in config order, noisy early anchors can consume the cap. Use per-anchor caps or post-detection ranking.

### 8.8 Search Space Growth

More anchors increase DFS roots. Use:

```text
max_cycle_anchors
max_candidates_per_anchor
max_candidates_total
max_branching
max_hops
liquidity floor
route health
changed-edge requirement
```

The changed-edge requirement is especially important because it keeps detection tied to recent market movement.

## 9. Minimal Patch Versus Correct Patch

### Minimal Patch

```text
Add WETH/DAI as is_stable = true
```

Do not do this.

Problems:

```text
WETH would be treated as a stablecoin by depeg guard.
Risk thresholds remain raw and wrong.
Quantity search remains wrong for 18-decimal assets.
Logs and daily PnL mix incompatible units.
```

### Acceptable Short-Term Patch

```text
Add is_cycle_anchor.
Use cycle anchors in GraphSnapshot, DistanceCache, and PathFinder.
Keep behavior defaulted to current stables.
Add WETH anchor only in tests.
```

This is safe as a preparatory refactor but should not be enabled in production for WETH/WMATIC without USD risk changes.

### Recommended Production Patch

```text
Separate stable, cycle anchor, and flash loan eligibility.
Use anchor reachability in detector.
Use token-aware quantity limits.
Use USD-normalized off-chain profitability.
Keep raw contract minProfit.
Add route-scoped depeg filtering.
Add flash-loan eligibility checks.
Roll out anchors gradually.
```

## 10. Definition Of Done

The flexible start/end project is complete when:

```text
USDC/USDT behavior remains unchanged by default.
Non-stable anchors can be enabled without using is_stable.
Detector can emit WETH/DAI/WMATIC cyclic candidates.
Quantity search uses sensible amounts for 6-decimal and 18-decimal tokens.
Validator rejects based on net USD profit after flash fee and gas.
TxBuilder sends raw token minProfit to the contract.
RiskManager no longer compares mixed raw token units.
CapitalSelector rejects unavailable capital sources before simulation.
Depeg guard can reject by route instead of globally, unless configured to halt all.
Tests cover stable-only compatibility and non-stable anchor behavior.
Fork simulation passes for at least one non-stable flash-loan anchor before live use.
```

## 11. Final Recommendation

Implement the change in two separate pull-sized units:

```text
PR 1:
  config/model/detector refactor from stable anchors to cycle anchors.
  defaults preserve current behavior.
  tests prove WETH can be an anchor when enabled.

PR 2:
  USD valuation, token-aware limits, capital selection hardening,
  tx_builder raw minProfit, route-scoped depeg, and production config enablement.
```

This avoids mixing a low-risk architecture refactor with high-risk profitability accounting. The detector change alone is straightforward. The production-safe flash-loan expansion depends on the risk-accounting changes, because the current global raw thresholds are the main source of incorrect behavior once the anchor set moves beyond `USDC` and `USDT`.
