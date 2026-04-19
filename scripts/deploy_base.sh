#!/usr/bin/env bash
set -euo pipefail

source .env

forge create contracts/ArbitrageExecutor.sol:ArbitrageExecutor \
  --rpc-url "$BASE_PUBLIC_RPC_URL" \
  --private-key "$DEPLOYER_PRIVATE_KEY" \
  --broadcast \
  --constructor-args "$BASE_AAVE_POOL" "$SAFE_OWNER"
