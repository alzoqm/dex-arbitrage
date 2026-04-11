# DEX N-Hop Cyclic Arbitrage Bot — 상세 설계서 v2

## 1. 프로젝트 개요

| 항목 | 내용 |
|---|---|
| 목적 | Base / Polygon 체인 내 DEX N-hop 순환 차익매매 자동화 |
| 언어 | Rust (tokio async runtime) |
| 노드 접근 | Base: Flashblocks-aware RPC + Standard RPC fallback / Polygon: Standard RPC(read) + Private Mempool(write) + Standard RPC fallback |
| 매매 전략 | USDT/USDC → … → USDT/USDC 순환 경로 후보 탐지 → exact quote / route simulation → 실행 |
| 자본 전략 | 자기 자본 + Aave V3 Flash Loan 병행 |
| 라우팅 | Spot-graph 기반 후보 경로 탐지 + hop별 Split Routing + 총 수량 최적화 |
| MEV 방어 | 체인별 Submitter Abstraction (Base: Flashblocks-aware read path + protected/private 가능 채널 + public fallback / Polygon: official Private Mempool default) |
| DEX 범위 | 지원 풀 타입을 명시적으로 제한한 Uniswap V2/V3, QuickSwap/Sushi/BaseSwap, Aerodrome(volatile + CL만), Curve plain/metapool(plain ERC20만), Balancer Weighted(static fee만) |
| 크로스체인 | 미적용 (체인별 독립 실행) |

> **v2 지원 범위 외**
> - Aerodrome V1 stable pool
> - Balancer Composable Stable / Linear / Boosted / Managed / dynamic-fee pool
> - Curve pool 중 rebasing / ERC4626 / rate-oraclized token 포함 풀
> - fee-on-transfer / rebasing / non-standard callback token
> - 체인 간 브릿지 기반 경로

---

## 2. 고수준 아키텍처

```
┌─────────────────────────────────────────────────────────────┐
│                        Entry Point                          │
│  CLI / Config Loader → Chain Selector (Base | Polygon)      │
└──────────────┬──────────────────────────────────┬───────────┘
               │                                  │
       ┌───────▼────────┐                ┌────────▼───────────┐
       │  Base Pipeline  │                │ Polygon Pipeline   │
       └───────┬────────┘                └────────┬───────────┘
               │  (동일 구조, 체인 채널/설정만 상이) │
               ▼                                  ▼
┌──────────────────────────────────────────────────────────────┐
│                    Per-Chain Pipeline                         │
│                                                              │
│  ┌──────────────┐   ┌──────────────┐   ┌─────────────────┐  │
│  │ ① Discovery  │──▶│ ② Graph Mgr  │──▶│ ③ Arb Detector  │  │
│  │  & Sync +    │   │ Versioned     │   │ Candidate Cycle │  │
│  │  Canonicality│   │ Spot Graph    │   │ Search          │  │
│  └──────────────┘   └──────────────┘   └───────┬─────────┘  │
│                                                │             │
│                                        ┌───────▼─────────┐  │
│                                        │ ④ Exact Quoter   │  │
│                                        │  & Split Router  │  │
│                                        └───────┬─────────┘  │
│                                                │             │
│                                        ┌───────▼─────────┐  │
│                                        │ ⑤ Execution Eng  │  │
│                                        │ Route Simulation │  │
│                                        │ Flash Loan +     │  │
│                                        │ Submitter Select │  │
│                                        └───────┬─────────┘  │
│                                                │             │
│                                        ┌───────▼─────────┐  │
│                                        │ ⑥ Risk & Logging │  │
│                                        └─────────────────┘  │
└──────────────────────────────────────────────────────────────┘
```

각 체인은 **독립된 tokio 태스크 그룹**으로 실행되며, 공유 상태 없이 체인별로 파이프라인을 완전 격리한다.  
단, 각 체인 내부에서는 **immutable snapshot + versioned state**를 사용하여 `Discovery/Updater`, `Detector`, `Validator`, `Submitter`가 동일한 상태 버전을 참조하도록 설계한다.

---

## 3. 모듈 상세 설계

### 3.1 모듈 ① — Discovery & Sync (초기 셋업 + 실시간 스트림)

#### 3.1.1 초기 부트스트랩 (Cold Start)

**목표**: 해당 체인의 지원 대상 DEX 풀(유동성 쌍)만 수집하고, 상태 버전이 있는 그래프 초기 상태를 구축한다.

**대상 DEX 프로토콜 (지원 범위 한정)**

| 체인 | AMM 유형 | DEX |
|---|---|---|
| Base | V2-like Volatile | Uniswap V2, BaseSwap, SushiSwap, Aerodrome V1 (volatile only) |
| Base | Concentrated Liquidity | Uniswap V3, Aerodrome V3 (Slipstream) |
| Base | StableSwap (plain ERC20 only) | Curve plain pools / metapools |
| Base | Weighted Pool (static fee only) | Balancer Weighted |
| Polygon | V2-like Volatile | Uniswap V2, QuickSwap V2, SushiSwap |
| Polygon | Concentrated Liquidity | Uniswap V3, QuickSwap V3 |
| Polygon | StableSwap (plain ERC20 only) | Curve plain pools / metapools |
| Polygon | Weighted Pool (static fee only) | Balancer Weighted |

**풀/자산 Admission Policy**

그래프는 “모든 ERC-20, 모든 풀”을 무차별적으로 수용하지 않는다. Discovery 단계에서 아래 규칙을 통과한 대상만 등록한다.

| 검사 항목 | 기준 |
|---|---|
| Factory / Registry 검증 | 공식 factory / registry / vault / pool factory에서 생성된 풀만 허용 |
| Pool Type allowlist | 위 표의 지원 풀 타입만 허용 |
| Token behavior | fee-on-transfer, rebasing, ERC4626, rate-oraclized, callback 특이 토큰은 기본 제외 |
| Liquidity Floor | USD 환산 최소 유동성 기준 미달 시 제외 |
| Pool Health | pause / freeze / dynamic-fee / owner-managed 특징이 있으면 기본 제외 또는 quarantine |
| Codehash / ABI | 기대한 ABI 및 bytecode 패턴과 일치해야 함 |
| Recent Revert Rate | warm-up 기간 동안 quote/sim 실패율이 높으면 quarantine |

**프로토콜별 풀 수집 방법**

| AMM 유형 | 수집 방법 |
|---|---|
| V2-like Volatile | Factory `allPairsLength()` → `allPairs(i)` 반복 |
| V3 / Slipstream | Factory `PoolCreated` 이벤트 로그 스캔 |
| Curve | Registry / Factory 조회 (`pool_count()` / `pool_list(i)` 등) + pool metadata 검사 |
| Balancer Weighted | Vault `PoolRegistered` 이벤트 + pool contract metadata 검사 |

**수집 절차**

1. **Factory / Registry / Vault 순회**: 지원 범위에 속하는 전체 풀 주소 목록 수집.
2. **풀 메타데이터 캐싱**: 풀마다 AMM 유형, 토큰 목록, fee tier, 불변 파라미터, 지원 여부를 1회 조회 후 로컬 캐시에 저장.
   - V2-like: `token0`, `token1`, `fee`, stable/volatile flag(해당 시), factory
   - V3 / Slipstream: `token0`, `token1`, `fee`, `tickSpacing`, factory
   - Curve: `coins(i)`, `A`, `fee`, 코인 개수, pool type, exotic token 여부
   - Balancer Weighted: `getPoolTokens(poolId)`, `getNormalizedWeights()`, `getSwapFeePercentage()`, owner / paused / dynamic fee 여부
3. **Admission Filtering**: 지원 풀 타입, 토큰 특성, codehash/factory 검증, 유동성 기준을 통과한 풀만 채택.
4. **초기 가격/유동성 스냅샷**: 풀마다 현재 상태를 정확한 정수 산술로 조회 → spot graph 간선과 quote cache 초기화.
5. **Aave Flash Loan 파라미터 수집**: `FLASHLOAN_PREMIUM_TOTAL()` 및 reserve availability를 조회하여 flash loan 비용 모델 초기화.
6. **가스비 기준값 수집**:
   - Base: L2 fee + L1 security fee 분리 모델 초기화
   - Polygon: EIP-1559 `baseFee` + `priorityFee` 모델 초기화
7. **초기 Graph Snapshot 생성**: `snapshot_id = 0` 으로 시작하는 immutable snapshot 생성.

**Alchemy / Provider 호출 최적화**

- `eth_call` Batch JSON-RPC: 부트스트랩 시 batch size를 체인/프로토콜별로 조절.
- Multicall3 활용: reserve / slot0 / token metadata / vault balance 조회 병렬화.
- **CU Budget Manager**:
  - `Critical`: 실행 직전 검증 / 상태 정합성 유지
  - `High`: 핫패스 그래프 업데이트
  - `Low`: 주기적 스냅샷 / cold-start 보조 데이터
- CU 한도 부족 시 `Low` 요청부터 지연시키고, `Critical` 요청은 항상 우선 처리한다.

**결과물**

- `Arc<GraphSnapshot>` — 현재 시점의 immutable graph snapshot
- `HashMap<PoolId, PoolState>` — 정밀 상태값(U256/u128 기반)
- `TokenBehaviorRegistry` — 토큰별 behavior flag
- `PoolAdmissionRegistry` — allow / quarantine / excluded 상태
- `QuoteCache` — exact quote 보조 캐시
- `ReorgRingBuffer` — 최근 N개 블록 스냅샷 보관

---

#### 3.1.2 실시간 업데이트 (Hot Path)

**체인별 이벤트 채널 아키텍처**

```
[Base]
Flashblocks-aware RPC/WSS
  ├─ pendingLogs / newFlashblockTransactions
  ├─ eth_call / eth_simulateV1 against pending state
  └─ Standard logs fallback
            │
            ▼
      Event Decoder
            │
            ▼
  Canonicality / Snapshot Manager
            │
            ▼
   Transactional Pool State Updater
            │
            ▼
      New Graph Snapshot Publish

[Polygon]
Standard WSS logs
  ├─ eth_subscribe("logs")
  └─ HTTP polling fallback (eth_getLogs)
            │
            ▼
      Event Decoder
            │
            ▼
  Canonicality / Snapshot Manager
            │
            ▼
   Transactional Pool State Updater
            │
            ▼
      New Graph Snapshot Publish
```

**프로토콜별 이벤트 디코딩**

| AMM 유형 | 필수 이벤트 | 비고 |
|---|---|---|
| V2-like Volatile | `Sync` | reserve 변동 반영 |
| V3 / Slipstream | `Initialize`, `Mint`, `Burn`, `Swap` | spot price + active liquidity + initialized tick 변화 반영 |
| Curve plain / metapool | `TokenExchange`, `TokenExchangeUnderlying`, `AddLiquidity`, `RemoveLiquidity`, `RemoveLiquidityOne`, `RemoveLiquidityImbalance`, `RampA`, `StopRampA` | 일부 이벤트는 직접 상태 복원이 불가하므로 pool re-read 필요 |
| Balancer Weighted | `Swap`, `PoolBalanceChanged` | join/exit는 `PoolBalanceChanged`로 반영. pause / fee 상태는 주기적 refresh 또는 health check |

**Canonicality / Reorg 처리**

- 최신 이벤트는 우선 `pending / preconfirmed` 상태로 반영할 수 있다.
- 최근 `N=32` 블록의 `GraphSnapshot` 과 `PoolDeltaLog` 를 ring buffer에 저장한다.
- `newHeads` / block hash를 통해 parent hash 불일치가 감지되면:
  1. 공통 조상 블록까지 rollback
  2. 해당 지점 이후 이벤트를 재적용
  3. 새 `snapshot_id` 발행
- Detector와 Validator는 항상 **동일한 `snapshot_id`** 를 참조한다.

**업데이트 원칙**

1. **Transactional Update**: 한 풀의 상태 갱신은 all-or-nothing으로 반영한다.
2. **Partial Failure 금지**: Curve/Balancer의 `eth_call` 재조회가 실패하면 기존 상태를 유지하고 해당 풀을 `stale/quarantine` 으로 마킹한다.
3. **Changed Edges Only**: 변경된 풀의 간선만 새 스냅샷에서 갱신한다.
4. **Pool Health Tracking**: stale / paused / unsupported / revert-heavy 풀은 detector 후보군에서 제외한다.

**업데이트 출력**

- `changed_edges: SmallVec<[EdgeIndex; 8]>`
- `snapshot_id: u64`
- `block_ref: {number, hash, finalized_level}`
- `pool_health_delta`

---

### 3.2 모듈 ② — Graph Manager & Update (그래프 테이블)

#### 3.2.1 그래프 모델링

**핵심 개념**: 토큰을 정점(Vertex), 풀(거래쌍)을 간선(Edge)으로 하는 **가중 방향 멀티그래프**.  
단, 여기서의 그래프는 **실행 수익을 직접 표현하는 그래프가 아니라, 후보 경로 prescreen 용 spot graph** 이다.

```
정점(V): Admission Policy를 통과한 ERC-20 토큰
간선(E): 지원 대상 풀 하나가 생성하는 방향 간선
         - V2-like: A → B, B → A
         - V3/Slipstream: A → B, B → A
         - Curve plain/metapool: 지원 코인 쌍에 대해 양방향
         - Balancer Weighted: 지원 토큰 쌍에 대해 양방향
         동일 토큰 쌍에 여러 풀이 존재 → 병렬 간선(multi-edge)
```

**간선 가중치 정의 (Spot Graph, No Slippage in Detection Layer)**

후보 차익 경로 탐지에 사용하는 가중치는 다음과 같이 정의한다:

```
w_spot(A → B) = -log(spot_rate_net(A → B))

spot_rate_net = infinitesimal_quote(A → B) × (1 - fee)
```

- `infinitesimal_quote`: 극소 수량 기준 spot quote
- `fee`: 풀 수수료
- **중요**: `slippage_estimate`는 간선 가중치에 넣지 않는다.
  - 슬리피지는 입력 수량 함수이므로, 탐지 그래프에 미리 고정값으로 집어넣으면 실행 가능 수익과 괴리가 생긴다.
  - 대신 exact quote / split optimizer / execution validator 단계에서만 반영한다.

**의미**

- `negative cycle in spot graph` = **후보 기회**
- `exact route quote + gas + flash fee > threshold` = **실행 기회**

따라서 탐지와 실행은 2단계로 분리한다.

---

#### 3.2.2 데이터 구조 — 인접 리스트 + 보조 인덱스

토큰 수를 `N`, 간선 수를 `E`라 하면 그래프는 여전히 희소 구조이므로 인접 리스트를 유지한다.  
단, 동시성 병목을 피하기 위해 mutable graph를 직접 공유하지 않고 **immutable snapshot** 을 교체하는 방식으로 운영한다.

```rust
// 개념적 구조 (코드가 아닌 구조 설명)
struct GraphStore {
    current: Arc<GraphSnapshot>,          // 최신 스냅샷
    ring: VecDeque<Arc<GraphSnapshot>>,   // 최근 N개 버전 보관
}

struct GraphSnapshot {
    snapshot_id: u64,
    block_number: u64,
    block_hash: B256,
    finality: FinalityLevel,              // Pending | Sealed | Finalized

    // 정점
    tokens: Vec<TokenInfo>,
    token_to_index: HashMap<Address, usize>,

    // 희소 인접 리스트
    adjacency: Vec<Vec<Edge>>,
    reverse_adj: Vec<Vec<EdgeRef>>,

    // 보조 인덱스
    pool_to_edges: HashMap<PoolId, SmallVec<[EdgeIndex; 6]>>,
    pair_to_pools: HashMap<(usize, usize), SmallVec<[EdgeIndex; 4]>>,

    // 정밀 풀 상태
    pools: HashMap<PoolId, PoolState>,
}

struct Edge {
    to: usize,
    pool_id: PoolId,
    amm_type: AmmType,

    // 탐지용 가중치
    weight_log_q32: i64,          // scaled -log(spot_rate_net)
    spot_rate_q128: U256,         // 정수 fixed-point

    // 수수료 / 유동성
    fee_ppm: u32,
    liquidity_depth: LiquidityInfo,

    // 건강도 / 버전
    pool_health: PoolHealth,      // stale, paused, quarantined, confidence
    snapshot_id: u64,
}

struct PoolHealth {
    stale: bool,
    paused: bool,
    quarantined: bool,
    confidence_bps: u16,
    last_successful_refresh_block: u64,
}

enum AmmType {
    UniswapV2Like { reserve0: u128, reserve1: u128 },
    UniswapV3Like { sqrt_price_x96: U256, liquidity: u128, tick: i32 },
    CurvePlain { balances: Vec<u128>, amp: u128, n_coins: u8 },
    BalancerWeighted { balances: Vec<u128>, weights: Vec<u128>, swap_fee_ppm: u32 },
}
```

**보조 인덱스**

| 인덱스 | 용도 |
|---|---|
| `pool_to_edges` | 이벤트 수신 → O(1)로 해당 풀의 모든 간선 접근 |
| `reverse_adj` | 역방향 탐색 / `dist_to_stable` 계산 |
| `stable_token_indices` | USDT / USDC 시작·종료점 빠른 접근 |
| `pair_to_pools` | exact quote / split routing 시 병렬 풀 조회 |

**정밀도 정책**

- **금액 / 유동성 / fee / quote**: `U256` / `u128` / fixed-point
- **탐지용 weight**: 정수 스케일(`i64`) 또는 내부 `f64` 변환을 허용하되, 최종 수익 판정에는 사용하지 않음
- **최종 실행 수익**: 반드시 정수 산술로 재검증

---

#### 3.2.3 간선 업데이트 흐름

```
Swap / Sync / Liquidity / PoolBalanceChanged 이벤트 수신
    │
    ▼
현재 snapshot 기준 pool_id 해석
    │
    ▼
필요 시 pool re-read (Curve / Balancer / V3 liquidity event)
    │
    ├─ 실패 → 기존 상태 유지 + stale/quarantine 마킹 + 새 snapshot 발행
    │
    └─ 성공
         │
         ▼
정밀 PoolState 갱신
         │
         ▼
spot_rate_q128 / weight_log_q32 / liquidity_depth 갱신
         │
         ▼
changed_edges 생성
         │
         ▼
새 immutable snapshot 발행
         │
         ▼
Detector에 (snapshot_id, changed_edges) 전달
```

**시간 복잡도**

- 대부분 O(1) ~ O(k), `k = 해당 풀에서 생성된 간선 수`
- Snapshot 발행은 변경된 구조만 재사용하여 copy-on-write 스타일로 최적화한다.

---

### 3.3 모듈 ③ — Arbitrage Detector (핵심 알고리즘)

#### 3.3.1 알고리즘 선택 근거

| 알고리즘 | 전체 재탐색 비용 | 증분 업데이트 | 채택 여부 |
|---|---|---|---|
| Bellman-Ford (전체) | O(V × E) | 불가 — 매번 전체 | ❌ |
| Floyd-Warshall | O(V³) | O(V²) per update | ❌ |
| **증분 Bellman-Ford / SPFA (spot graph)** | 국소적 | 가능 | ✅ |
| DFS/BFS 후보 순환 탐색 | hop 제한 시 유용 | 가능 | ✅ 보조 |

#### 3.3.2 채택 알고리즘: Edge-Scoped Incremental Negative Cycle Detection

**핵심 아이디어**:  
가중치가 변경된 간선 주변만 탐색해서 **후보 음의 순환** 을 찾고, 실제 수익성은 후단 exact quote 단계에서 판단한다.

**전체 흐름**

```
간선 (u → v)의 spot weight 변경
    │
    ├─ [1단계] snapshot 고정
    │   detector는 입력으로 받은 snapshot_id만 사용
    │
    ├─ [2단계] 시작점 필터링
    │   stable token(USDT/USDC)에서 u까지 도달 가능한가?
    │   v에서 stable token으로 복귀 가능한가?
    │   pool_health가 양호한가?
    │
    ├─ [3단계] 후보 경로 탐색
    │   MAX_HOPS 범위 내에서 음의 순환 후보를 탐색
    │   단, 이 단계는 spot graph 기준 prescreen
    │
    ├─ [4단계] exact quote 단계로 승격
    │   후보 경로만 exact quote / split / size search 수행
    │
    └─ [5단계] 수익성 판정
        net_profit = final_output - input - gas_buffered_cost - flash_fee
        if net_profit > threshold:
            → Execution Engine으로 전달
```

**중요 변경점**

기존처럼 `dist + w + dist < 0`만으로 바로 실행하지 않는다.  
이제는 **음의 순환 = exact quote를 돌려볼 가치가 있는 후보** 로 해석한다.

---

#### 3.3.3 사전 계산 테이블 (Distance Cache)

빠른 증분 판단을 위해 두 개의 거리 테이블을 유지한다.

```
dist_from_stable[v] : stable token → v 까지의 최소 spot weight
dist_to_stable[v]   : v → stable token 까지의 최소 spot weight
prev_from_stable[v] : 경로 복원용
prev_to_stable[v]   : 경로 복원용
```

- **초기화**: 부트스트랩 시 stable token들을 source set으로 전체 1회 계산
- **증분 갱신**: `changed_edges` 중심으로 SPFA/queue propagation
- **주의**: 이 캐시는 candidate prescreen 전용이다. exact profit을 의미하지 않는다.

---

#### 3.3.4 Hop 제한 & 가지치기

| 파라미터 | 설명 | 권장값 |
|---|---|---|
| `MAX_HOPS` | 순환 경로 최대 스왑 수 | 4~5 |
| `SCREENING_MARGIN` | spot graph 후보 승격 최소 기준 | 0~10 bps |
| `MIN_NET_PROFIT` | exact quote 후 최소 순이익 | 체인/가스 조건별 동적 |
| `LIQUIDITY_FLOOR` | 최소 유동성 | $10,000 상당 |
| `POOL_HEALTH_MIN` | confidence 하한 | 9,000 bps 등 |
| `STABLE_DEPEG_CUTOFF` | stable path 차단 기준 | 예: $0.995 |

가지치기 규칙:
1. `pool_health`가 불량(stale / quarantined / paused)이면 제외.
2. Hop 수가 `MAX_HOPS`를 넘으면 종단.
3. 이미 방문한 토큰 재방문 금지 (단순 순환만 허용).
4. exact quote로 넘어가기 전 low-TVL / exotic-token path는 제외.
5. stable token이 depeg guard를 위반하면 stablecoin 루프 전체 중단.

---

### 3.4 모듈 ④ — Split Router & Optimizer (신규)

#### 3.4.1 Split Routing 개요

동일 토큰 쌍(A → B)에 여러 풀이 존재하면, 단일 풀에 전체 물량을 넣는 것보다 **여러 풀에 분배**하는 편이 더 나은 총 출력량을 만들 수 있다.  
단, 이득이 추가 gas와 실패 리스크를 상회할 때만 Split을 적용한다.

```
후보 경로:
USDC → WETH → DAI → USDT

각 hop마다:
1) 병렬 풀 조회
2) 각 풀에 exact quote
3) capacity / health / gas 반영
4) split이 유리하면 분배, 아니면 single pool
```

---

#### 3.4.2 최적 분배 알고리즘

**문제 정의**

```
maximize: Σ output_i(x_i) - extra_gas_cost(active_pools)

subject to:
  Σ x_i = total_input
  0 ≤ x_i ≤ capacity_i
  pool_i.health == healthy
```

**입력별 quote 방식**

| AMM 유형 | quote 방식 |
|---|---|
| V2-like | 해석식(closed-form) |
| V3 / Slipstream | tick-walking exact simulator |
| Curve plain / metapool | fixed-point Newton solver + early exit |
| Balancer Weighted | weighted math exact quote |

**알고리즘: Exact Quote Guided Water-Filling**

닫힌 형태 미분식이 항상 깔끔하지 않으므로, v2는 다음 하이브리드 방식을 사용한다.

1. 병렬 풀 후보를 수집한다.
2. 각 풀에 대해 **작은 quote slice** 단위의 한계 출력량을 exact quote로 평가한다.
3. 가장 나은 marginal output을 제공하는 풀에 먼저 할당한다.
4. 할당 후 해당 풀의 marginal quote를 다시 평가한다.
5. 총 입력이 모두 분배될 때까지 반복한다.
6. 분배 결과에 대해 route-level gas / minOut / profitability를 재확인한다.

이 방식은:
- V2 closed-form
- V3 piecewise liquidity
- Curve / Balancer exact solver

를 하나의 일관된 인터페이스로 처리할 수 있다.

**Capacity Constraint**

각 풀에는 사전 계산된 `capacity_i` 를 둔다.

- V2-like: reserve 대비 안전 비율
- V3-like: crossing tick 수 / initialized liquidity / max price impact
- Curve / Balancer: quote 안정성, slippage ceiling, supported token 조건

capacity를 넘는 split은 후보에서 제외한다.

---

#### 3.4.3 경로 통합 — N-hop Split Routing

경로 전체는 **외부 루프(총 입력 수량)** 와 **내부 루프(hop별 split)** 로 구성한다.

```
외부 루프:
  입력 수량 후보를 coarse-to-fine 탐색

내부 루프:
  Hop 1 exact split
  → Hop 1 output
  → Hop 2 exact split
  → Hop 2 output
  → ...
  → Hop N exact split
  → 최종 output
```

**수량 탐색 전략**

순이익 함수가 항상 완전한 단봉형이라고 가정하지 않으므로, 단순 binary search만 사용하지 않는다.

1. geometric ladder로 입력 수량 후보 구간을 빠르게 훑는다.
2. 상위 구간에서 local refinement(ternary / bracketed search)를 수행한다.
3. 최고 순이익 수량을 선택한다.

---

#### 3.4.4 Split Routing 의사결정 기준

```
IF 병렬 풀 수 == 1:
    → 단일 풀
ELIF 추가 split 절감 가치 ≤ 추가 gas + 리스크 버퍼:
    → 단일 풀
ELIF 일부 풀의 capacity / health / paused 조건이 불량:
    → 건강한 풀만 대상으로 재최적화
ELSE:
    → Split Routing 적용
```

추가 판단 기준:
- 분배량이 dust 수준이면 해당 split 비활성화
- route-level minProfit를 만족하지 않으면 split 계획 전체 폐기
- partial fill / try-catch 복구는 사용하지 않고 **사전 시뮬레이션과 원자적 실패**를 기본으로 한다

---

### 3.5 모듈 ⑤ — Execution Engine (Flash Loan + MEV Protection)

#### 3.5.1 실행 아키텍처 총괄

```
candidate_path + snapshot_id 수신
    │
    ├─ [1] exact off-chain quote
    │   - 동일 snapshot_id 기준
    │   - hop별 split + 총 입력 수량 최적화
    │
    ├─ [2] 자본 전략 결정
    │   - self-funded vs flashLoanSimple
    │
    ├─ [3] route-level 최종 검증
    │   - Base: Flashblocks pending state 기준 eth_simulateV1 / eth_call
    │   - Polygon: latest 기준 eth_call
    │   - pool별 재조회 루프는 사용하지 않음
    │
    ├─ [4] tx 구성
    │   - per-split minOut
    │   - route-level minProfit
    │   - snapshot/version metadata
    │
    └─ [5] submitter 선택 후 전송
        - chain-specific private/protected/public channel
```

**Quote / Simulation Ladder**

1. **Spot Graph Prescreen** — detector가 후보 경로 발견
2. **Exact Off-chain Quote** — same snapshot 기준 정밀 계산
3. **Target-state Simulation** — 체인 aware RPC 시뮬레이션 1회
4. **Signed Tx Build** — calldata / fee / nonce 확정
5. **Submit** — 채널 선택 후 전송

---

#### 3.5.2 Flash Loan 통합 (Aave V3)

**v2 선택**

- 시작 토큰이 stablecoin(USDC 또는 USDT) 하나이므로 기본 경로는 **`flashLoanSimple()`** 를 사용한다.
- `flashLoanSimple()` 은 gas 효율이 높고, fee waiver가 없다.
- 플래시론 수수료는 하드코딩하지 않고 **on-chain `FLASHLOAN_PREMIUM_TOTAL()` 조회값** 을 사용한다.

**흐름**

```
1. startup / periodic refresh 시 FLASHLOAN_PREMIUM_TOTAL 조회
2. opportunity마다:
   - self-funded profitability 계산
   - flashLoanSimple profitability 계산
3. 더 나은 모드 선택
4. executeOperation() 내부에서 경로 실행
5. amount + premium 상환
6. minProfit 미달 시 전체 revert
```

**수익성 계산**

```
net_profit_self  = final_output - input_amount - gas_buffered_cost
net_profit_flash = final_output - input_amount - gas_buffered_cost - flash_fee

flash_fee = borrowed_amount × current_flashloan_premium
```

**Self-funded vs Flash Loan 자동 선택**

```
IF self_balance >= optimal_amount
   AND net_profit_self >= net_profit_flash:
       → Self-funded
ELIF net_profit_flash >= MIN_NET_PROFIT:
       → Flash Loan
ELSE:
       → 실행하지 않음
```

---

#### 3.5.3 커스텀 Executor 컨트랙트 설계

```
┌─────────────────────────────────────────────┐
│         ArbitrageExecutor Contract           │
│                                              │
│  Inherits / Interfaces                       │
│   - IFlashLoanSimpleReceiver                 │
│   - IUniswapV3SwapCallback                   │
│   - ReentrancyGuard                          │
│   - Ownable / Safe-owned auth                │
│                                              │
│  [진입점 1] executeSelfFunded(params)         │
│    → transferFrom / prefunded balance 사용     │
│    → _executeSwapPath                         │
│                                              │
│  [진입점 2] executeFlashLoan(params)          │
│    → Aave flashLoanSimple                     │
│    → executeOperation 콜백에서 경로 실행       │
│                                              │
│  [콜백] uniswapV3SwapCallback(...)            │
│    → canonical pool / factory 검증            │
│    → owed token 정산                          │
│                                              │
│  [내부] _executeSwapPath(hops[])              │
│    → 각 hop별 split 실행                      │
│    → per-split minAmountOut 체크               │
│    → route-level finalBalance / minProfit 체크 │
│                                              │
│  [운영]                                       │
│    → authorizedOperators                      │
│    → pause / emergency rescue / dust sweep    │
└─────────────────────────────────────────────┘
```

**보안 / 운영 방침**

- `owner` 는 개인 EOA가 아니라 **Safe 멀티시그** 주소를 권장
- `authorizedOperators` 는 봇 핫월렛으로 한정
- `nonReentrant` 를 외부 진입점에 적용
- `pause()` / `unpause()` / `rescueTokens()` 제공
- dust token은 실행 종료 후 sweep

**Approval 전략**

- 무제한 approve를 광범위하게 남기지 않는다.
- 필요한 대상(router / vault / pool)에 대해 `exact approve` 또는 `forceApprove` 패턴을 사용한다.
- 신뢰 가능한 정적 대상만 allowlist에 포함한다.

---

#### 3.5.4 MEV 방어 전략

**원칙**: 특정 체인에 특정 relay를 하드코딩하지 않고, 체인별 read/write 경로와 채널 건강도를 분리한다.

**Submitter Matrix**

| 체인 | Read Path | Write Path | 기본 정책 |
|---|---|---|---|
| Base | Flashblocks-aware RPC (`pending`, `pendingLogs`, `eth_simulateV1`) + Standard RPC fallback | `ProtectedSubmitter` (provider/private/protected 채널이 있으면 우선) + `PublicEip1559Submitter` fallback | 최신 포함률 / 지연 / 실패율을 보고 채널 선택 |
| Polygon | Standard RPC / WSS read path | `PolygonPrivateMempoolSubmitter` 기본 + emergency public fallback | official Private Mempool 우선, public fallback은 정책적으로 제한 |

**채널 선택기**

채널마다 다음 지표를 유지한다.

- 최근 포함률
- 평균 포함 지연
- simulation mismatch rate
- reverted submission rate
- cost efficiency

가장 건강한 채널을 우선 선택하고, TTL 내 미포함 시 **fresh snapshot 기준 재시뮬레이션 후** 같은 또는 다른 채널로 재시도한다.

**가스 / 제출 정책**

- Base:
  - Flashblocks pending state에서 사전 시뮬레이션
  - L1 fee + L2 fee 분리 모델
  - protected/private channel이 없으면 aggressive EIP-1559 public submit
- Polygon:
  - official Private Mempool 기본 사용
  - read path는 기존 RPC provider 유지
  - private path 불능 시에만 public fallback 고려

---

#### 3.5.5 매매 수량 최적화 (Split Routing 통합)

```
maximize:
  final_output(input) - input - gas_buffered_cost - flash_fee

subject to:
  - per-hop max slippage / max price impact
  - pool capacity / pool health
  - route-level minProfit
  - admitted pool / token only
```

**외부 루프**

- geometric ladder + local refinement로 최적 input을 탐색한다.

**내부 루프**

- 각 hop에서 split optimizer가 최적 분배량을 계산한다.
- hop의 총 출력은 다음 hop의 총 입력이 된다.

**최종 검증**

- 선택된 `optimal_input` 과 split plan은 **same snapshot** 기준 exact quote를 다시 한 번 수행한다.
- 이후 chain-aware simulation이 통과해야만 submitter로 넘어간다.

---

#### 3.5.6 Nonce 관리

동시에 여러 기회가 발견될 수 있으므로 nonce는 채널 독립적으로 추상화해 관리한다.

- `NonceManager` 는 로컬에서 nonce reservation을 원자적으로 수행
- 채널별 상태:
  - `Reserved`
  - `Submitted`
  - `Included`
  - `Dropped / Expired`
- public fallback 채널에서는 replacement 정책(`same nonce`, higher fee)을 지원
- private/protected 채널은 `status poll + timeout reconciliation` 로 정리
- `eth_getTransactionCount(..., "pending")` 와 local reservation ledger를 함께 사용한다

---

### 3.6 모듈 ⑥ — Risk Management & Logging

#### 3.6.1 리스크 파라미터

| 파라미터 | 설명 |
|---|---|
| `MAX_POSITION_SIZE` | 단일 트랜잭션 최대 투입 금액 |
| `MAX_FLASH_LOAN_SIZE` | Flash Loan 최대 차입 한도 |
| `DAILY_LOSS_LIMIT` | 일일 최대 손실 한도 |
| `MAX_CONCURRENT_TX` | 동시 미확정 트랜잭션 수 |
| `GAS_PRICE_CEILING` | 이 이상이면 실행 금지 |
| `GAS_RISK_BUFFER_PCT` | 가스비 안전 마진 |
| `CIRCUIT_BREAKER` | 연속 손실 시 일시 정지 |
| `STALENESS_TIMEOUT` | 마지막 신뢰성 있는 업데이트 이후 허용 시간 |
| `POOL_HEALTH_MIN` | detector 진입 최소 confidence |
| `STABLE_DEPEG_CUTOFF` | stablecoin 경로 차단 기준 |
| `REORG_BUFFER_DEPTH` | rollback 가능한 최근 snapshot 수 |
| `CHANNEL_TTL_MS` | submitter 재시도 전 기회 유효 시간 |

#### 3.6.2 로깅 & 모니터링

```
[구조화 로깅 — tracing crate]
│
├── arb_candidate      : spot graph에서 후보 경로 발견
├── arb_exact_quote    : exact quote / split / size search 결과
├── arb_rejected       : 실행하지 않은 이유 (profit, gas, stale, health, depeg, channel)
├── arb_execution      : 실행 결과 (tx_hash, 실제 수익, gas)
├── route_simulation   : chain-aware final simulation 결과
├── split_routing      : hop별 분배 내역과 절감량
├── flash_loan         : 차입액, premium, 선택 모드
├── submitter_stats    : 채널별 포함률, 지연, 실패율
├── graph_update       : snapshot_id, changed_edges, pool health
├── snapshot_reorg     : rollback / reapply 내역
├── ws_health          : WSS / Flashblocks / polling 상태
└── gas_tracker        : Base(L1/L2) / Polygon(EIP-1559) 가스 추이
```

**메트릭 대시보드**

- opportunities/min
- exact-quote 승격률
- execution success rate
- rejection reason breakdown
- split routing 적용률 / 슬리피지 절감량 / 추가 gas
- route simulation latency
- event → submit end-to-end latency
- snapshot rollback count
- stale / quarantined pool 수
- Polygon Private Mempool submit success
- Base Flashblocks availability / reorg rate
- 누적 PnL (flash fee 차감 후)
- Alchemy CU 소비율

---

## 4. AMM별 수학 모델 상세

### 4.1 Uniswap V2 (Constant Product)

> 적용 범위: Uniswap V2, QuickSwap V2, SushiSwap, BaseSwap, Aerodrome V1 volatile 등 V2-like volatile pool

```
x × y = k

output = (reserve_out × input × (1 - fee)) / (reserve_in + input × (1 - fee))
spot_rate = reserve_out / reserve_in
```

- input / output / reserve 계산은 정수 산술로 수행
- split optimizer의 marginal quote는 closed-form 또는 작은 quote slice로 계산
- direct pair call 시에는 **input token을 pair에 먼저 전송한 뒤 `swap()`** 을 호출한다

### 4.2 Uniswap V3 (Concentrated Liquidity)

> 적용 범위: Uniswap V3, QuickSwap V3, Aerodrome V3(Slipstream)

```
현재 tick 기준 유효 유동성(L) 산출
sqrtPriceX96와 tickBitmap을 기반으로
initialized tick을 실제로 순회하며 quote 계산

1. 현재 tick / sqrtPrice 로 시작
2. 다음 initialized tick까지의 최대 교환량 계산
3. 입력량이 남아 있으면 tick crossing
4. crossing 시 liquidityNet 반영
5. 모든 입력이 소진될 때까지 반복
```

- V3/Slipstream는 **선형 근사로 전체 hop quote를 계산하지 않는다**
- `tickBitmap + liquidityNet + slot0` 를 캐싱하고, quote는 tick-walking simulator로 계산
- exact validation에서는 필요 시 protocol quoter와 동등한 결과가 나오는지 테스트로 검증
- direct pool call 시 `uniswapV3SwapCallback` 을 구현하고, callback caller가 canonical factory에서 생성된 pool인지 검증한다

### 4.3 Curve (StableSwap)

> 적용 범위: plain ERC20 기반 plain pool / metapool만 지원  
> 제외: rebasing, ERC4626, rate-oraclized token 포함 풀

```
StableSwap invariant 기반 exact quote
1. balances / A / fee 읽기
2. D 계산 (Newton's method)
3. dx 입력 반영
4. dy 계산
5. fee 차감
```

- Newton solver는 상대 오차 기준 조기 종료를 사용한다
- solver 실패 또는 unsupported edge case에서는 해당 풀을 quarantine 하거나 validation 단계에서 protocol quote fallback을 사용한다
- `RampA`, `StopRampA` 이벤트는 파라미터 변화로 간주해 상태를 갱신한다

### 4.4 Balancer V2 (Weighted Math)

> 적용 범위: static-fee Weighted pool만 지원  
> 제외: Composable Stable, Linear, Boosted, Managed, dynamic-fee pool

```
Weighted Product invariant:
Π(B_i ^ w_i) = k

output = B_out × [1 - (B_in / (B_in + input × (1-fee)))^(w_in/w_out)]
spot_rate = (B_in / w_in) / (B_out / w_out)
```

- validation 단계에서 Balancer 경로는 `querySwap` / `queryBatchSwap` 과 동일한 결과를 맞추도록 테스트한다
- `PoolBalanceChanged` 는 join/exit에 의한 잔액 변화 반영
- paused / dynamic-fee pool은 admission 단계에서 기본 제외

---

## 5. 데이터 플로우 전체 시퀀스

```
[Base Flashblocks / Polygon WSS]
    │
    │  logs / pendingLogs / liquidity events
    ▼
┌──────────────────┐
│  Event Decoder   │  이벤트 → (pool_id, amm_type, state_delta)
└────────┬─────────┘
         │
         ▼
┌──────────────────┐
│ Canonicality &   │  reorg check, rollback buffer 관리
│ Snapshot Manager │
└────────┬─────────┘
         │
         ▼
┌──────────────────┐
│ Graph Updater    │  changed pools만 exact 상태 갱신
│ (Transactional)  │  partial failure 시 stale/quarantine
└────────┬─────────┘
         │
         │  (snapshot_id, changed_edges)
         ▼
┌──────────────────┐
│ Candidate        │  spot graph 기반 음의 순환 후보 탐지
│ Detector         │
└────────┬─────────┘
         │
         │  candidate_path
         ▼
┌──────────────────┐
│ Exact Quoter &   │  same snapshot 기준
│ Split Optimizer  │  hop split + total size search
└────────┬─────────┘
         │
         │  exact_plan
         ▼
┌──────────────────┐
│ Route Simulation │  Base: eth_simulateV1/eth_call
│ & Capital Select │  Polygon: eth_call
└────────┬─────────┘
         │
         │  executable_plan
         ▼
┌──────────────────┐
│ Tx Builder       │  callback-aware calldata, minOut, minProfit
└────────┬─────────┘
         │
         ▼
┌──────────────────┐
│ Submitter Select │  Base protected/public, Polygon private/public
└────────┬─────────┘
         │
         ▼
┌──────────────────┐
│ Post Execution   │  receipt, PnL, nonce, rejection stats
│ Monitor          │
└──────────────────┘
```

---

## 6. Rust Crate 의존성 계획

| 영역 | Crate | 용도 |
|---|---|---|
| 비동기 런타임 | `tokio` | async runtime, 채널, 타이머 |
| 이더리움 | `alloy` | ABI 인코딩, provider, signer, RPC 타입 |
| WebSocket / HTTP | `tokio-tungstenite`, `reqwest` | WSS 연결, JSON-RPC, private mempool / protected submit |
| 스냅샷 스왑 | `arc-swap` | immutable snapshot 교체 |
| 컬렉션 최적화 | `smallvec` | changed_edges / pair pool 인덱스 최적화 |
| 직렬화 | `serde`, `serde_json`, `bincode` | 설정, 캐시, 이벤트 파싱 |
| 수학 | `fixed` | 고정 소수점 계산 |
| 큰 정수 | `ruint` | U256 연산 |
| 로깅 | `tracing`, `tracing-subscriber` | 구조화 로깅 |
| 메트릭 | `metrics`, `metrics-exporter-prometheus` | Prometheus |
| 설정 | `config`, `dotenvy` | 설정 파일 + 환경변수 |
| 테스트 | `tokio::test`, `proptest` | 비동기 테스트, fuzz/property test |

---

## 7. 프로젝트 디렉토리 구조

```
dex-arbitrage/
├── Cargo.toml
├── config/
│   ├── base.toml                # Base read/write 채널, Flashblocks, 가스 정책
│   └── polygon.toml             # Polygon Private Mempool, 가스 정책
├── src/
│   ├── main.rs
│   ├── config.rs
│   ├── types.rs
│   │
│   ├── discovery/
│   │   ├── mod.rs
│   │   ├── factory_scanner.rs
│   │   ├── curve_registry.rs
│   │   ├── balancer_registry.rs
│   │   ├── admission.rs         # token / pool allowlist / quarantine
│   │   ├── pool_fetcher.rs
│   │   └── event_stream.rs      # Flashblocks / WSS / polling
│   │
│   ├── amm/
│   │   ├── mod.rs
│   │   ├── uniswap_v2.rs
│   │   ├── uniswap_v3.rs        # tick-walking quote
│   │   ├── curve.rs
│   │   └── balancer.rs
│   │
│   ├── graph/
│   │   ├── mod.rs
│   │   ├── model.rs             # GraphSnapshot / PoolState
│   │   ├── weight.rs            # spot graph weight
│   │   ├── updater.rs           # transactional update
│   │   ├── snapshot.rs          # immutable snapshot / rollback ring
│   │   └── distance_cache.rs
│   │
│   ├── detector/
│   │   ├── mod.rs
│   │   ├── incremental_bf.rs
│   │   ├── path_finder.rs
│   │   └── pruning.rs
│   │
│   ├── router/
│   │   ├── mod.rs
│   │   ├── exact_quoter.rs      # exact quote abstraction
│   │   ├── split_optimizer.rs   # water-filling / capacity constraints
│   │   ├── quantity_search.rs   # coarse-to-fine input search
│   │   └── split_decision.rs
│   │
│   ├── execution/
│   │   ├── mod.rs
│   │   ├── validator.rs         # route-level exact simulation
│   │   ├── flash_loan.rs
│   │   ├── capital_selector.rs
│   │   ├── tx_builder.rs
│   │   ├── submitter.rs         # submitter abstraction
│   │   ├── base_submitter.rs    # protected/public + Flashblocks-aware simulation
│   │   ├── polygon_submitter.rs # official Private Mempool write path
│   │   └── nonce_manager.rs
│   │
│   ├── risk/
│   │   ├── mod.rs
│   │   ├── limits.rs
│   │   ├── gas_tracker.rs
│   │   └── depeg_guard.rs
│   │
│   └── monitoring/
│       ├── mod.rs
│       ├── logger.rs
│       └── metrics.rs
│
├── contracts/
│   ├── ArbitrageExecutor.sol
│   ├── adapters/
│   │   ├── UniswapV2Adapter.sol
│   │   ├── UniswapV3Adapter.sol
│   │   ├── CurveAdapter.sol
│   │   └── BalancerAdapter.sol
│   ├── interfaces/
│   │   ├── IFlashLoanSimpleReceiver.sol
│   │   ├── IAavePool.sol
│   │   └── IUniswapV3SwapCallback.sol
│   └── test/
│       └── ExecutorTest.sol
│
└── tests/
    ├── amm_math_tests.rs
    ├── split_router_tests.rs
    ├── graph_tests.rs
    ├── detector_tests.rs
    ├── regression_arb_scenarios.rs
    ├── reorg_tests.rs
    ├── performance_event_stress.rs
    └── integration_tests.rs
```

---

## 8. 성능 목표 및 벤치마크 기준

| 지표 | 목표 |
|---|---|
| 이벤트 수신 → 상태 갱신 | < 2ms |
| 상태 갱신 → candidate detection | < 5ms (MAX_HOPS=5 기준) |
| exact quote + split + size search | < 15ms (3-hop, 3 pools/hop 이하 기준) |
| route-level final simulation + tx build | < 40ms (Base Flashblocks-aware path) / < 80ms (standard fallback) |
| **총 End-to-End 레이턴시** | **< 100ms (Base hot path) / < 150ms (fallback path)** |
| 제출된 트랜잭션 simulation mismatch | < 1% |
| 메모리 사용량 | < 1.2GB per chain (snapshot ring buffer 포함) |
| stale/quarantine pool 비율 | 모니터링 후 자동 축소 |
| Alchemy CU 소비 | 일일 예산 내 유지 |

---

## 9. 스마트 컨트랙트 아키텍처 상세

### 9.1 ArbitrageExecutor 컨트랙트 구조

```
ArbitrageExecutor
├── IFlashLoanSimpleReceiver
├── IUniswapV3SwapCallback
├── ReentrancyGuard
├── Ownable (권장 owner = Safe multisig)
│
├── State Variables
│   ├── owner: address
│   ├── authorizedOperators: mapping(address => bool)
│   ├── aavePool: IPool
│   ├── allowedFactories / allowedPools
│   └── paused: bool
│
├── External Functions
│   ├── executeSelfFunded(ExecutionParams calldata params)
│   ├── executeFlashLoan(FlashLoanParams calldata params)
│   ├── executeOperation(...)               [Aave callback]
│   ├── uniswapV3SwapCallback(...)         [V3 callback]
│   ├── pause() / unpause()
│   └── sweepDust(token)
│
├── Internal Functions
│   ├── _executeSwapPath(Hop[] memory hops)
│   ├── _executeSingleSwap(...)
│   ├── _approveIfNeeded(...)
│   ├── _verifyCanonicalV3Pool(...)
│   └── _checkFinalProfit(...)
│
└── Structs
    ├── ExecutionParams { hops, minProfit, deadline, snapshotId }
    ├── FlashLoanParams { asset, amount, hops, minProfit, snapshotId }
    ├── Hop { splits: Split[] }
    └── Split { adapterType, pool, tokenIn, tokenOut, amountIn, minAmountOut }
```

### 9.2 DEX 어댑터별 호출 인터페이스

| 어댑터 | 호출 대상 | 핵심 함수 / 주의점 |
|---|---|---|
| UniswapV2Adapter | Pair Contract 직접 | 입력 토큰을 pair에 먼저 전송한 뒤 `swap(...)` 호출 |
| UniswapV3Adapter | Pool Contract 직접 | `swap(...)` 호출 후 `uniswapV3SwapCallback(...)` 에서 정산, canonical factory 검증 필수 |
| CurveAdapter | Pool Contract | 지원되는 plain/metapool ABI에 한해 `exchange(...)` / `exchange_underlying(...)` |
| BalancerAdapter | Vault Contract | `swap(...)` / 필요 시 `batchSwap(...)`, validation은 `querySwap` / `queryBatchSwap`와 동일 결과 기준 |

### 9.3 가스비 예상 (hop당)

| 구성요소 | 예상 Gas |
|---|---|
| Aave V3 flashLoanSimple 오버헤드 | ~80,000 |
| Uniswap V2-like swap | ~70,000 |
| Uniswap V3 / Slipstream swap + callback | ~120,000~150,000 |
| Curve exchange | ~150,000~300,000 |
| Balancer Weighted swap | ~130,000 |
| Split 추가분 (pool당) | ~20,000~40,000 |

```
예시: 3-hop Flash Loan + Split (총 5개 풀 터치)
≈ 80,000 + 130,000 + 140,000 + 180,000 + 30,000×2
≈ 590,000 gas (경로/프로토콜에 따라 변동)

Base total fee = L2 execution fee + L1 security fee
Polygon total fee = EIP-1559 base fee + priority fee
```

---

## 10. 장애 복구 & 안정성

| 장애 시나리오 | 대응 |
|---|---|
| Flashblocks / WSS 연결 끊김 | 체인별 fallback 채널로 자동 전환 |
| Reorg / preconfirmation 미포함 | snapshot ring buffer rollback + 재적용 |
| Alchemy rate limit (429) | CU budget manager + priority queue + backpressure |
| Graph data stale | pool quarantine + detector 제외 + 주기적 refresh |
| Curve/Balancer re-read 실패 | partial update 금지, 기존 상태 유지, stale 마킹 |
| Route simulation 실패 | 즉시 폐기 + rejection reason 기록 |
| Flash Loan 실패 | self-funded 모드 재평가 또는 기회 폐기 |
| Private Mempool / protected submit 실패 | fresh snapshot 재시뮬레이션 후 fallback 채널 고려 |
| Nonce 충돌 | local reservation + pending nonce reconciliation + replacement 정책 |
| 프로세스 비정상 종료 | pool metadata / admission cache로 warm start, state만 재수집 |

---

## 11. 향후 확장 고려사항 (v3+)

1. **지원 풀 타입 확장**: Aerodrome stable, Balancer Composable Stable/Boosted, Curve exotic token pool 지원.
2. **체인별 채널 고도화**: 더 많은 protected/private submitter 추가, 채널 선택기 자동 튜닝.
3. **크로스체인 확장**: 브릿지 비용/시간을 간선으로 모델링하여 체인 간 경로 탐색.
4. **고급 전략**: opportunity priority queue, dynamic MAX_HOPS, JIT liquidity, ML 기반 slippage/gas 예측.
