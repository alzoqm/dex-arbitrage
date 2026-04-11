#!/usr/bin/env bash
set -euo pipefail

CHAIN="${1:-base}"
source .env

common=(OPERATOR_PRIVATE_KEY DEPLOYER_PRIVATE_KEY SAFE_OWNER)
base=(BASE_PUBLIC_RPC_URL BASE_EXECUTOR_ADDRESS BASE_AAVE_POOL BASE_USDC BASE_USDT BASE_WETH BASE_DAI)
polygon=(POLYGON_PUBLIC_RPC_URL POLYGON_EXECUTOR_ADDRESS POLYGON_AAVE_POOL POLYGON_USDC POLYGON_USDT POLYGON_WMATIC POLYGON_WETH POLYGON_DAI)

missing=()
for key in "${common[@]}"; do
  [[ -n "${!key:-}" ]] || missing+=("$key")
done

case "$CHAIN" in
  base)
    for key in "${base[@]}"; do
      [[ -n "${!key:-}" ]] || missing+=("$key")
    done
    ;;
  polygon)
    for key in "${polygon[@]}"; do
      [[ -n "${!key:-}" ]] || missing+=("$key")
    done
    ;;
  *)
    echo "unsupported chain: $CHAIN" >&2
    exit 1
    ;;
esac

if [[ ${#missing[@]} -gt 0 ]]; then
  printf 'missing envs for %s:\n' "$CHAIN"
  printf ' - %s\n' "${missing[@]}"
  exit 1
fi

echo "env check passed for $CHAIN"
