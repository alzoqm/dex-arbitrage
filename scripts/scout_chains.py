#!/usr/bin/env python3
"""Scout candidate availability across supported chains.

The script runs one initial-refresh pass per chain in simulation mode. By
default it stops immediately after candidate detection so we can compare whether
the chain produces any search candidates without spending time on exact routing.
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
STATE_DIR = ROOT / "state" / "chain-scout"

CANDIDATE_RE = re.compile(
    r"candidate detection complete .*?changed_edge_count=(?P<edges>\d+).*?candidate_count=(?P<candidates>\d+).*?detect_ms=(?P<detect>\d+)"
)
BOOTSTRAP_RE = re.compile(r"bootstrap complete .*?token_count=(?P<tokens>\d+).*?pool_count=(?P<pools>\d+)")
ROUTE_RE = re.compile(r"route ranking complete .*?routed_count=(?P<routed>\d+).*?best_estimated_net_profit_usd_e8=(?P<best>-?\d+)")
SUMMARY_RE = re.compile(
    r"profitability refresh summary .*?candidate_count=(?P<candidates>\d+).*?no_route=(?P<no_route>\d+).*?simulated=(?P<simulated>\d+).*?submitted=(?P<submitted>\d+).*?refresh_total_ms=(?P<refresh>\d+)"
)


@dataclass(frozen=True)
class ChainScout:
    chain: str
    env: dict[str, str]


SCOUTS: dict[str, ChainScout] = {
    "avalanche": ChainScout(
        "avalanche",
        {
            "AVALANCHE_PUBLIC_RPC_URL": "https://api.avax.network/ext/bc/C/rpc",
            "AVALANCHE_AAVE_POOL": "0x794a61358D6845594F94dc1DB02A252b5b4814aD",
            "AVALANCHE_USDC": "0xB97EF9Ef8734C71904D8002F8b6Bc66Dd9c48a6E",
            "AVALANCHE_USDT": "0x9702230A8Ea53601f5cD2dc00fDBc13d4dF4A8c7",
            "AVALANCHE_WAVAX": "0xB31f66AA3C1e785363F0875A1B74E27b85FD66c7",
            "AVALANCHE_BTCB": "0x152b9d0FdC40C096757F570A51E494bd4b943E50",
            "AVALANCHE_EURC": "0xC891EB4cbdEFf6e073e859e987815Ed1505c2ACD",
            "AVALANCHE_UNISWAP_V2_FACTORY": "0x9e5A52f57b3038F1B8EeE45F28b3C1967e22799C",
            "AVALANCHE_PANGOLIN_V2_FACTORY": "0xefa94DE7a4656D787667C749f7E1223D71E9FD88",
            "AVALANCHE_TRADERJOE_V1_FACTORY": "0x9Ad6C38BE94206cA50bb0d90783181662f0Cfa10",
            "AVALANCHE_TRADERJOE_LB_V22_FACTORY": "0xb43120c4745967fa9b93E79C149E66B0f2D6Fe0c",
            "AVALANCHE_TRADERJOE_LB_V22_ROUTER": "0x18556DA13313f3532c54711497A8FedAC273220E",
            "AVALANCHE_TRADERJOE_LB_V22_QUOTER": "0x9A550a522BBaDFB69019b0432800Ed17855A51C3",
            "AVALANCHE_TRADERJOE_LB_V21_FACTORY": "0x8e42f2F4101563bF679975178e880FD87d3eFd4e",
            "AVALANCHE_TRADERJOE_LB_V21_ROUTER": "0xb4315e873dBcf96Ffd0acd8EA43f689D8c20fB30",
            "AVALANCHE_TRADERJOE_LB_V21_QUOTER": "0xd76019A16606FDa4651f636D9751f500Ed776250",
            "AVALANCHE_TRADERJOE_LB_BIN_STEPS": "1,2,5,10,15,20,25,50,100",
            "AVALANCHE_UNISWAP_V3_FACTORY": "0x740b1c1de25031C31FF4fC9A62f554A55cdC1baD",
            "AVALANCHE_UNISWAP_V3_QUOTER": "0xbe0F5544EC67e9B3b2D979aaA43f18Fd87E6257F",
            "AVALANCHE_USDC_PRICE_E8": "100000000",
            "AVALANCHE_USDT_PRICE_E8": "100000000",
            "AVALANCHE_EURC_PRICE_E8": "108000000",
        },
    ),
    "sonic": ChainScout(
        "sonic",
        {
            "SONIC_PUBLIC_RPC_URL": "https://rpc.soniclabs.com",
            "SONIC_AAVE_POOL": "0x5362dBb1e601abF3a4c14c22ffEdA64042E5eAA3",
            "SONIC_USDC": "0x29219dd400f2Bf60E5a23d13Be72B486D4038894",
            "SONIC_WETH": "0x50c42dEAcD8Fc9773493ED674b675bE577f2634b",
            "SONIC_WS": "0x039e2fB66102314Ce7b64Ce5Ce3E5183bc94aD38",
            "SONIC_SPOOKY_V2_FACTORY": "0xEE4bC42157cf65291Ba2FE839AE127e3Cc76f741",
            "SONIC_SPOOKY_V3_FACTORY": "0x3D91B700252e0E3eE7805d12e048a988Ab69C8ad",
            "SONIC_SPOOKY_V3_QUOTER": "0x3F2026Cae76b987C4002e62B9dF70988b4388234",
            "SONIC_USDC_PRICE_E8": "100000000",
        },
    ),
    "gnosis": ChainScout(
        "gnosis",
        {
            "GNOSIS_PUBLIC_RPC_URL": "https://rpc.gnosischain.com",
            "GNOSIS_AAVE_POOL": "0xb50201558B00496A145fE76f7424749556E326D8",
            "GNOSIS_USDC": "0xDDAfbb505ad214D7b80b1f830fcCc89B60fb7A83",
            "GNOSIS_WXDAI": "0xe91D153E0b41518A2Ce8Dd3D7944Fa863463a97d",
            "GNOSIS_WETH": "0x6A023CCd1ff6F2045C3309768eAd9E68F978f6e1",
            "GNOSIS_EURE": "0xcB444e90D8198415266c6a2724b7900fb12FC56E",
            "GNOSIS_WSTETH": "0x6C76971f98945AE98dD7d4DFcA8711ebea946eA6",
            "GNOSIS_HONEYSWAP_V2_FACTORY": "0xA818b4F111Ccac7AA31D0BCc0806d64F2E0737D7",
            "GNOSIS_SUSHISWAP_V3_FACTORY": "0xf78031CBCA409F2FB6876BDFDBc1b2df24cF9bEf",
            "GNOSIS_BALANCER_VAULT": "0xBA12222222228d8Ba445958a75a0704d566BF2C8",
            "GNOSIS_USDC_PRICE_E8": "100000000",
            "GNOSIS_WXDAI_PRICE_E8": "100000000",
            "GNOSIS_EURE_PRICE_E8": "108000000",
        },
    ),
    "celo": ChainScout(
        "celo",
        {
            "CELO_PUBLIC_RPC_URL": "https://forno.celo.org",
            "CELO_AAVE_POOL": "0x3E59A31363E2ad014dcbc521c4a0d5757d9f3402",
            "CELO_USDC": "0xcebA9300f2b948710d2653dD7B07f33A8B32118C",
            "CELO_USDT": "0x48065fbBE25f71C9282ddf5e1cD6D6A887483D5e",
            "CELO_WETH": "0xD221812de1BD094f35587EE8E174B07B6167D9Af",
            "CELO_CELO": "0x471EcE3750Da237f93B8E339c536989b8978a438",
            "CELO_UBESWAP_V2_FACTORY": "0x62d5b84be28a183abb507e125b384122d2c25fae",
            "CELO_UNISWAP_V3_FACTORY": "0xAfE208a311B21f13EF87E33A90049fC17A7acDEc",
            "CELO_UNISWAP_V3_QUOTER": "0x82825d0554fA07f7FC52Ab63c961F330fdEFa8E8",
            "CELO_USDC_PRICE_E8": "100000000",
            "CELO_USDT_PRICE_E8": "100000000",
        },
    ),
    "linea": ChainScout(
        "linea",
        {
            "LINEA_PUBLIC_RPC_URL": "https://rpc.linea.build",
            "LINEA_AAVE_POOL": "0xc47b8C00b0f69a36fa203Ffeac0334874574a8Ac",
            "LINEA_USDC": "0x176211869cA2b568f2A7D4EE941E073a821EE1ff",
            "LINEA_USDT": "0xA219439258ca9da29E9Cc4cE5596924745e12B93",
            "LINEA_WETH": "0xe5D7C2a44FfDDf6b295A15c148167daaAf5Cf34f",
            "LINEA_WBTC": "0x3aAB2285ddcDdaD8edf438C1bAB47e1a9D05a9b4",
            "LINEA_WSTETH": "0xB5beDd42000b71FddE22D3eE8a79Bd49A568fC8F",
            "LINEA_UNISWAP_V2_FACTORY": "0x114A43DF6C5f54EBB8A9d70Cd1951D3dD68004c7",
            "LINEA_UNISWAP_V3_FACTORY": "0x31FAfd4889FA1269F7a13A66eE0fB458f27D72A9",
            "LINEA_UNISWAP_V3_QUOTER": "0x58EAD433Ea99708604C4dD7c9B7e80c70976E202",
            "LINEA_PANCAKESWAP_V3_FACTORY": "0x0bFBCF9FA4f9C56B0F40a671Ad40E0805A091865",
            "LINEA_PANCAKESWAP_V3_QUOTER": "0xB048Bbc1Ee6b733FFfCFb9e9CeF7375518e25997",
            "LINEA_USDC_PRICE_E8": "100000000",
            "LINEA_USDT_PRICE_E8": "100000000",
        },
    ),
    "bsc": ChainScout(
        "bsc",
        {
            "BSC_PUBLIC_RPC_URL": "https://bsc-dataseed.binance.org",
            "BSC_AAVE_POOL": "0x6807dc923806fE8Fd134338EABCA509979a7e0cB",
            "BSC_USDT": "0x55d398326f99059fF775485246999027B3197955",
            "BSC_USDC": "0x8AC76a51cc950d9822D68b83fE1Ad97B32Cd580d",
            "BSC_WBNB": "0xbb4CdB9CBd36B01bD1cBaEBF2De08d9173bc095c",
            "BSC_BTCB": "0x7130d2A12B9BCbFAe4f2634d864A1Ee1Ce3Ead9c",
            "BSC_ETH": "0x2170Ed0880ac9A755fd29B2688956BD959F933F8",
            "BSC_FDUSD": "0xc5f0f7b66764F6ec8C8Dff7BA683102295E16409",
            "BSC_APESWAP_V2_FACTORY": "0x0841BD0B734E4F5853f0dD8d7EA041c241fb0Da6",
            "BSC_BISWAP_V2_FACTORY": "0x858E3312ed3A876947EA49d572A7C42DE08af7EE",
            "BSC_USDT_PRICE_E8": "100000000",
            "BSC_USDC_PRICE_E8": "100000000",
            "BSC_FDUSD_PRICE_E8": "100000000",
        },
    ),
}


def load_env_file(path: Path) -> dict[str, str]:
    values: dict[str, str] = {}
    if not path.exists():
        return values
    for raw_line in path.read_text().splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, value = line.split("=", 1)
        value = value.strip()
        if value and len(value) >= 2 and value[0] == value[-1] and value[0] in {"'", '"'}:
            value = value[1:-1]
        values[key.strip()] = value
    return values


def run_chain(scout: ChainScout, args: argparse.Namespace, index: int) -> dict[str, str | int]:
    STATE_DIR.mkdir(parents=True, exist_ok=True)
    tag = args.tag or time.strftime("%Y%m%d-%H%M%S")
    log_path = STATE_DIR / f"{scout.chain}-scout-{tag}.log"
    env = dict(os.environ)
    for key, value in load_env_file(ROOT / ".env").items():
        env.setdefault(key, value)
    env.update(scout.env)
    executor_key = f"{scout.chain.upper()}_EXECUTOR_ADDRESS"
    if not env.get(executor_key):
        env[executor_key] = "0x0000000000000000000000000000000000000001"
    env.update(
        {
            "CHAIN": scout.chain,
            "SIMULATION_ONLY": "true",
            "DIAGNOSTIC_STOP_AFTER_DETECTION": "false" if args.route else "true",
            "SKIP_INITIAL_REFRESH": "false",
            "DERIVE_POOL_PRICES": "true",
            "REQUIRE_TRUSTED_ROUTE_TOKENS": "false",
            "EXECUTION_TRUSTED_SYMBOLS": "",
            "EXECUTION_EXCLUDED_SYMBOL_CONTAINS": args.exclude_symbol_contains,
            "BOOTSTRAP_REPLAY_CACHED_POOL_STATES": "false",
            "EVENT_BACKFILL_BLOCKS": "0",
            "STALENESS_TIMEOUT_MS": "31536000000",
            "DISCOVERY_FETCH_UNSEEN_POOLS": "false",
            "DISCOVERY_LOG_CHUNK_BLOCKS": str(args.log_chunk_blocks),
            "DISCOVERY_LOG_MIN_CHUNK_BLOCKS": "1",
            "POOL_FETCH_MULTICALL_CHUNK_SIZE": str(args.pool_chunk_size),
            "TOKEN_METADATA_MULTICALL_CHUNK_SIZE": str(args.token_chunk_size),
            "SEARCH_MAX_CANDIDATES_PER_REFRESH": str(args.max_candidates),
            "SEARCH_CANDIDATE_SELECTION_BUFFER_MULTIPLIER": str(args.buffer_multiplier),
            "SEARCH_TOP_K_PATHS_PER_SIDE": str(args.top_k_paths),
            "SEARCH_MAX_VIRTUAL_BRANCHES_PER_NODE": str(args.branches),
            "SEARCH_PATH_BEAM_WIDTH": str(args.beam_width),
            "SEARCH_DEDUP_TOKEN_PATHS": "false",
            "SEARCH_MAX_PAIR_EDGES_PER_PAIR": str(args.pair_edges),
            f"{scout.chain.upper()}_MIN_TRADE_USD_E8": str(args.min_trade_usd_e8),
            f"{scout.chain.upper()}_MIN_NET_PROFIT_USD_E8": str(args.min_profit_usd_e8),
            f"{scout.chain.upper()}_MAX_POSITION_USD_E8": str(args.max_position_usd_e8),
            f"{scout.chain.upper()}_MAX_FLASH_LOAN_USD_E8": str(args.max_flash_loan_usd_e8),
            "ROUTE_CANDIDATE_CONCURRENCY": "16",
            "ROUTE_VALIDATION_TOP_K": "8",
            "ROUTE_VERIFY_SCAN_LIMIT": "16",
            "NO_ROUTE_SAMPLE_LOG_LIMIT": "0",
            "PROMETHEUS_BIND": "127.0.0.1:0",
            "RUST_LOG": "info",
        }
    )
    if env.get("POLICY_VENUES"):
        # Policy-limited scouts, such as LB-only probes, must not overwrite the
        # chain-wide production cache with a tiny venue subset.
        env["DISABLE_POOL_STATE_CACHE"] = "true"
    cmd = ["target/release/dex-arbitrage", "--chain", scout.chain, "--simulate-only", "--once"]
    started = time.time()
    with log_path.open("w") as log:
        try:
            proc = subprocess.run(
                cmd,
                cwd=ROOT,
                env=env,
                stdout=log,
                stderr=subprocess.STDOUT,
                timeout=args.timeout_secs,
            )
            exit_code = proc.returncode
        except subprocess.TimeoutExpired:
            exit_code = 124
    elapsed = round(time.time() - started, 3)
    text = log_path.read_text(errors="replace") if log_path.exists() else ""
    boot = BOOTSTRAP_RE.search(text)
    cand = CANDIDATE_RE.search(text)
    route = ROUTE_RE.search(text)
    summary = SUMMARY_RE.search(text)
    return {
        "chain": scout.chain,
        "exit": exit_code,
        "elapsed_s": elapsed,
        "tokens": int(boot.group("tokens")) if boot else 0,
        "pools": int(boot.group("pools")) if boot else 0,
        "edges": int(cand.group("edges")) if cand else 0,
        "candidates": int(cand.group("candidates")) if cand else 0,
        "detect_ms": int(cand.group("detect")) if cand else 0,
        "routed": int(route.group("routed")) if route else 0,
        "simulated": int(summary.group("simulated")) if summary else 0,
        "submitted": int(summary.group("submitted")) if summary else 0,
        "log": str(log_path),
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--chains", nargs="+", default=["avalanche", "sonic", "gnosis", "celo", "linea", "bsc"])
    parser.add_argument("--route", action="store_true", help="continue past detection into route/rank/validator")
    parser.add_argument("--timeout-secs", type=int, default=900)
    parser.add_argument("--tag", default="")
    parser.add_argument("--max-candidates", type=int, default=256)
    parser.add_argument("--buffer-multiplier", type=int, default=8)
    parser.add_argument("--top-k-paths", type=int, default=32)
    parser.add_argument("--branches", type=int, default=96)
    parser.add_argument("--beam-width", type=int, default=768)
    parser.add_argument("--pair-edges", type=int, default=12)
    parser.add_argument("--min-trade-usd-e8", type=int, default=1_000_000_000)
    parser.add_argument("--min-profit-usd-e8", type=int, default=10_000_000)
    parser.add_argument("--max-position-usd-e8", type=int, default=10_000_000_000)
    parser.add_argument("--max-flash-loan-usd-e8", type=int, default=10_000_000_000)
    parser.add_argument("--exclude-symbol-contains", default="test,fake,mock,scam")
    parser.add_argument("--pool-chunk-size", type=int, default=300)
    parser.add_argument("--token-chunk-size", type=int, default=250)
    parser.add_argument("--log-chunk-blocks", type=int, default=2000)
    args = parser.parse_args()

    rows = []
    for index, chain in enumerate(args.chains):
        if chain not in SCOUTS:
            raise SystemExit(f"unsupported scout chain: {chain}")
        rows.append(run_chain(SCOUTS[chain], args, index))

    print("| chain | exit | elapsed_s | tokens | pools | edges | candidates | detect_ms | routed | simulated | submitted | log |")
    print("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |")
    for row in rows:
        print(
            f"| {row['chain']} | {row['exit']} | {row['elapsed_s']} | {row['tokens']} | {row['pools']} | "
            f"{row['edges']} | {row['candidates']} | {row['detect_ms']} | {row['routed']} | "
            f"{row['simulated']} | {row['submitted']} | {row['log']} |"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
