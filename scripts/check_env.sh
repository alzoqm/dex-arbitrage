#!/usr/bin/env bash
set -euo pipefail

source .env
CHAIN="${1:-${CHAIN:-base}}"
MODE="${2:-verify}"

common_verify=(OPERATOR_PRIVATE_KEY)
common_deploy=(DEPLOYER_PRIVATE_KEY SAFE_OWNER)
base_verify=(BASE_PUBLIC_RPC_URL BASE_AAVE_POOL BASE_USDC BASE_USDT BASE_WETH BASE_DAI BASE_CBETH BASE_UNISWAP_V2_FACTORY BASE_SUSHISWAP_V2_FACTORY BASE_BASESWAP_FACTORY BASE_AERODROME_V2_FACTORY BASE_UNISWAP_V3_FACTORY BASE_AERODROME_V3_LEGACY_FACTORY BASE_AERODROME_V3_CAPS_FACTORY BASE_AERODROME_V3_FACTORY BASE_UNISWAP_V3_QUOTER BASE_AERODROME_V3_LEGACY_QUOTER BASE_AERODROME_V3_CAPS_QUOTER BASE_AERODROME_V3_QUOTER BASE_CURVE_REGISTRY BASE_BALANCER_VAULT BASE_WETH_PRICE_E8)
base_run=(BASE_EXECUTOR_ADDRESS)
polygon_verify=(POLYGON_PUBLIC_RPC_URL POLYGON_AAVE_POOL POLYGON_USDC POLYGON_USDT POLYGON_WMATIC POLYGON_WETH POLYGON_DAI POLYGON_UNISWAP_V3_FACTORY POLYGON_UNISWAP_V3_QUOTER POLYGON_WMATIC_PRICE_E8 POLYGON_WETH_PRICE_E8)
polygon_run=(POLYGON_EXECUTOR_ADDRESS)

missing=()

require_key() {
  local key="$1"
  local value="${!key:-}"
  if [[ -z "$value" || "$value" == *"확인 필요"* || "$value" == *"추가 세팅 필요"* ]]; then
    missing+=("$key")
  fi
}

case "$CHAIN" in
  base)
    chain_verify=("${base_verify[@]}")
    chain_run=("${base_run[@]}")
    ;;
  polygon)
    chain_verify=("${polygon_verify[@]}")
    chain_run=("${polygon_run[@]}")
    ;;
  *)
    echo "unsupported chain: $CHAIN" >&2
    exit 1
    ;;
esac

case "$MODE" in
  verify)
    keys=("${common_verify[@]}" "${chain_verify[@]}")
    ;;
  run)
    keys=("${common_verify[@]}" "${chain_verify[@]}" "${chain_run[@]}")
    ;;
  deploy)
    keys=("${common_deploy[@]}" "${chain_verify[@]}")
    ;;
  live)
    keys=("${common_verify[@]}" "${chain_verify[@]}" "${chain_run[@]}")
    if [[ "$CHAIN" == "base" ]]; then
      if [[ "${ALLOW_PUBLIC_FALLBACK:-false}" != "true" ]]; then
        keys+=(BASE_PROTECTED_RPC_URL)
      fi
    elif [[ "${ALLOW_PUBLIC_FALLBACK:-false}" != "true" ]]; then
      keys+=(POLYGON_PRIVATE_MEMPOOL_URL)
    fi
    ;;
  *)
    echo "unsupported mode: $MODE" >&2
    echo "usage: $0 [base|polygon] [verify|run|deploy|live]" >&2
    exit 1
    ;;
esac

for key in "${keys[@]}"; do
  require_key "$key"
done

if [[ ${#missing[@]} -gt 0 ]]; then
  printf 'missing envs for %s/%s:\n' "$CHAIN" "$MODE"
  printf ' - %s\n' "${missing[@]}"
  exit 1
fi

echo "env check passed for $CHAIN/$MODE"
