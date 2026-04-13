# Production Readiness and Alchemy Cost Optimization Report

Date: 2026-04-13 Asia/Seoul

Project: `dex-arbitrage`

Scope: full repository review, static code review, local test execution, and current Alchemy Pay As You Go pricing/CU documentation check. The optimization recommendations below assume the search universe should not be reduced. Cost improvements must come from better state maintenance, local deterministic math, batching, scheduling, and provider architecture rather than narrowing routes, venues, tokens, or candidate count by arbitrary limits.

## Executive Summary

This project is a strong prototype for a Base/Polygon DEX cyclic arbitrage bot. It already has useful foundations: chain-specific config, factory/log discovery, cached discovery and pool state, immutable graph snapshots, changed-edge candidate detection, split routing, self-funded/mixed flash loan selection, Prometheus metrics, and a Solidity executor.

It is not production-ready for real capital yet. The biggest blockers are:

1. The hot quote path can generate unbounded Alchemy `eth_call` cost when V3 and Curve quotes are enabled.
2. Event ingestion defaults can be expensive on Alchemy for large pool universes, especially `address_logs` with many watched pool addresses.
3. Polygon log chunk defaults exceed Alchemy Pay As You Go `eth_getLogs` range guidance for Polygon.
4. RPC reliability is basic: no request timeout, retry/backoff, 429-aware CU budget manager, read fallback, or request priority.
5. Reorg handling is only partially designed: a snapshot ring exists, but block parent validation and rollback/replay are not wired into the engine.
6. Execution safety is not yet production grade: nonce replacement, private-only policy controls, final simulation parsing, and deployment post-configuration are incomplete.
7. Test coverage passes locally but is still mostly unit-level. Fork, reorg, rate-limit, live-RPC, and adapter-specific Solidity coverage are missing.

Local verification status:

- `cargo test --all`: pass, 64 Rust tests.
- `forge test`: pass, 5 Solidity tests.
- `cargo clippy --all-targets --all-features -- -D warnings`: pass.
- `cargo fmt --all -- --check`: pass.

## Current Architecture

Main runtime flow:

1. `src/main.rs` loads `.env`, parses `--chain`, installs logging/metrics, and starts `run_chain`.
2. `src/engine.rs` bootstraps discovery, builds a `GraphStore`, processes all initial edges, then polls events forever.
3. `src/discovery/factory_scanner.rs` discovers pools through factory `allPairs`, V3 `PoolCreated` logs, Curve registries, and Balancer vault logs.
4. `src/discovery/pool_fetcher.rs` fetches V2, V3, Curve, and Balancer pool state.
5. `src/discovery/event_stream.rs` watches pool logs and generates either exact patches or full refresh triggers.
6. `src/graph/model.rs` builds token, pool, pair, adjacency, and reverse adjacency indexes.
7. `src/detector/path_finder.rs` finds changed-edge cycles around cycle anchors.
8. `src/router/mod.rs`, `src/router/exact_quoter.rs`, and `src/router/split_optimizer.rs` search input size and split per hop.
9. `src/execution/validator.rs` selects capital, estimates gas, performs profitability checks, and simulates.
10. `src/execution/submitter.rs` signs and submits through protected/private/public channels.
11. `contracts/ArbitrageExecutor.sol` executes self-funded and Aave flash-loan routes.

## Strong Existing Foundations

The project already does several things that reduce cost and production risk:

- Discovery cache: `state/{chain}_discovery_cache.json`.
- Token metadata cache: `state/{chain}_token_metadata_cache.json`.
- Pool state cache: `state/{chain}_pool_state_cache.json`.
- Multicall support through `src/discovery/multicall.rs`.
- Event-driven refresh instead of full repeated scanning.
- Pool health and staleness checks in admission/detector paths.
- USD-denominated risk checks in `src/risk/valuation.rs` and `src/risk/limits.rs`.
- Mixed flash loan accounting where only the shortfall is borrowed.
- Strict target allowlist support in the executor contract.
- Prometheus exporter bound to localhost by default.

These pieces should be preserved. The production work is mainly about making them more exact, more reliable, and cheaper at scale.

## Alchemy Pay As You Go Facts Used

Official Alchemy pricing page, checked for this report:

- Pay As You Go: `$0.45 / 1M CU` for the first 300M CU/month, then `$0.40 / 1M CU`.
- Pay As You Go throughput: `10,000 CU/s`, shown as starting from 300 requests/s.
- Free plan includes 30M CU/month and lower rate limits.

Official Alchemy CU docs, checked for this report:

- `eth_blockNumber`: 10 CU.
- `eth_call`: 26 CU.
- `eth_getLogs`: 60 CU.
- `eth_estimateGas`: 20 CU.
- `eth_gasPrice`: 20 CU.
- `eth_maxPriorityFeePerGas`: 10 CU.
- `eth_sendRawTransaction`: 40 CU.
- `eth_getTransactionReceipt`: 20 CU.
- `eth_getBlockReceipts`: 20 CU, with higher throughput CU.
- Webhooks and WebSocket subscription APIs are bandwidth-priced at 0.04 CU/byte. Alchemy states a typical 1000 byte subscription event averages about 40 CU.
- Alchemy `eth_getLogs` docs show Polygon Pay As You Go block-range support as 2000 blocks, while Ethereum/Base/Optimism/Arbitrum-style chains have broader/unlimited Pay As You Go ranges subject to response caps.

Sources:

- Alchemy pricing: https://www.alchemy.com/pricing
- Alchemy compute unit costs: https://www.alchemy.com/docs/reference/compute-unit-costs
- Alchemy Base `eth_getLogs` ranges: https://www.alchemy.com/docs/chains/base/base-api-endpoints/eth-get-logs
- Alchemy Polygon `eth_getLogs` ranges: https://www.alchemy.com/docs/chains/polygon-pos/polygon-po-s-api-endpoints/eth-get-logs

## Alchemy Cost Model for This Bot

Use a 30-day month:

```text
seconds_per_month = 2,592,000

payg_cost(cu_month):
  first = min(cu_month, 300,000,000) * 0.45 / 1,000,000
  rest  = max(cu_month - 300,000,000, 0) * 0.40 / 1,000,000
  total = first + rest
```

Default block polling cost:

```text
poll_interval_ms = 400
polls_per_second = 2.5
eth_blockNumber = 10 CU

monthly CU per chain = 2.5 * 10 * 2,592,000 = 64,800,000 CU
approx PAYG cost per chain = $29.16/month
```

Address-log polling cost, assuming 2 second blocks and `EVENT_LOG_ADDRESS_CHUNK_SIZE=200`:

```text
address_logs_cu_per_second =
  blocks_per_second * ceil(watched_addresses / 200) * 60
```

Examples:

| Watched addresses | Chunks | CU/month from `eth_getLogs` | Approx PAYG cost |
| ---: | ---: | ---: | ---: |
| 1,000 | 5 | 388.8M | $170.52 |
| 5,000 | 25 | 1.944B | $792.60 |
| 10,000 | 50 | 3.888B | $1,570.20 |

Those numbers exclude `eth_blockNumber`, quote calls, gas calls, simulation, discovery, receipt polling, retries, and reconnect backfills.

Topic-log polling cost, assuming 2 second blocks:

```text
topic_logs_cu_per_second = 0.5 * 60 = 30 CU/s
monthly CU = 77.76M
approx PAYG cost = $34.99/month per chain
```

Topic logs may return huge responses because they are not address-filtered. That can hurt latency and reliability even if CU is lower.

WebSocket cost break-even:

```text
average websocket event = about 40 CU
break_even_events_per_second = address_logs_cu_per_second / 40
```

For 1000 watched addresses, address polling is about 150 CU/s, so WSS is cheaper below about 3.75 delivered events/s. For 10,000 watched addresses, address polling is about 1500 CU/s, so WSS is cheaper below about 37.5 delivered events/s.

## Main Alchemy Cost Risks in Current Code

### 1. V3 and Curve exact quoting can dominate cost

`src/router/exact_quoter.rs` defaults `USE_V3_RPC_QUOTER=true`. For V3 pools, each quote can call the external quoter through Alchemy `eth_call` at 26 CU. Curve quotes call `get_dy` or `get_dy_underlying`, also through `eth_call`.

The router may evaluate many amounts per candidate:

- Initial ladder.
- Up to 8 seed centers.
- Refinement points.
- 32 ternary-search rounds with left/right evaluations.
- Dense points after ternary search.
- Split optimizer quotes multiple pools per hop.

For a V3/Curve-heavy route, one candidate can easily become hundreds or thousands of `eth_call` requests before final simulation. That is the largest avoidable Alchemy cost in the system.

Production recommendation:

- Do not use Alchemy as the hot-path quote engine.
- Maintain local exact AMM state and perform quotes in-process.
- Keep Alchemy calls for event ingestion, gap repair, final simulation, and submission only.

### 2. `address_logs` scales with number of watched pools

`src/discovery/event_stream.rs` defaults to `EVENT_INGEST_MODE=address_logs`, chunks addresses by `EVENT_LOG_ADDRESS_CHUNK_SIZE=200`, and calls `eth_getLogs` once per address chunk per block range.

This preserves address precision, but cost grows linearly with watched pools. In a full-universe deployment, this can be more expensive than the trading logic itself.

Production recommendation:

- Use a dynamic ingester that chooses WSS, topic logs, address logs, or block receipts based on measured event density and address count.
- Prefer WSS for sparse watched events and HTTP `eth_getLogs` only for gap backfill.
- Keep topic logs as a fallback when address chunks become too expensive and response volume is still manageable.

### 3. Polygon log chunk defaults can exceed Pay As You Go limits

`DISCOVERY_LOG_CHUNK_BLOCKS` defaults to 50,000, and `EVENT_LOG_CHUNK_BLOCKS` defaults to 5,000.

Alchemy docs show Polygon Pay As You Go `eth_getLogs` block range support as 2,000 blocks. That means Polygon cold discovery, downtime backfill, or large event gaps may fail or degrade unless the chunk size is made chain-aware.

Production recommendation:

- Enforce `polygon_max_eth_getlogs_range <= 2000` by default.
- Make chunk sizing provider-plan-aware:
  - Base: large chunks are acceptable but still respect response-size caps.
  - Polygon: cap at 2000 for Pay As You Go.
  - Unknown chain/provider: start small, increase only after successful probes.

### 4. Discovery starts from block 0 by default

Both `config/base.toml` and `config/polygon.toml` set many DEX `start_block = 0`.

For V3 and Balancer discovery, this can force huge `eth_getLogs` scans on first run if the discovery cache is missing. This is not a production bootstrap strategy.

Production recommendation:

- Use real factory/vault deployment block numbers.
- Ship a signed pool-universe snapshot for each chain/venue.
- Use Alchemy only for incremental discovery after the snapshot cursor.
- Store cache writes atomically so a crash cannot corrupt the only warm-start state.

### 5. No CU budget manager or 429-aware scheduler

`src/rpc/mod.rs` counts approximate CU metrics but does not enforce a budget. It also has no timeout, no retry/backoff, no 429 handling, and no read fallback path even though `fallback_rpc_url` exists.

Production recommendation:

- Add a central RPC scheduler with request classes:
  - Critical: final simulation, private submit, receipt reconciliation.
  - High: event gap repair, changed-pool refresh.
  - Normal: discovery increments, token metadata.
  - Low: price refresh, maintenance scans, historical backfills.
- Use token buckets in CU/s and CU/day per provider.
- On 429, respect backoff, reduce low/normal priority throughput, and keep critical flow available.
- Load CU weights from config so Alchemy doc changes do not require code edits.

## Optimization Plan Without Reducing Search Space

The current config includes search caps:

- `max_pair_edges_per_pair`
- `max_split_parallel_pools`
- `top_k_paths_per_side`
- `max_virtual_branches_per_node`
- `path_beam_width`
- `max_candidates_per_refresh`

Lowering those values would reduce cost, but it reduces search coverage. That is not acceptable under this report's scope. The better production path is to keep the full universe and replace arbitrary limits with exact enumeration plus admissible economic pruning.

### A. Replace RPC-based quotes with local exact AMM state

Uniswap V2:

- Already local and integer-based. Keep it.

Uniswap V3 / concentrated liquidity:

- Maintain per-pool state:
  - `slot0`
  - active liquidity
  - current tick
  - initialized ticks and liquidity net
  - tick bitmap
- Bootstrap initialized ticks from a local indexer or provider snapshot.
- Update state from Swap/Mint/Burn/Initialize logs.
- Quote with exact swap-step math locally.
- Use Alchemy `eth_call` only to repair state on detected mismatch and for final route simulation.

Curve:

- Implement local StableSwap invariant math for supported pool classes.
- Track balances, `A`, fee, admin fee, rates, and underlying flags.
- For unsupported Curve pool classes, quarantine or route them through a slower non-hot validation path.
- Avoid `get_dy` for every candidate amount.

Balancer:

- Current weighted fallback quote is local but uses floating-point math.
- Replace with fixed-point Balancer weighted math for deterministic agreement with the vault.
- Add support for stable/composable pools only after exact math and fork tests exist.

Expected cost effect:

- Hot candidate search and split optimization become zero-Alchemy-CU work.
- Alchemy is used for events and final validation, not for every quote sample.

### B. Use all parallel pools with a convex allocator

Current split optimizer uses greedy slices and caps the number of parallel pools. It is cheap to implement but not optimal and depends on `max_split_parallel_pools`.

Production replacement:

1. For every token pair, keep all healthy pools.
2. Remove only mathematically dominated pools. A pool is dominated only if it can never beat another pool for any input in the allowed size interval.
3. For a target total input, allocate across pools by equalizing marginal output:
   - V2 pools have an analytic marginal output curve.
   - V3 pools can expose local marginal output from swap-step math.
   - Curve/Balancer can use local derivative or finite-difference derivative.
4. Solve allocation by binary search on marginal price `lambda`, with capacity constraints.
5. Materialize only non-zero allocations.

This preserves the full pool universe while reducing quote evaluations and producing better routes.

### C. Replace beam search with complete changed-edge cycle enumeration

Current detector uses beam/top-k style limits. A production full-universe detector should instead enumerate all economically possible simple cycles that contain a changed edge, then use admissible upper bounds.

Recommended algorithm:

1. For every changed directed edge `(u -> v)`, enumerate simple paths from cycle anchors to `u` and from `v` back to the same anchor.
2. Use bitsets for visited tokens to avoid duplicate nodes.
3. Use a gas-aware maximum hop boundary, not an arbitrary search-width boundary. A path is pruned only when its best possible remaining edge weights cannot cover gas and risk thresholds.
4. Precompute per-token best outgoing edge bounds by hop depth. This gives an admissible upper bound for branch-and-bound.
5. Keep all non-dominated cycles. Do not cap by top-k until after exact local valuation has proven domination.

This avoids missing profitable cycles while keeping CPU bounded by provable economic pruning.

### D. Use local state for backtesting and replay

For production, the same local AMM state engine should support:

- Historical replay.
- Deterministic candidate reproduction.
- Quote mismatch diagnosis.
- Reorg replay.
- Regression tests using real blocks.

This reduces Alchemy spend because debugging and strategy tuning do not require repeated live RPC calls.

### E. Move final validation to one expensive call per executable route

After local exact quote and local gas estimate:

1. Only the best non-dominated executable route should call Alchemy for final simulation.
2. Use `eth_call` or `eth_simulateV1` once at the target state.
3. Use `eth_estimateGas` only after the route passes local gas bounds.
4. If final simulation fails, mark the pool/path state as suspect and repair the minimum necessary state.

Target steady-state cost per executable candidate:

```text
eth_estimateGas: 20 CU
eth_gasPrice: 20 CU
eth_maxPriorityFeePerGas: 10 CU
eth_call or eth_simulateV1: 26 to 40 CU
total before submit: about 76 to 90 CU
```

That is acceptable. The current risk is spending far more than this before reaching final validation.

## Production Readiness Gaps

### P0 - Must Fix Before Real Capital

| Area | Current risk | Required production change |
| --- | --- | --- |
| Hot-path quoting | V3/Curve quotes can call Alchemy for every sample. | Implement local exact V3 and Curve quote engines. Use Alchemy only for repair/final simulation. |
| RPC reliability | No timeout, retry, backoff, 429 handling, read fallback, or request priority. | Add RPC scheduler, timeouts, retry policy, CU budget manager, provider health, and failover. |
| Reorg handling | Snapshot rollback exists but engine does not validate parent hash or replay events. | Track canonical block chain, detect parent mismatch, rollback to common ancestor, replay logs. |
| Polygon `eth_getLogs` ranges | Defaults can exceed Pay As You Go range support. | Chain/provider-aware chunk caps and adaptive split-on-failure. |
| Private execution policy | Public fallback can leak arbitrage unless explicitly controlled. | Default to private/protected-only. Public fallback only with explicit policy and fresh simulation. |
| Nonce lifecycle | Dropped nonces are marked but not replaced or reconciled. | Add pending nonce monitor, replacement bumping, cancellation, and restart recovery. |
| Contract deployment | Scripts deploy only. They do not set operator, strict allowlist, targets, pause state, or verify. | Add idempotent deployment/post-deploy scripts and Safe multisig ownership flow. |
| Contract safety | V3 callback checks only allowlist, not canonical factory/pool derivation. | Verify canonical pool/factory/codehash per adapter, or enforce complete allowlist automation. |
| Final simulation parsing | Custom simulate call is treated as success if the RPC call returns `Ok`. | Parse result/revert details and require successful state transition/profit. |
| Depeg guard | `DepegGuard` exists but is not integrated into route rejection. | Wire stable depeg checks into detector/risk/validator path. |

### P1 - Needed for Production Operations

| Area | Current risk | Required production change |
| --- | --- | --- |
| Observability | Metrics are minimal. | Add opportunity funnel metrics, CU by chain/provider/method, cache hit rates, quote mismatch, reorg count, state lag, route latency, PnL, revert reason. |
| Cache durability | JSON caches are written directly. | Use atomic write-rename, schema versions, compression, checksums, and cache repair. |
| CI/CD | No CI workflow in repo. | Add GitHub Actions for fmt, clippy, tests, forge tests, coverage, audit tools, and Docker build. |
| Deployment packaging | No Dockerfile/systemd/Kubernetes manifests. | Add reproducible release images, runtime config validation, health endpoints, and restart policy. |
| Provider architecture | `fallback_rpc_url` is configured but not used for normal read failover. | Route reads through provider pool with health scoring and method support discovery. |
| Fork tests | Solidity tests cover mainly V2/mixed flash path. | Add fork tests for Base/Polygon V3, Curve, Balancer, allowlist, callback, paused, rescue, slippage, and revert paths. |
| Load testing | No sustained event/RPC stress tests. | Replay real block logs and prove latency/CU budgets under full universe. |
| Secrets | `.env` holds private keys. | Use KMS/HSM or remote signer, least-privileged operator key, rotation, and secret scanning. |

### P2 - Performance and Strategy Improvements

| Area | Current limitation | Recommended change |
| --- | --- | --- |
| Snapshot publishing | Uses `RwLock<Arc<GraphSnapshot>>`; design doc mentions `arc-swap`, but dependency is absent. | Use `arc-swap` for lock-free snapshot reads. |
| Snapshot rebuild | Refresh clones/rebuilds all pools and indexes. | Use structural sharing and incremental per-pool edge updates. |
| Distance cache | Recomputed every refresh. | Incremental reachability or bitset-based bounded-hop reachability. |
| Quote cache | Clears all entries when above 20,000. | Use per-snapshot segmented LRU with hit-rate metrics and no full-cache flush. |
| Gas model | Static hop gas estimates before `estimateGas`. | Maintain empirical gas model per adapter/path and compare to estimateGas. |
| Price inputs | Manual prices are required for gas/risk valuation. | Add signed price feed/oracle service with staleness and deviation checks. |

## Recommended Alchemy Cost Architecture

### Target steady-state flow

1. WSS log subscription receives pool events.
2. HTTP `eth_getLogs` is used only for reconnect/downtime gap repair.
3. Local AMM state applies patches.
4. Detector enumerates all changed-edge cycles with economic branch-and-bound.
5. Router quotes all candidates locally.
6. Split allocator solves local optimal allocation across all non-dominated pools.
7. Validator sends one final simulation for the best executable route.
8. Submitter sends privately/protected.
9. Receipt monitor polls with adaptive interval and stops after inclusion/replacement.

### Provider split

Recommended provider roles:

- Alchemy Pay As You Go: WSS event feed, HTTP gap fill, final simulation, submit fallback, receipts.
- Local Reth/Erigon or indexed archive service: historical bootstrap, replay, V3 tick indexing, full-universe state rebuilds.
- Optional second RPC provider: independent final simulation and failover for provider-specific errors.

This is the most important cost point: full-universe arbitrage should not use paid RPC as the inner quote loop.

### CU budget manager

Add a central `RpcScheduler` around `RpcClient::request`.

Required behavior:

- Method CU loaded from config.
- Per-provider token bucket in CU/s.
- Per-chain daily/monthly budget.
- Priority queues.
- 429/403/5xx classification.
- Retry only idempotent reads.
- Jittered exponential backoff.
- Request timeout by class.
- Circuit breaker per provider and method.
- Metrics:
  - `rpc_cu_total{chain,provider,method,priority}`
  - `rpc_requests_total{chain,provider,method,status}`
  - `rpc_429_total`
  - `rpc_budget_remaining`
  - `rpc_queue_depth{priority}`
  - `rpc_latency_seconds`

### Event ingestion decision rule

At runtime, estimate:

```text
address_logs_cu_s = blocks_s * ceil(watched_addresses / address_chunk_size) * 60
topic_logs_cu_s   = blocks_s * 60
wss_cu_s          = delivered_events_s * avg_event_bytes * 0.04
```

Then choose:

- WSS if delivered events are sparse and connection is healthy.
- Topic logs if event density is acceptable and address chunking is too expensive.
- Address logs if response volume from topic logs is too high and address count is small.
- Block receipts only for short ranges where it is cheaper and supported.

Always perform HTTP gap fill after reconnect using the last canonical processed block.

## Concrete Code-Level Findings

### `src/rpc/mod.rs`

Findings:

- No request timeout on the `reqwest::Client`.
- No retries or fallback reads.
- No 429 handling.
- CU weights are hard-coded and incomplete for bandwidth-priced subscriptions and throughput CU.
- JSON-RPC batch is not implemented. Be careful: Alchemy charges batch as method CU times method count, so JSON-RPC batch is mostly latency optimization, not CU optimization.

Recommended changes:

- Add `RpcScheduler`.
- Add timeout defaults by request class.
- Add `request_with_policy`.
- Add provider capabilities and fallback routing.
- Add an official-CU config file with version/date.

### `src/discovery/factory_scanner.rs`

Findings:

- `DISCOVERY_LOG_CHUNK_BLOCKS=50000` default is unsafe for Polygon Pay As You Go.
- DEX `start_block=0` causes expensive first bootstrap when cache is absent.
- Cache saves are non-atomic.

Recommended changes:

- Use deployment block numbers.
- Enforce chain-aware chunk caps.
- Add adaptive split-on-range-error.
- Write caches atomically.
- Allow bootstrapping from signed pool snapshots.

### `src/discovery/event_stream.rs`

Findings:

- Default `address_logs` cost grows with watched addresses.
- WSS starts once; if the subscription stops, there is no reconnect supervisor.
- WSS filter address set is fixed at start and is not refreshed when the pool universe changes.
- Block parent/reorg validation is not wired.

Recommended changes:

- Add reconnecting WSS supervisor.
- Rebuild WSS subscription when watched address set changes.
- Track last processed canonical block/hash.
- Add dynamic ingest mode selection.
- Use HTTP gap fill after WSS reconnect.

### `src/router/exact_quoter.rs`

Findings:

- V3 quoter RPC is on by default.
- Curve exact quotes call RPC per amount.
- Cache clears everything when length exceeds 20,000.

Recommended changes:

- Disable V3 RPC quoter in production hot path after local V3 math lands.
- Implement exact local V3 and Curve math.
- Replace full flush with segmented LRU.
- Add quote cache hit/miss metrics.

### `src/router/split_optimizer.rs`

Findings:

- Greedy slice allocation is not globally optimal.
- `max_split_parallel_pools` limits search coverage.
- It may call exact quote repeatedly for nearby allocations.

Recommended changes:

- Use marginal-price equalization allocator across all non-dominated pools.
- Cache marginal curves per pool and snapshot.
- Keep arbitrary caps only as emergency circuit-breaker controls, not normal optimization.

### `src/detector/path_finder.rs`

Findings:

- Beam/top-k settings can discard possible profitable cycles.
- This is a search-space limitation, not a pure optimization.

Recommended changes:

- Replace beam limits with complete changed-edge cycle enumeration and admissible economic pruning.
- Use bitset visited sets and precomputed best-edge bounds.
- Keep all non-dominated candidates until local exact valuation.

### `src/engine.rs`

Findings:

- Processes candidates sequentially and can work on stale candidates after state changes.
- Snapshot rollback is not used.
- `poll_from_block = latest_block` can skip reorg awareness because only block numbers are tracked in the polling cursor.

Recommended changes:

- Add opportunity priority queue with TTL.
- Before submit, require fresh snapshot and final simulation at current target state.
- Track block hash/parent hash and implement rollback/replay.

### `contracts/ArbitrageExecutor.sol`

Findings:

- The executor is compact and understandable, but production safety depends heavily on target allowlisting.
- V3 callback trusts `_checkTarget(msg.sender)`. If allowlist setup is incomplete, callback safety weakens.
- Deployment scripts do not configure operator, allowlist, strict mode, pause state, or ownership transfer.
- Tests do not yet cover real V3/Curve/Balancer paths on forks.

Recommended changes:

- Add canonical factory/codehash verification or enforce complete target allowlist automation.
- Add post-deploy script:
  - set operator
  - set allowed pools/vaults
  - enable strict allowlist
  - transfer owner to Safe
  - verify contract
  - emit/save deployment manifest
- Add fork tests and fuzz tests.

## Production Checklist

### Engineering

- Add CI: fmt, clippy, test, forge test, coverage, cargo audit, deny unknown lints.
- Add Dockerfile and release image.
- Add environment validation for all chain/provider mode combinations.
- Add runbooks for:
  - RPC outage
  - WSS reconnect storm
  - 429/rate limit
  - reorg
  - stuck nonce
  - reverted transaction
  - contract pause/rescue
- Add structured deployment manifests.

### Security

- Use Safe multisig owner.
- Use remote signer/KMS/HSM for operator key.
- Never keep production private keys in `.env` on disk.
- Add key rotation and emergency revoke process.
- Audit executor contract before mainnet capital.
- Add `slither`, `forge fmt`, `forge coverage`, and fuzz/property tests.
- Add token behavior policy for fee-on-transfer, rebasing, ERC4626, callback, and rate-oraclized tokens. `TokenBehavior` exists but is not actively enforced in the production path.

### Runtime

- Private/protected submit by default.
- Public fallback disabled unless explicitly enabled per chain and route class.
- Final simulation against current state immediately before signing.
- Nonce replacement and cancellation.
- Receipt reconciliation on restart.
- PnL reconciliation from on-chain events, not just expected profit.
- Stable/depeg guard integrated into detector/risk.
- Gas fee model based on current chain behavior plus empirical adapter/path history.

### Data and State

- Local state indexer for V3 ticks, Curve parameters, Balancer pool details, and pool universe snapshots.
- Atomic cache writes.
- Schema migration for state files.
- Reorg-aware block log replay.
- Persistent event cursors with block hash.
- Backtest/replay tooling from historical blocks.

### Alchemy Cost Controls

- CU budget manager.
- Method-level CU config.
- Dynamic event ingest selection.
- Chain-aware log chunk caps.
- WSS reconnect with HTTP gap fill.
- Local quote engine.
- Final-simulation-only RPC hot path.
- Dashboard alerts:
  - CU/s > budget
  - monthly projected CU > budget
  - 429 rate > 0
  - quote RPC count > threshold
  - event gap depth > threshold

## Recommended Implementation Order

1. Add chain-aware `eth_getLogs` chunk limits and RPC timeouts/retries.
2. Add CU budget manager and method-priority scheduler.
3. Switch event ingestion to supervised WSS plus HTTP gap fill for production.
4. Add real deployment block numbers and signed pool-universe snapshots.
5. Build local V3 exact quote engine and tick indexer.
6. Build local Curve exact quote engine for supported pool classes.
7. Replace split optimizer with all-pool marginal allocator.
8. Replace detector beam/top-k limits with complete changed-edge cycle enumeration plus economic branch-and-bound.
9. Wire reorg detection and snapshot rollback/replay.
10. Harden submitter: private-only policy, nonce replacement, final simulation parsing, receipt reconciliation.
11. Add fork/load/reorg/rate-limit tests and CI.
12. Add production deployment scripts with Safe ownership and strict allowlist automation.

## Verdict

The current project is suitable for simulation, research, and controlled fork testing. It should not be used with meaningful production capital until P0 items are complete.

For Alchemy Pay As You Go, the most important optimization is architectural: stop using paid RPC as an inner-loop quote engine. Keep the search space full, but make the state and math local. Then use Alchemy for event delivery, gap repair, final simulation, transaction submission, and reconciliation. That is the path to both full-universe search and controlled API cost.
