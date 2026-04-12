
# DEX N-Hop Cyclic Arbitrage Bot (Rust)

Base / Polygon 체인의 DEX N-hop cyclic arbitrage 설계를 Rust 프로젝트로 옮긴 구현체다.  
핵심 목적은 다음과 같다.

- 체인별 독립 파이프라인 실행
- immutable graph snapshot 기반 후보 탐지
- hop별 split routing + 총 투입 수량 최적화
- self-funded / Aave V3 flash loan 자동 선택
- Base / Polygon 제출 채널 분리
- `.env` 중심 운영 설정
- Solidity executor 컨트랙트 동봉

## 구성

- `src/`: Rust 실행 엔진
- `config/base.toml`, `config/polygon.toml`: 체인별 기본 정책/DEX 정의
- `.env.example`: 사용자가 채워야 하는 환경변수
- `contracts/`: executor + interfaces
- `tests/`: 러스트 단위 테스트
- `foundry.toml`: Solidity 계약 빌드용 Foundry 설정

## 핵심 구현 포인트

- **Discovery**
  - V2-like: factory `allPairsLength/allPairs`
  - V3-like: `PoolCreated` 로그 스캔
  - Curve: registry `pool_count/pool_list`
  - Balancer: vault `PoolRegistered` 로그 스캔
- **Graph**
  - pool refresh 결과를 받아 immutable snapshot 재구성
  - pair → pools, pool → edges 보조 인덱스 유지
- **Detector**
  - `MAX_HOPS` 이내 DFS prescreen
  - 변경 간선이 한 번 이상 포함된 경로만 승격
  - Aave flash-loan-enabled anchor 시작/복귀 reachability 캐시 기반 pruning
  - `[search]` / `SEARCH_*` 설정으로 pair별 대체 pool, beam width, 후보 수 조정
- **Router**
  - V2 local exact math
  - V3 quoter 기반 exact quote (quoter 없으면 보수적 fallback)
  - Curve `get_dy` exact quote
  - Balancer weighted fallback quote
  - water-filling split optimizer
- **Execution**
  - route calldata 생성
  - `eth_call`/preconf RPC 검증
  - self-funded vs flash loan 선택
  - Base protected/public, Polygon private/public 제출 순서화
- **Risk**
  - max position / max flash loan / max concurrent / gas ceiling / depeg guard
  - daily loss / circuit breaker skeleton 포함

## 탐색 폭 설정

`config/base.toml`, `config/polygon.toml`의 `[search]` 섹션에서 multi-venue 탐색 폭을 조절한다. 같은 값은 환경변수로도 덮어쓸 수 있다.

| TOML key | Env override | 의미 |
| --- | --- | --- |
| `top_k_paths_per_side` | `SEARCH_TOP_K_PATHS_PER_SIDE` | changed edge 앞/뒤에서 유지할 경로 수 |
| `max_virtual_branches_per_node` | `SEARCH_MAX_VIRTUAL_BRANCHES_PER_NODE` | 노드별 확장 branch 수 |
| `path_beam_width` | `SEARCH_PATH_BEAM_WIDTH` | DFS beam frontier 크기 |
| `max_candidates_per_refresh` | `SEARCH_MAX_CANDIDATES_PER_REFRESH` | refresh당 detector 후보 상한 |
| `max_pair_edges_per_pair` | `SEARCH_MAX_PAIR_EDGES_PER_PAIR` | detector가 같은 token pair에서 유지할 pool 수 |
| `max_split_parallel_pools` | `MAX_SPLIT_PARALLEL_POOLS` | router가 hop별 split 대상으로 볼 pool 수 |

## 빠른 시작

1. `.env.example` 를 `.env` 로 복사
2. `.env` 에 RPC / private key / executor / DEX 주소 입력
3. Solidity executor를 배포하고 `*_EXECUTOR_ADDRESS` 를 `.env` 에 입력
4. 빌드/실행

```bash
./scripts/check_env.sh base
./scripts/check_env.sh polygon
cargo run --release -- --chain base
cargo run --release -- --chain polygon --simulate-only
```

또는 Makefile 사용:

```bash
make run-base
make run-polygon-sim
```

## 계약 배포

Foundry 사용 예시:

```bash
forge build
forge create contracts/ArbitrageExecutor.sol:ArbitrageExecutor   --rpc-url $BASE_PUBLIC_RPC_URL   --private-key $DEPLOYER_PRIVATE_KEY   --constructor-args $BASE_AAVE_POOL $SAFE_OWNER
```

배포 후:
- `setOperator(operator, true)`
- 필요 시 `setAllowedTargets([...], true)` 또는 `setStrictTargetAllowlist(false)` 유지
- self-funded 운용이면 executor에 stable 자금 입금
- strict allowlist를 켤 경우 Balancer는 pool + vault 둘 다 허용 목록에 등록

## 운영 모드

- `--simulate-only`: 발견/검증까지만 수행하고 제출은 하지 않음
- `--once`: 1회 bootstrap + detection 후 종료
- `.env` 의 `SIMULATION_ONLY=true` 와 CLI flag 모두 지원

## 주의

이 프로젝트는 실매매용 구조와 코드를 최대한 맞춰 두었지만, 현재 컨테이너에 Rust 툴체인이 없어 여기서 `cargo check` / 실체인 배포까지 검증하지는 못했다.  
실사용 전에는 반드시 다음 순서로 검증해야 한다.

1. `cargo fmt && cargo clippy && cargo test`
2. `forge build && forge test`
3. Base Sepolia / Polygon Amoy 또는 별도 fork 환경 검증
4. small size self-funded mode → protected/private mode → flash loan mode 순서로 단계적 전환

## 권장 .env 채움 순서

1. RPC / WSS / private submit endpoints
2. operator/deployer private key
3. executor / Aave / DEX factory / registry / vault 주소
4. 토큰 주소
5. risk / gas ceiling / position limit

## 파일 트리

```text
dex-arbitrage/
├── Cargo.toml
├── README.md
├── .env.example
├── config/
├── src/
├── contracts/
├── foundry.toml
└── tests/
```
