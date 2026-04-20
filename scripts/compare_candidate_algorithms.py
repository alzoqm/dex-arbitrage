#!/usr/bin/env python3
"""Compare candidate ranking algorithms on deterministic synthetic arbitrage data.

The synthetic data intentionally includes many high-spread, low-capacity long-tail
routes and fewer lower-spread, high-capacity executable routes. This mirrors the
failure mode where spot-price Top-K burns validation budget on dust liquidity.
"""

from __future__ import annotations

from dataclasses import dataclass
import argparse
import math
import random
import statistics
import time


@dataclass(frozen=True)
class Route:
    route_id: int
    spot_edge_bps: float
    capacity_usd: float
    quality_bps: float
    hops: int
    fee_bps: float
    gas_usd: float
    true_opportunity: bool


@dataclass
class EvalResult:
    route_id: int
    net_usd: float
    best_input_usd: float


def generate_routes(n: int, true_opps: int, seed: int) -> list[Route]:
    rng = random.Random(seed)
    routes: list[Route] = []

    for i in range(n - true_opps):
        # Long-tail decoys: attractive spot edge, low executable capacity,
        # lower token quality, and more hops.
        routes.append(
            Route(
                route_id=i,
                spot_edge_bps=rng.uniform(90, 2_500),
                capacity_usd=10 ** rng.uniform(0.2, 2.4),
                quality_bps=rng.uniform(600, 3_500),
                hops=rng.randint(3, 5),
                fee_bps=rng.uniform(18, 80),
                gas_usd=rng.uniform(0.06, 0.45),
                true_opportunity=False,
            )
        )

    for j in range(true_opps):
        route_id = n - true_opps + j
        # Executable opportunities: lower apparent edge but materially larger
        # capacity and higher quality.
        routes.append(
            Route(
                route_id=route_id,
                spot_edge_bps=rng.uniform(18, 130),
                capacity_usd=10 ** rng.uniform(3.0, 5.6),
                quality_bps=rng.uniform(7_500, 10_000),
                hops=rng.randint(2, 4),
                fee_bps=rng.uniform(8, 35),
                gas_usd=rng.uniform(0.04, 0.25),
                true_opportunity=True,
            )
        )

    rng.shuffle(routes)
    return routes


def exact_eval(route: Route) -> EvalResult:
    """Small deterministic slippage/fee/gas model.

    This represents the expensive stage: per-hop output math, slippage, fees,
    flash fee, and gas. In production this maps to router exact quote work plus
    validator simulation for the final top candidates.
    """

    # Candidate input sizes as fractions of route capacity.
    fractions = (0.005, 0.01, 0.02, 0.04, 0.08, 0.12, 0.18, 0.25)
    best = EvalResult(route.route_id, net_usd=-1e18, best_input_usd=0.0)

    for frac in fractions:
        input_usd = max(1.0, route.capacity_usd * frac)
        gross_edge = input_usd * route.spot_edge_bps / 10_000.0
        # Convex price impact grows sharply near capacity.
        impact = input_usd * (frac**1.55) * (1.0 + 0.18 * route.hops)
        fees = input_usd * route.fee_bps / 10_000.0
        flash_fee = input_usd * 5.0 / 10_000.0
        quality_haircut = input_usd * (10_000.0 - route.quality_bps) / 10_000.0 * 0.025
        net = gross_edge - impact - fees - flash_fee - quality_haircut - route.gas_usd
        if net > best.net_usd:
            best = EvalResult(route.route_id, net_usd=net, best_input_usd=input_usd)

    return best


def spot_score(route: Route) -> float:
    return route.spot_edge_bps


def profit_proxy_score(route: Route) -> float:
    edge = max(route.spot_edge_bps - route.fee_bps - 5.0, 0.0) / 10_000.0
    capacity_slice = route.capacity_usd * 0.04
    hop_penalty = 1.0 / (1.0 + max(route.hops - 2, 0) * 0.35)
    quality = route.quality_bps / 10_000.0
    gas_penalty = route.gas_usd
    return capacity_slice * edge * quality * hop_penalty - gas_penalty


def run_spot_topk(routes: list[Route], top_k: int) -> tuple[list[EvalResult], int]:
    selected = sorted(routes, key=spot_score, reverse=True)[:top_k]
    return [exact_eval(route) for route in selected], len(selected)


def run_exact_all(routes: list[Route], top_k: int) -> tuple[list[EvalResult], int]:
    evaluated = [exact_eval(route) for route in routes]
    evaluated.sort(key=lambda result: result.net_usd, reverse=True)
    return evaluated[:top_k], len(routes)


def run_two_stage(routes: list[Route], proxy_k: int, top_k: int) -> tuple[list[EvalResult], int]:
    selected = sorted(routes, key=profit_proxy_score, reverse=True)[:proxy_k]
    evaluated = [exact_eval(route) for route in selected]
    evaluated.sort(key=lambda result: result.net_usd, reverse=True)
    return evaluated[:top_k], len(selected)


def summarize(
    name: str,
    routes: list[Route],
    results: list[EvalResult],
    exact_evals: int,
    elapsed_ms: float,
    min_profit_usd: float,
) -> dict[str, float | str]:
    true_ids = {route.route_id for route in routes if route.true_opportunity}
    found_profitable = [result for result in results if result.net_usd >= min_profit_usd]
    found_true = [result for result in found_profitable if result.route_id in true_ids]
    recall = len({result.route_id for result in found_true}) / max(len(true_ids), 1)
    best = max((result.net_usd for result in results), default=-1e18)
    return {
        "algorithm": name,
        "recall": recall,
        "profitable": len(found_profitable),
        "true_profitable": len(found_true),
        "best_net_usd": best,
        "exact_evals": exact_evals,
        "rpc_cost_proxy": exact_evals,
        "elapsed_ms": elapsed_ms,
    }


def timed(fn, *args):
    started = time.perf_counter()
    results, exact_evals = fn(*args)
    elapsed_ms = (time.perf_counter() - started) * 1000.0
    return results, exact_evals, elapsed_ms


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--routes", type=int, default=10_000)
    parser.add_argument("--true-opps", type=int, default=80)
    parser.add_argument("--top-k", type=int, default=16)
    parser.add_argument("--proxy-k", type=int, default=384)
    parser.add_argument("--trials", type=int, default=8)
    parser.add_argument("--min-profit-usd", type=float, default=0.05)
    args = parser.parse_args()

    rows: dict[str, list[dict[str, float | str]]] = {
        "spot_topk": [],
        "exact_all": [],
        "two_stage_proxy": [],
    }

    for seed in range(args.trials):
        routes = generate_routes(args.routes, args.true_opps, seed)
        for name, fn_args in [
            ("spot_topk", (run_spot_topk, routes, args.top_k)),
            ("exact_all", (run_exact_all, routes, args.top_k)),
            ("two_stage_proxy", (run_two_stage, routes, args.proxy_k, args.top_k)),
        ]:
            fn = fn_args[0]
            results, exact_evals, elapsed_ms = timed(fn, *fn_args[1:])
            rows[name].append(
                summarize(
                    name,
                    routes,
                    results,
                    exact_evals,
                    elapsed_ms,
                    args.min_profit_usd,
                )
            )

    print(
        "algorithm,avg_recall,avg_profitable,avg_true_profitable,avg_best_net_usd,"
        "avg_exact_evals,avg_rpc_cost_proxy,avg_elapsed_ms"
    )
    for name, samples in rows.items():
        def avg(key: str) -> float:
            return statistics.fmean(float(sample[key]) for sample in samples)

        print(
            f"{name},"
            f"{avg('recall'):.4f},"
            f"{avg('profitable'):.2f},"
            f"{avg('true_profitable'):.2f},"
            f"{avg('best_net_usd'):.4f},"
            f"{avg('exact_evals'):.2f},"
            f"{avg('rpc_cost_proxy'):.2f},"
            f"{avg('elapsed_ms'):.3f}"
        )


if __name__ == "__main__":
    main()
