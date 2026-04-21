# Avalanche Live Setup

This file tracks the Avalanche migration target and the minimum steps required before live trading.

## Current Result

Avalanche is the only tested alternative chain that produced meaningful candidates.

Latest scout results:

```text
Avalanche detection:
  pools=56,427
  changed_edges=108,869
  candidates=149

Avalanche wide scout:
  candidates=1463
  routed=29
  route/risk/gas candidates=8

Avalanche fork executor simulation:
  simulated=0
  representative failure=Joe: TRANSFER_FAILED
```

Interpretation:

```text
Avalanche is the best current direction.
But live-ready execution is not cleared yet.
The current blocker is TraderJoe V1 execution compatibility and/or long-tail token transfer behavior.
```

## Funding Status

2026-04-21 funding move:

```text
Source: Base operator ETH
Bridge/swap route: LI.FI / NEAR route
Base transaction:
0xf52525a6a723a66c8c4abaa9a6bfb581fe19fc2fe9046e016e149f2e2604196f

Avalanche operator -> deployer AVAX transfer:
0x67c008be8353aaa7b7fd340543b2663e8d15e515f4ca53d7526b43a08a3a5f25
```

Balances after funding:

```text
Base operator ETH:   0.006306487946367934
Base deployer ETH:   0.005966672036390757
Avalanche operator: 10.533675269010233563 AVAX
Avalanche deployer:  4.500000000000000000 AVAX
```

Do not deploy the Avalanche executor yet unless the next deployed bytecode is final for the
TraderJoe V1/LB execution path. The current fork simulation showed execution failures, so deploying
before the adapter fix may waste gas.

## Supported Scope Added

Config:

```text
config/avalanche.toml
```

Runtime scripts:

```bash
./scripts/run_avalanche.sh
./scripts/deploy_avalanche.sh
python3 scripts/scout_chains.py --chains avalanche
```

Initial DEX scope:

```text
Uniswap V2
Pangolin V2
Trader Joe V1
Trader Joe Liquidity Book V2.1/V2.2
```

Initial token scope:

```text
USDC
USDT
WAVAX
BTCb
EURC
```

## Required `.env`

Public/static values:

```env
AVALANCHE_PUBLIC_RPC_URL=https://api.avax.network/ext/bc/C/rpc
AVALANCHE_FALLBACK_RPC_URL=
AVALANCHE_PRECONF_RPC_URL=
AVALANCHE_WSS_URL=
AVALANCHE_PROTECTED_RPC_URL=
AVALANCHE_PRIVATE_SUBMIT_METHOD=eth_sendRawTransaction
AVALANCHE_SIMULATE_METHOD=eth_call

AVALANCHE_AAVE_POOL=0x794a61358D6845594F94dc1DB02A252b5b4814aD

AVALANCHE_USDC=0xB97EF9Ef8734C71904D8002F8b6Bc66Dd9c48a6E
AVALANCHE_USDT=0x9702230A8Ea53601f5cD2dc00fDBc13d4dF4A8c7
AVALANCHE_WAVAX=0xB31f66AA3C1e785363F0875A1B74E27b85FD66c7
AVALANCHE_BTCB=0x152b9d0FdC40C096757F570A51E494bd4b943E50
AVALANCHE_EURC=0xC891EB4cbdEFf6e073e859e987815Ed1505c2ACD

AVALANCHE_UNISWAP_V2_FACTORY=0x9e5A52f57b3038F1B8EeE45F28b3C1967e22799C
AVALANCHE_PANGOLIN_V2_FACTORY=0xefa94DE7a4656D787667C749f7E1223D71E9FD88
AVALANCHE_TRADERJOE_V1_FACTORY=0x9Ad6C38BE94206cA50bb0d90783181662f0Cfa10
AVALANCHE_TRADERJOE_LB_V22_FACTORY=0xb43120c4745967fa9b93E79C149E66B0f2D6Fe0c
AVALANCHE_TRADERJOE_LB_V21_FACTORY=0x8e42f2F4101563bF679975178e880FD87d3eFd4e
AVALANCHE_TRADERJOE_LB_BIN_STEPS=1,2,5,10,15,20,25,50,100

AVALANCHE_USDC_PRICE_E8=100000000
AVALANCHE_USDT_PRICE_E8=100000000
AVALANCHE_EURC_PRICE_E8=108000000
AVALANCHE_WAVAX_PRICE_E8=<set current AVAX/USD e8>
AVALANCHE_BTCB_PRICE_E8=<set current BTC/USD e8>
```

Deployment values:

```env
SAFE_OWNER=<Avalanche Safe address or temporary owner address>
DEPLOYER_PRIVATE_KEY=<funded Avalanche deployer private key>
OPERATOR_PRIVATE_KEY=<funded Avalanche operator private key>
AVALANCHE_EXECUTOR_ADDRESS=<filled after deployment>
```

## Verify Env

```bash
source .env
./scripts/check_env.sh avalanche verify
```

Before deployment:

```bash
cast chain-id --rpc-url "$AVALANCHE_PUBLIC_RPC_URL"
cast wallet address --private-key "$DEPLOYER_PRIVATE_KEY"
cast balance "$(cast wallet address --private-key "$DEPLOYER_PRIVATE_KEY")" \
  --rpc-url "$AVALANCHE_PUBLIC_RPC_URL" \
  --ether
```

Expected chain ID:

```text
43114
```

## Deploy Executor

The deployer must hold AVAX on Avalanche C-Chain.

```bash
source .env
forge build
./scripts/deploy_avalanche.sh
```

After deployment:

```env
AVALANCHE_EXECUTOR_ADDRESS=<deployed address>
```

Verify:

```bash
cast code "$AVALANCHE_EXECUTOR_ADDRESS" --rpc-url "$AVALANCHE_PUBLIC_RPC_URL"
cast call "$AVALANCHE_EXECUTOR_ADDRESS" "owner()(address)" --rpc-url "$AVALANCHE_PUBLIC_RPC_URL"
cast call "$AVALANCHE_EXECUTOR_ADDRESS" "aavePool()(address)" --rpc-url "$AVALANCHE_PUBLIC_RPC_URL"
```

## Important Blocker

Do not move to live yet.

The current executor fork test failed with:

```text
Joe: TRANSFER_FAILED on V1 long-tail route
MIN_PROFIT after LB exact verification/fork execution
```

This means candidates exist, but the current execution path is not live-safe on Avalanche yet.

Next code task:

```text
Improve Trader Joe LB sizing/quote accuracy.
V1 long-tail routes should stay filtered unless proven executable.
```

## LB Adapter Status

Implemented:

```text
TraderJoeLb AMM kind
configured-token LB pair discovery
LB pool state fetch
LB direct pair execution in ArbitrageExecutor
LB exact verification through pair.getSwapOut
LB split minOut relaxation with final MIN_PROFIT protection
```

Latest LB-only fork result:

```text
candidate_count=15
routed_count=15
exact verification removed all profitable candidates
simulated=0
```

Interpretation:

```text
LB execution path is wired, but current sizing still overestimates profitability.
No live Avalanche deployment yet.
```

## Scout Commands

Detection only:

```bash
python3 scripts/scout_chains.py --chains avalanche --timeout-secs 900
```

Wide route/risk scout:

```bash
python3 scripts/scout_chains.py \
  --chains avalanche \
  --route \
  --timeout-secs 900 \
  --max-candidates 2048 \
  --buffer-multiplier 24 \
  --top-k-paths 128 \
  --branches 256 \
  --beam-width 8192 \
  --pair-edges 32 \
  --min-trade-usd-e8 1000000 \
  --min-profit-usd-e8 1000
```

Targeted fork replay from a previous log:

```bash
python3 scripts/replay_avalanche_route.py \
  --from-log state/chain-scout/<log-with-pool-route>.log \
  --index 0
```

Targeted fork replay from explicit pools:

```bash
python3 scripts/replay_avalanche_route.py \
  --pool-route 0xPoolA,0xPoolB,0xPoolC
```
