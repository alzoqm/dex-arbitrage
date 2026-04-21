# Base Provider Compare

This comparison keeps the Base trading/search settings fixed and only swaps the
provider-facing Base RPC variables.

## 1. Fill `.env`

Keep the current generic Base values as your current baseline, then add these:

```env
BASE_ALCHEMY_PUBLIC_RPC_URL=
BASE_ALCHEMY_PRECONF_RPC_URL=
BASE_ALCHEMY_WSS_URL=
BASE_ALCHEMY_PROTECTED_RPC_URL=
BASE_ALCHEMY_SIMULATE_METHOD=

BASE_QUICKNODE_PUBLIC_RPC_URL=
BASE_QUICKNODE_PRECONF_RPC_URL=
BASE_QUICKNODE_WSS_URL=
BASE_QUICKNODE_PROTECTED_RPC_URL=
BASE_QUICKNODE_SIMULATE_METHOD=
```

Recommended first pass:

```env
# Alchemy baseline
BASE_ALCHEMY_PUBLIC_RPC_URL=<your alchemy base https url>
BASE_ALCHEMY_PRECONF_RPC_URL=<leave empty unless you have a distinct preconfirm url>
BASE_ALCHEMY_WSS_URL=<your alchemy base wss url>
BASE_ALCHEMY_PROTECTED_RPC_URL=<same as public unless you have a distinct protected url>
BASE_ALCHEMY_SIMULATE_METHOD=eth_call

# QuickNode baseline
BASE_QUICKNODE_PUBLIC_RPC_URL=<your quicknode base https url>
BASE_QUICKNODE_PRECONF_RPC_URL=<your quicknode flashblocks https url or same endpoint if supported>
BASE_QUICKNODE_WSS_URL=<your quicknode base wss url>
BASE_QUICKNODE_PROTECTED_RPC_URL=<same as public unless you have a distinct protected url>
BASE_QUICKNODE_SIMULATE_METHOD=eth_simulateV1
```

Notes:

- If `BASE_ALCHEMY_*` is omitted, the compare script falls back to the current generic `BASE_*` values.
- QuickNode values are required for the `quicknode` run.
- If a provider has no usable WSS endpoint for your plan, leave that provider WSS empty and the script will run without `BASE_WSS_URL`.

## 2. Run the comparison

```bash
python3 scripts/compare_base_providers.py --duration-secs 120
```

You can also run only one side:

```bash
python3 scripts/compare_base_providers.py --providers quicknode --duration-secs 120
python3 scripts/compare_base_providers.py --providers alchemy --duration-secs 120
```

## 3. Output

The script writes:

- per-provider logs under `state/provider-compare/`
- one JSON summary under `state/provider-compare/`

Summary columns:

- `bootstrap_s`: time until `bootstrap complete`
- `snapshots`: number of `candidate detection complete` rows
- `snapshots_with_candidates`: snapshots where `candidate_count > 0`
- `max_candidate_count`
- `avg_candidate_count`
- `avg_detect_ms`
- `rpc_requests`
- `rpc_cu`
- `warns`
- `errors`

By default, the script disables cached-pool replay and event backfill during
comparison:

```env
BOOTSTRAP_REPLAY_CACHED_POOL_STATES=false
EVENT_BACKFILL_BLOCKS=0
STALENESS_TIMEOUT_MS=31536000000
DISCOVERY_LOG_CHUNK_BLOCKS=5
DISCOVERY_LOG_MIN_CHUNK_BLOCKS=1
EVENT_LOG_CHUNK_BLOCKS=5
EVENT_LOG_MIN_CHUNK_BLOCKS=1
```

This is intentional. Provider comparison should measure live-search latency and
cost, not the time required to replay an old local cache anchor through many
historical blocks. The log chunk size is also capped at 5 blocks so QuickNode
Free/Discover endpoints can complete `eth_getLogs` requests.

To include replay/backfill explicitly:

```bash
python3 scripts/compare_base_providers.py --duration-secs 120 --with-replay
```

## 4. How to judge the winner

For this project, the better provider is not simply the cheaper one.

The priority is:

1. more `snapshots_with_candidates`
2. lower `avg_detect_ms`
3. lower `bootstrap_s`
4. lower `rpc_cu` for the same candidate opportunity rate
5. fewer warnings/errors

If QuickNode produces the same or better candidate rate with lower detection
latency, it is the better live-search path even if raw RPC cost is similar.

If QuickNode produces no better candidate rate than Alchemy, keep Alchemy for
baseline read traffic and continue optimizing the local exact-quote path first.
