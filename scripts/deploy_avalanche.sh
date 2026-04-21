#!/usr/bin/env bash
set -euo pipefail

source .env

forge create --broadcast \
  --chain-id 43114 \
  --rpc-url "$AVALANCHE_PUBLIC_RPC_URL" \
  --private-key "$DEPLOYER_PRIVATE_KEY" \
  contracts/ArbitrageExecutor.sol:ArbitrageExecutor \
  --constructor-args "$AVALANCHE_AAVE_POOL" "$SAFE_OWNER"
