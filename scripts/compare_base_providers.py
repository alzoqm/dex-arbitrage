#!/usr/bin/env python3
"""Run identical Base canaries against multiple RPC providers and summarize results.

This script intentionally keeps the trading/search settings unchanged and only swaps
provider-facing Base RPC variables. It is designed for provider comparison, not for
production execution.

Required env keys for provider runs:
  Alchemy:
    BASE_ALCHEMY_PUBLIC_RPC_URL            (falls back to current BASE_PUBLIC_RPC_URL)
    BASE_ALCHEMY_PRECONF_RPC_URL           (optional)
    BASE_ALCHEMY_WSS_URL                   (optional)
    BASE_ALCHEMY_PROTECTED_RPC_URL         (optional)
    BASE_ALCHEMY_SIMULATE_METHOD           (optional)

  QuickNode:
    BASE_QUICKNODE_PUBLIC_RPC_URL          (required)
    BASE_QUICKNODE_PRECONF_RPC_URL         (optional)
    BASE_QUICKNODE_WSS_URL                 (optional)
    BASE_QUICKNODE_PROTECTED_RPC_URL       (optional)
    BASE_QUICKNODE_SIMULATE_METHOD         (optional)

The script writes per-provider logs and a JSON summary under state/provider-compare/.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import signal
import subprocess
import sys
import time
from dataclasses import asdict, dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Iterable


ROOT = Path(__file__).resolve().parent.parent
STATE_DIR = ROOT / "state" / "provider-compare"
LOG_TS_FORMAT = "%Y-%m-%dT%H:%M:%S.%fZ"

TIMESTAMP_RE = re.compile(r"^(?P<ts>\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+Z)")
CANDIDATE_RE = re.compile(
    r"candidate detection complete .*?candidate_count=(?P<count>\d+).*?detect_ms=(?P<detect>\d+)"
)
PROFIT_RE = re.compile(
    r"profitability refresh summary .*?candidate_count=(?P<count>\d+).*?simulated=(?P<simulated>\d+).*?submitted=(?P<submitted>\d+).*?refresh_total_ms=(?P<refresh>\d+)"
)
RPC_RE = re.compile(
    r"rpc usage summary .*?total_requests=(?P<requests>\d+).*?total_cu=(?P<cu>\d+)"
)
BOOTSTRAP_RE = re.compile(r"bootstrap complete ")


@dataclass
class ProviderSpec:
    name: str
    public_rpc_url: str
    preconf_rpc_url: str
    ws_url: str
    protected_rpc_url: str
    simulate_method: str


@dataclass
class RunSummary:
    provider: str
    log_path: str
    duration_secs: int
    bootstrap_secs: float | None
    snapshots: int
    snapshots_with_candidates: int
    max_candidate_count: int
    avg_candidate_count: float
    avg_detect_ms: float
    profitability_rows: int
    max_profitability_candidate_count: int
    simulated_rows: int
    submitted_rows: int
    avg_refresh_total_ms: float
    rpc_summary_rows: int
    rpc_total_requests: int
    rpc_total_cu: int
    warn_count: int
    error_count: int
    exited_cleanly: bool
    exit_code: int | None


def load_env_file(path: Path) -> dict[str, str]:
    values: dict[str, str] = {}
    if not path.exists():
        return values
    for raw_line in path.read_text().splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("export "):
            line = line[len("export ") :]
        if "=" not in line:
            continue
        key, value = line.split("=", 1)
        key = key.strip()
        value = value.strip()
        if value and len(value) >= 2 and value[0] == value[-1] and value[0] in {"'", '"'}:
            value = value[1:-1]
        values[key] = value
    return values


def merged_env() -> dict[str, str]:
    env = dict(os.environ)
    for key, value in load_env_file(ROOT / ".env").items():
        env.setdefault(key, value)
    return env


def env_value(env: dict[str, str], key: str, default: str = "") -> str:
    return env.get(key, default).strip()


def build_provider_specs(env: dict[str, str]) -> list[ProviderSpec]:
    generic_public = env_value(env, "BASE_PUBLIC_RPC_URL")
    generic_preconf = env_value(env, "BASE_PRECONF_RPC_URL")
    generic_wss = env_value(env, "BASE_WSS_URL")
    generic_protected = env_value(env, "BASE_PROTECTED_RPC_URL")
    generic_simulate = env_value(env, "BASE_SIMULATE_METHOD", "eth_simulateV1")

    alchemy = ProviderSpec(
        name="alchemy",
        public_rpc_url=env_value(env, "BASE_ALCHEMY_PUBLIC_RPC_URL", generic_public),
        preconf_rpc_url=env_value(env, "BASE_ALCHEMY_PRECONF_RPC_URL", generic_preconf),
        ws_url=env_value(env, "BASE_ALCHEMY_WSS_URL", generic_wss),
        protected_rpc_url=env_value(env, "BASE_ALCHEMY_PROTECTED_RPC_URL", generic_protected),
        simulate_method=env_value(env, "BASE_ALCHEMY_SIMULATE_METHOD", generic_simulate),
    )
    quicknode = ProviderSpec(
        name="quicknode",
        public_rpc_url=env_value(env, "BASE_QUICKNODE_PUBLIC_RPC_URL"),
        preconf_rpc_url=env_value(env, "BASE_QUICKNODE_PRECONF_RPC_URL"),
        ws_url=env_value(env, "BASE_QUICKNODE_WSS_URL"),
        protected_rpc_url=env_value(env, "BASE_QUICKNODE_PROTECTED_RPC_URL"),
        simulate_method=env_value(env, "BASE_QUICKNODE_SIMULATE_METHOD", generic_simulate),
    )

    providers: list[ProviderSpec] = []
    if alchemy.public_rpc_url:
        providers.append(normalize_provider(alchemy))
    if quicknode.public_rpc_url:
        providers.append(normalize_provider(quicknode))
    return providers


def normalize_provider(spec: ProviderSpec) -> ProviderSpec:
    preconf = spec.preconf_rpc_url or spec.public_rpc_url
    protected = spec.protected_rpc_url or preconf or spec.public_rpc_url
    return ProviderSpec(
        name=spec.name,
        public_rpc_url=spec.public_rpc_url,
        preconf_rpc_url=preconf,
        ws_url=spec.ws_url,
        protected_rpc_url=protected,
        simulate_method=spec.simulate_method or "eth_simulateV1",
    )


def parse_iso(ts: str) -> datetime:
    return datetime.strptime(ts, LOG_TS_FORMAT).replace(tzinfo=timezone.utc)


def read_lines(path: Path) -> list[str]:
    if not path.exists():
        return []
    return path.read_text(errors="replace").splitlines()


def summarize_log(log_path: Path, provider: str, duration_secs: int, exit_code: int | None) -> RunSummary:
    lines = read_lines(log_path)
    first_ts: datetime | None = None
    bootstrap_ts: datetime | None = None
    candidate_counts: list[int] = []
    detect_ms: list[int] = []
    profit_counts: list[int] = []
    refresh_ms: list[int] = []
    simulated_rows = 0
    submitted_rows = 0
    rpc_total_requests = 0
    rpc_total_cu = 0
    rpc_rows = 0
    warn_count = 0
    error_count = 0

    for line in lines:
        ts_match = TIMESTAMP_RE.match(line)
        if ts_match and first_ts is None:
            first_ts = parse_iso(ts_match.group("ts"))
        if " WARN " in line:
            warn_count += 1
        if " ERROR " in line:
            error_count += 1
        if bootstrap_ts is None and BOOTSTRAP_RE.search(line):
            ts_match = TIMESTAMP_RE.match(line)
            if ts_match:
                bootstrap_ts = parse_iso(ts_match.group("ts"))
        candidate_match = CANDIDATE_RE.search(line)
        if candidate_match:
            candidate_counts.append(int(candidate_match.group("count")))
            detect_ms.append(int(candidate_match.group("detect")))
        profit_match = PROFIT_RE.search(line)
        if profit_match:
            profit_counts.append(int(profit_match.group("count")))
            refresh_ms.append(int(profit_match.group("refresh")))
            simulated_rows += int(profit_match.group("simulated"))
            submitted_rows += int(profit_match.group("submitted"))
        rpc_match = RPC_RE.search(line)
        if rpc_match:
            rpc_rows += 1
            rpc_total_requests += int(rpc_match.group("requests"))
            rpc_total_cu += int(rpc_match.group("cu"))

    bootstrap_secs = None
    if first_ts and bootstrap_ts:
        bootstrap_secs = round((bootstrap_ts - first_ts).total_seconds(), 3)

    snapshots = len(candidate_counts)
    snapshots_with_candidates = sum(1 for count in candidate_counts if count > 0)
    avg_candidate_count = round(sum(candidate_counts) / snapshots, 3) if snapshots else 0.0
    avg_detect = round(sum(detect_ms) / len(detect_ms), 3) if detect_ms else 0.0
    avg_refresh = round(sum(refresh_ms) / len(refresh_ms), 3) if refresh_ms else 0.0

    return RunSummary(
        provider=provider,
        log_path=str(log_path),
        duration_secs=duration_secs,
        bootstrap_secs=bootstrap_secs,
        snapshots=snapshots,
        snapshots_with_candidates=snapshots_with_candidates,
        max_candidate_count=max(candidate_counts, default=0),
        avg_candidate_count=avg_candidate_count,
        avg_detect_ms=avg_detect,
        profitability_rows=len(profit_counts),
        max_profitability_candidate_count=max(profit_counts, default=0),
        simulated_rows=simulated_rows,
        submitted_rows=submitted_rows,
        avg_refresh_total_ms=avg_refresh,
        rpc_summary_rows=rpc_rows,
        rpc_total_requests=rpc_total_requests,
        rpc_total_cu=rpc_total_cu,
        warn_count=warn_count,
        error_count=error_count,
        exited_cleanly=(exit_code == 0),
        exit_code=exit_code,
    )


def provider_run_env(
    base_env: dict[str, str],
    spec: ProviderSpec,
    prometheus_port: int,
    with_replay: bool,
) -> dict[str, str]:
    env = dict(base_env)
    env["BASE_PUBLIC_RPC_URL"] = spec.public_rpc_url
    env["BASE_FALLBACK_RPC_URL"] = spec.public_rpc_url
    env["BASE_PRECONF_RPC_URL"] = spec.preconf_rpc_url
    env["BASE_PROTECTED_RPC_URL"] = spec.protected_rpc_url
    env["BASE_SIMULATE_METHOD"] = spec.simulate_method
    env["PROMETHEUS_BIND"] = f"127.0.0.1:{prometheus_port}"
    if not with_replay:
        # Provider comparison should measure live-search latency/cost, not the
        # cost of replaying an old local cache anchor through historical blocks.
        env["BOOTSTRAP_REPLAY_CACHED_POOL_STATES"] = "false"
        env["EVENT_BACKFILL_BLOCKS"] = "0"
        # Keep cached pool states admissible for canaries. Otherwise an old
        # local cache forces a full pool refresh before any live provider data
        # can be measured.
        env["STALENESS_TIMEOUT_MS"] = "31536000000"
    # QuickNode free/discover endpoints currently cap eth_getLogs ranges very
    # tightly. Use the same conservative chunking for both providers so the
    # comparison reaches the live loop without provider-specific failures.
    env["DISCOVERY_LOG_CHUNK_BLOCKS"] = "5"
    env["DISCOVERY_LOG_MIN_CHUNK_BLOCKS"] = "1"
    env["EVENT_LOG_CHUNK_BLOCKS"] = "5"
    env["EVENT_LOG_MIN_CHUNK_BLOCKS"] = "1"
    if spec.ws_url:
        env["BASE_WSS_URL"] = spec.ws_url
    else:
        env.pop("BASE_WSS_URL", None)
    return env


def terminate_process(proc: subprocess.Popen[str]) -> int | None:
    if proc.poll() is not None:
        return proc.returncode
    try:
        proc.send_signal(signal.SIGINT)
        return proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        proc.terminate()
        try:
            return proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            return proc.wait(timeout=5)


def run_provider(
    spec: ProviderSpec,
    base_env: dict[str, str],
    duration_secs: int,
    tag: str,
    prometheus_port: int,
    with_replay: bool,
) -> RunSummary:
    STATE_DIR.mkdir(parents=True, exist_ok=True)
    log_path = STATE_DIR / f"base-{spec.name}-compare-{tag}.log"
    env = provider_run_env(base_env, spec, prometheus_port, with_replay)
    cmd = ["cargo", "run", "--release", "--", "--chain", "base", "--simulate-only"]
    with log_path.open("w") as log_file:
        proc = subprocess.Popen(
            cmd,
            cwd=ROOT,
            env=env,
            stdout=log_file,
            stderr=subprocess.STDOUT,
            text=True,
        )
        time.sleep(duration_secs)
        exit_code = terminate_process(proc)
    return summarize_log(log_path, spec.name, duration_secs, exit_code)


def print_markdown_table(rows: Iterable[RunSummary]) -> None:
    print("| provider | bootstrap_s | snapshots | snapshots_with_candidates | max_candidate_count | avg_candidate_count | avg_detect_ms | rpc_requests | rpc_cu | warns | errors |")
    print("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |")
    for row in rows:
        bootstrap = "-" if row.bootstrap_secs is None else f"{row.bootstrap_secs:.3f}"
        print(
            f"| {row.provider} | {bootstrap} | {row.snapshots} | {row.snapshots_with_candidates} | "
            f"{row.max_candidate_count} | {row.avg_candidate_count:.3f} | {row.avg_detect_ms:.3f} | "
            f"{row.rpc_total_requests} | {row.rpc_total_cu} | {row.warn_count} | {row.error_count} |"
        )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--duration-secs", type=int, default=120)
    parser.add_argument("--providers", nargs="+", default=["alchemy", "quicknode"])
    parser.add_argument("--tag", default=time.strftime("%Y%m%d-%H%M%S"))
    parser.add_argument(
        "--with-replay",
        action="store_true",
        help="include cached-pool replay/backfill during provider runs",
    )
    args = parser.parse_args()

    env = merged_env()
    specs = {spec.name: spec for spec in build_provider_specs(env)}
    requested = []
    for name in args.providers:
        spec = specs.get(name)
        if spec is None:
            print(
                f"missing provider env for {name}. "
                f"Fill the provider-specific BASE_{name.upper()}_* RPC variables in .env.",
                file=sys.stderr,
            )
            return 2
        requested.append(spec)

    summaries: list[RunSummary] = []
    for index, spec in enumerate(requested):
        summaries.append(
            run_provider(
                spec=spec,
                base_env=env,
                duration_secs=args.duration_secs,
                tag=args.tag,
                prometheus_port=9898 + index,
                with_replay=args.with_replay,
            )
        )

    summary_path = STATE_DIR / f"base-provider-compare-{args.tag}.json"
    summary_path.write_text(json.dumps([asdict(summary) for summary in summaries], indent=2))

    print()
    print_markdown_table(summaries)
    print()
    print(f"json_summary={summary_path}")
    for summary in summaries:
        print(f"{summary.provider}_log={summary.log_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
