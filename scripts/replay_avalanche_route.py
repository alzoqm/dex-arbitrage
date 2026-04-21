#!/usr/bin/env python3
"""Replay selected Avalanche pools against a local fork executor.

This is a targeted diagnostic tool. It avoids rescanning every candidate by
restricting the initial refresh to `INITIAL_REFRESH_POOL_IDS`.
"""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import time


ROOT = Path(__file__).resolve().parent.parent
STATE_DIR = ROOT / "state" / "chain-scout"
ANVIL_OWNER = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
AVALANCHE_RPC = "https://api.avax.network/ext/bc/C/rpc"
AVALANCHE_AAVE_POOL = "0x794a61358D6845594F94dc1DB02A252b5b4814aD"


def normalize_pool_route(value: str) -> str:
    parts = re.split(r"\s*(?:->|>|,|\+)\s*", value.strip())
    pools = []
    for part in parts:
        part = part.strip()
        if re.fullmatch(r"0x[a-fA-F0-9]{40}", part):
            pools.append(part)
    if not pools:
        raise SystemExit("no pool addresses found in pool route")
    return ",".join(dict.fromkeys(pools))


def pool_route_from_log(path: Path, index: int) -> str:
    rows = []
    for line in path.read_text(errors="replace").splitlines():
        if "pool_route=" not in line:
            continue
        match = re.search(
            r"pool_route=(.*?)(?: verify_| input_token=| hop_count=| net_profit_usd_e8=|$)",
            line,
        )
        if match:
            rows.append(match.group(1).strip())
    if not rows:
        raise SystemExit(f"no pool_route= lines found in {path}")
    try:
        return rows[index]
    except IndexError:
        raise SystemExit(f"pool route index {index} out of range; found {len(rows)}") from None


def run(cmd: list[str], *, env: dict[str, str] | None = None, timeout: int | None = None) -> str:
    return subprocess.check_output(
        cmd,
        cwd=ROOT,
        env=env,
        text=True,
        stderr=subprocess.STDOUT,
        timeout=timeout,
    )


def deploy_executor(anvil_key: str) -> str:
    output = run(
        [
            "forge",
            "create",
            "--broadcast",
            "--chain-id",
            "43114",
            "--rpc-url",
            "http://127.0.0.1:8547",
            "--private-key",
            anvil_key,
            "contracts/ArbitrageExecutor.sol:ArbitrageExecutor",
            "--constructor-args",
            AVALANCHE_AAVE_POOL,
            ANVIL_OWNER,
        ],
        timeout=120,
    )
    match = re.search(r"Deployed to:\s*(0x[a-fA-F0-9]{40})", output)
    if not match:
        raise SystemExit(f"executor deployment did not return address:\n{output}")
    return match.group(1)


def main() -> int:
    parser = argparse.ArgumentParser()
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--pool-route", help="Pool route as comma-separated, arrow-separated, or plus-separated addresses")
    group.add_argument("--from-log", type=Path, help="Read pool_route= from a previous scout log")
    parser.add_argument("--index", type=int, default=0, help="pool_route row index when using --from-log")
    parser.add_argument("--tag", default=time.strftime("%Y%m%d-%H%M%S"))
    parser.add_argument("--timeout-secs", type=int, default=240)
    parser.add_argument(
        "--anvil-private-key",
        default=os.environ.get("ANVIL_PRIVATE_KEY", ""),
        help="Private key for a funded anvil account. Can also be set via ANVIL_PRIVATE_KEY.",
    )
    args = parser.parse_args()

    if not args.anvil_private_key:
        raise SystemExit("ANVIL_PRIVATE_KEY or --anvil-private-key is required")

    raw_route = args.pool_route if args.pool_route else pool_route_from_log(args.from_log, args.index)
    pool_ids = normalize_pool_route(raw_route)

    STATE_DIR.mkdir(parents=True, exist_ok=True)
    log_path = STATE_DIR / f"avalanche-targeted-replay-{args.tag}.log"
    anvil_log_path = STATE_DIR / f"anvil-avalanche-targeted-{args.tag}.log"

    with anvil_log_path.open("w") as anvil_log:
        anvil = subprocess.Popen(
            [
                "anvil",
                "--fork-url",
                AVALANCHE_RPC,
                "--chain-id",
                "43114",
                "--port",
                "8547",
                "--host",
                "127.0.0.1",
            ],
            cwd=ROOT,
            stdout=anvil_log,
            stderr=subprocess.STDOUT,
            text=True,
        )
    try:
        time.sleep(5)
        executor = deploy_executor(args.anvil_private_key)
        env = dict(os.environ)
        env.update(
            {
                "CHAIN": "avalanche",
                "SIMULATION_ONLY": "true",
                "POLICY_VENUES": "",
                "OPERATOR_PRIVATE_KEY": args.anvil_private_key,
                "SAFE_OWNER": ANVIL_OWNER,
                "AVALANCHE_EXECUTOR_ADDRESS": executor,
                "AVALANCHE_PUBLIC_RPC_URL": "http://127.0.0.1:8547",
                "AVALANCHE_PROTECTED_RPC_URL": "http://127.0.0.1:8547",
                "AVALANCHE_SIMULATE_METHOD": "eth_call",
                "INITIAL_REFRESH_POOL_IDS": pool_ids,
                "SKIP_INITIAL_REFRESH": "false",
                "DERIVE_POOL_PRICES": "true",
                "REQUIRE_TRUSTED_ROUTE_TOKENS": "false",
                "EXECUTION_EXCLUDED_SYMBOL_CONTAINS": "test,fake,mock,scam",
                "BOOTSTRAP_REPLAY_CACHED_POOL_STATES": "false",
                "EVENT_BACKFILL_BLOCKS": "0",
                "STALENESS_TIMEOUT_MS": "31536000000",
                "DISCOVERY_FETCH_UNSEEN_POOLS": "false",
                "SEARCH_MAX_CANDIDATES_PER_REFRESH": "256",
                "SEARCH_CANDIDATE_SELECTION_BUFFER_MULTIPLIER": "8",
                "SEARCH_TOP_K_PATHS_PER_SIDE": "64",
                "SEARCH_MAX_VIRTUAL_BRANCHES_PER_NODE": "128",
                "SEARCH_PATH_BEAM_WIDTH": "2048",
                "SEARCH_DEDUP_TOKEN_PATHS": "false",
                "SEARCH_MAX_PAIR_EDGES_PER_PAIR": "16",
                "AVALANCHE_MIN_TRADE_USD_E8": "1000000",
                "AVALANCHE_MIN_NET_PROFIT_USD_E8": "1000",
                "ROUTE_CANDIDATE_CONCURRENCY": "8",
                "ROUTE_VALIDATION_TOP_K": "8",
                "ROUTE_VERIFY_SCAN_LIMIT": "16",
                "ROUTE_SEARCH_TIMEOUT_MS": "30000",
                "EXACT_QUOTE_TIMEOUT_MS": "3000",
                "NO_ROUTE_SAMPLE_LOG_LIMIT": "0",
                "PROMETHEUS_BIND": "127.0.0.1:0",
                "RUST_LOG": "info",
            }
        )
        with log_path.open("w") as log:
            proc = subprocess.run(
                ["target/release/dex-arbitrage", "--chain", "avalanche", "--simulate-only", "--once"],
                cwd=ROOT,
                env=env,
                stdout=log,
                stderr=subprocess.STDOUT,
                text=True,
                timeout=args.timeout_secs,
            )
        print(f"exit_code={proc.returncode}")
        print(f"executor={executor}")
        print(f"pool_ids={pool_ids}")
        print(f"log={log_path}")
        summary = run(
            [
                "rg",
                "-n",
                "candidate detection complete|route ranking complete|validated opportunity|simulation failed|ranked route lost|profitability refresh summary",
                str(log_path),
            ],
            timeout=30,
        )
        print(summary)
        return proc.returncode
    finally:
        anvil.send_signal(signal.SIGINT)
        try:
            anvil.wait(timeout=5)
        except subprocess.TimeoutExpired:
            anvil.kill()


if __name__ == "__main__":
    raise SystemExit(main())
