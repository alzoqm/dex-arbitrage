# Base Live Setup Progress

작성일: 2026-04-14

이 문서는 지금까지 Base-first 실매매 준비를 어떤 순서로 진행했는지, 현재 어떤 값이 세팅됐는지, 다음에 무엇을 해야 하는지 기록한다.

보안 원칙:

- 이 문서에는 private key, Alchemy API key, 전체 RPC URL을 적지 않는다.
- 실제 secret 값은 로컬 `.env`에만 있다.
- `.env`와 `.secrets/`는 Git 추적에서 제외한다.
- public address와 transaction hash만 문서에 기록한다.

## 1. 현재 지갑/주소 역할

### 1.1 Safe owner 지갑

```text
0xD9e1eb7CadD8cD227e5305f6A93038221Bd005Ef
```

역할:

- Safe UI에서 관리자 트랜잭션에 서명하는 MetaMask 지갑.
- executor의 owner가 직접 이 주소인 것은 아니다.
- `.env`에 private key를 넣지 않는다.

### 1.2 Base Safe 주소

```text
0xbEA8fA57302325c7462EA2D4d8022E82a652D5eD
```

역할:

- `ArbitrageExecutor`의 owner.
- operator 등록/해제, pause, rescue, ownership 변경 같은 관리자 권한을 가진다.

검증 결과:

```text
Safe code exists: yes
Safe threshold: 1
Safe owners: [0xD9e1eb7CadD8cD227e5305f6A93038221Bd005Ef]
Safe version: 1.4.1
```

`.env` 설정:

```env
SAFE_OWNER=0xbEA8fA57302325c7462EA2D4d8022E82a652D5eD
```

### 1.3 Deployer 지갑

```text
0xa99050636686256eb77756A5A13A9E3fc81b127e
```

역할:

- Base `ArbitrageExecutor` 배포에 사용.
- 배포 후에는 일반 운영 권한이 없다.
- private key는 `.env`에 있으며 이 문서에는 기록하지 않는다.

현재 확인된 잔액:

```text
배포 전: 0.006 ETH
배포 후: 0.005983736094814575 ETH
```

### 1.4 Operator 지갑

```text
0x28a91B69f43f54B0b237dCca35AfB0BE53b56A12
```

역할:

- bot이 실제 거래 트랜잭션을 서명하는 hot wallet.
- `executeFlashLoan` / `executeSelfFunded` 호출 권한을 가진다.
- 큰 자금을 보관하지 않고 Base gas용 ETH만 둔다.

검증 결과:

```text
operator registered on executor: true
```

## 2. Base executor 배포 결과

현재 사용해야 하는 최신 executor:

```text
0xDDe9FDB14AF542B064334297808ec976E7cF7dCC
```

2026-04-19 재배포 이유:

```text
Aerodrome Classic V2 adapter와 최신 실행 안전장치가 로컬 최신 컨트랙트에 추가됐다.
기존 executor(0x39cF...)는 이전 바이트코드라서 최신 실행 경로와 맞지 않았다.
따라서 새 ArbitrageExecutor를 배포하고 Safe에서 새 executor에 operator 권한을 다시 등록했다.
```

최신 `.env` 설정:

```env
BASE_EXECUTOR_ADDRESS=0xDDe9FDB14AF542B064334297808ec976E7cF7dCC
```

최신 온체인 검증 결과:

```text
executor code exists: yes
executor code output chars: 29367
aavePool(): 0xA238Dd80C259a72e81d7e4664a9801593F98d1c5
owner(): 0xbEA8fA57302325c7462EA2D4d8022E82a652D5eD
paused(): false
strictTargetAllowlist(): false
operators(0x28a91B69f43f54B0b237dCca35AfB0BE53b56A12): true
```

이전 executor 기록:

배포된 executor:

```text
0x39cFf9ff02aE6dE82553a611c30D943847F2De55
```

배포 트랜잭션:

```text
0x0ab4a52cc00aac49b9fba150544c3fa5271a8878a0039ad488556bdd5cb8fc85
```

배포 인자:

```text
aavePool_: 0xA238Dd80C259a72e81d7e4664a9801593F98d1c5
owner_:    0xbEA8fA57302325c7462EA2D4d8022E82a652D5eD
```

`.env` 설정:

```env
BASE_EXECUTOR_ADDRESS=0x39cFf9ff02aE6dE82553a611c30D943847F2De55
```

온체인 검증 결과:

```text
executor code exists: yes
executor code bytes: 13617
aavePool(): 0xA238Dd80C259a72e81d7e4664a9801593F98d1c5
owner(): 0xbEA8fA57302325c7462EA2D4d8022E82a652D5eD
strictTargetAllowlist(): false
operators(0x28a91B69f43f54B0b237dCca35AfB0BE53b56A12): true
```

## 3. 진행 순서 요약

### 3.1 Base-first 환경으로 전환

프로젝트 실행 기준을 Base로 맞췄다.

`.env`:

```env
CHAIN=base
```

Base RPC, WSS, Aave Pool, 주요 token, 주요 DEX 주소를 채웠다. RPC URL과 API key는 secret이므로 문서화하지 않는다.

### 3.2 Base DEX 범위 설정

현재 Base 설정의 venue 범위:

```toml
venues = ["uniswap_v2", "sushiswap_v2", "baseswap_v2", "aerodrome_v2", "uniswap_v3", "aerodrome_v3_legacy", "aerodrome_v3_caps", "aerodrome_v3", "curve", "balancer"]
symbols = []
```

의미:

- `symbols = []`라서 설정상 특정 심볼만으로 제한하지 않는다.
- 현재 코드가 지원하고 주소 검증이 끝난 Base venue 전체를 대상으로 한다.
- Aerodrome Classic V2와 Slipstream/V3 3개 factory는 켜져 있다.
- Aerodrome Classic V2 실행 경로를 포함한 최신 executor로 재배포했고 Safe operator 권한도 갱신했다.

### 3.3 Flash loan 기준 설정

Base Aave V3 Pool:

```text
0xA238Dd80C259a72e81d7e4664a9801593F98d1c5
```

확인한 flash loan premium:

```text
FLASHLOAN_PREMIUM_TOTAL = 5
해석: 5 bps = 0.05%
```

Base Aave에서 flash-loan-enabled로 확인한 reserve들은 discovery 단계에서 cycle anchor로 승격될 수 있게 했다. `USDC`, `USDT`는 stable이라는 이유만으로 anchor가 되지 않게 `is_cycle_anchor = false`, `allow_self_funded = false`로 맞췄다.

### 3.4 Safe 생성 및 검증

Safe 생성 트랜잭션:

```text
0x7b136efcd338524afd3d4ec4c547968a66436b381639bebbcd6a6c068d2463b9
```

처음에는 MetaMask owner 주소와 Safe 주소가 헷갈렸으나, receipt와 Safe 함수 호출로 아래를 구분했다.

```text
0xD9e1... = Safe owner MetaMask 지갑
0xbEA8... = 실제 Safe Account 주소
0x4e1D... = Safe Proxy Factory
0x4167... = Safe singleton/mastercopy
0x7b13... = Safe 생성 transaction hash
```

최종적으로 `.env`의 `SAFE_OWNER`는 실제 Safe Account 주소인 `0xbEA8...`로 설정했다.

### 3.5 Executor 배포

처음 `./scripts/deploy_base.sh` 실행 시 Foundry 설정상 dry-run으로 끝났다. 실제 배포에는 `--broadcast`가 필요했다.

실제 배포 명령:

```bash
source .env

forge create contracts/ArbitrageExecutor.sol:ArbitrageExecutor \
  --broadcast \
  --json \
  --rpc-url "$BASE_PUBLIC_RPC_URL" \
  --private-key "$DEPLOYER_PRIVATE_KEY" \
  --constructor-args "$BASE_AAVE_POOL" "$SAFE_OWNER"
```

배포 결과:

```text
deployedTo: 0x39cFf9ff02aE6dE82553a611c30D943847F2De55
transactionHash: 0x0ab4a52cc00aac49b9fba150544c3fa5271a8878a0039ad488556bdd5cb8fc85
```

그 뒤 `.env`의 `BASE_EXECUTOR_ADDRESS`에 배포 주소를 반영했다.

### 3.6 Operator 등록

Safe Transaction Builder에서 executor의 `setOperator(address,bool)`를 호출했다.

입력값:

```text
To:
0x39cFf9ff02aE6dE82553a611c30D943847F2De55

operator:
0x28a91B69f43f54B0b237dCca35AfB0BE53b56A12

allowed:
true
```

사용한 calldata:

```text
0x558a729700000000000000000000000028a91b69f43f54b0b237dcca35afb0be53b56a120000000000000000000000000000000000000000000000000000000000000001
```

검증 결과:

```text
operators(0x28a91B69f43f54B0b237dCca35AfB0BE53b56A12) = true
```

## 4. 현재 완료된 것

```text
[x] Base Safe 생성
[x] Safe owner 확인
[x] SAFE_OWNER를 .env에 반영
[x] Base ArbitrageExecutor 배포
[x] BASE_EXECUTOR_ADDRESS를 .env에 반영
[x] executor owner가 Safe인지 확인
[x] executor aavePool이 Base Aave Pool인지 확인
[x] operator 지갑 등록
[x] strictTargetAllowlist=false 확인
[x] base/live env check 통과
[x] 테스트용 config/base.test.toml 생성
```

## 5. 현재 아직 남은 것

### 5.1 테스트 설정 적용

테스트용 설정은 실제 실행 경로에 적용했다.

백업:

```text
config/base.live.backup.toml
```

현재 적용된 테스트 설정:

```text
config/base.toml = config/base.test.toml 내용으로 교체됨
```

`.env` override도 테스트 한도에 맞춰 낮췄다.

```text
BASE_MAX_POSITION_USD_E8 = $100
BASE_MAX_FLASH_LOAN_USD_E8 = $100
DAILY_LOSS_LIMIT_USD_E8 = $5
BASE_MAX_POSITION = 100 USDC raw 기준
BASE_MAX_FLASH_LOAN = 100 USDC raw 기준
DAILY_LOSS_LIMIT = -5 USDC raw 기준
```

최신 `.env` 백업:

```text
.secrets/env-backups/.env.base-test-applied-20260414-000941
```

### 5.2 simulate-only 실행

실제 거래 전 최종 dry run을 시작했다.

```bash
cargo run --release -- --chain base --once --simulate-only
```

결과:

```text
release build 성공
프로그램 시작 성공
bootstrapping discovery 진입
3분 이상 추가 출력 없이 초기 전체 discovery 진행
사용자 턴에서 무기한 대기하지 않도록 프로세스 중단
```

해석:

```text
이 단계는 거래 실패가 아니다.
첫 실행이라 discovery cache가 아직 없고, Base 전체 supported venue를 스캔하기 때문에 초기 bootstrap이 매우 무겁다.
특히 factory_all_pairs 방식 V2 venue는 allPairsLength 전체를 읽고 각 pool 상태까지 fetch한다.
Base Uniswap V2 factory는 pair 수가 매우 많아 첫 bootstrap이 오래 걸릴 수 있다.
```

현재 `state/`에는 아직 유효한 discovery/pool cache가 생성되지 않았다.

### 5.3 live 실행 전 확인

아래는 live 직전 다시 확인한다.

```text
operator 지갑 Base ETH 잔액
BASE_WETH_PRICE_E8 최신성
BASE_MAX_POSITION_USD_E8
BASE_MAX_FLASH_LOAN_USD_E8
DAILY_LOSS_LIMIT_USD_E8
Alchemy 사용량/요금제
로그 저장 위치
실패 시 즉시 중단 방법
```

## 6. 보안 보존 방식

현재 `.gitignore`에 아래 항목을 추가했다.

```gitignore
.secrets/
```

이미 있던 보안 관련 ignore:

```gitignore
.env
.env.local
```

따라서 아래 파일/폴더는 Git에 올라가지 않아야 한다.

```text
.env
.env.local
.secrets/
```

`.env`는 로컬에서만 보관한다. `.env`에는 다음 secret이 포함될 수 있다.

```text
RPC API key
OPERATOR_PRIVATE_KEY
DEPLOYER_PRIVATE_KEY
```

이 문서에는 secret 값을 기록하지 않았다.

## 7. 복구 시 필요한 공개값

만약 로컬 문맥을 잃어버렸을 때, secret 없이도 상태를 이해하는 데 필요한 공개값은 아래다.

```text
Chain: Base mainnet
Chain ID: 8453
Safe: 0xbEA8fA57302325c7462EA2D4d8022E82a652D5eD
Safe owner wallet: 0xD9e1eb7CadD8cD227e5305f6A93038221Bd005Ef
Executor current: 0xDDe9FDB14AF542B064334297808ec976E7cF7dCC
Executor previous: 0x39cFf9ff02aE6dE82553a611c30D943847F2De55
Previous executor deploy tx: 0x0ab4a52cc00aac49b9fba150544c3fa5271a8878a0039ad488556bdd5cb8fc85
Operator: 0x28a91B69f43f54B0b237dCca35AfB0BE53b56A12
Deployer: 0xa99050636686256eb77756A5A13A9E3fc81b127e
Aave Pool: 0xA238Dd80C259a72e81d7e4664a9801593F98d1c5
```

## 8. 빠른 검증 명령

```bash
source .env

cast chain-id --rpc-url "$BASE_PUBLIC_RPC_URL"

cast call "$BASE_EXECUTOR_ADDRESS" \
  "owner()(address)" \
  --rpc-url "$BASE_PUBLIC_RPC_URL"

cast call "$BASE_EXECUTOR_ADDRESS" \
  "aavePool()(address)" \
  --rpc-url "$BASE_PUBLIC_RPC_URL"

OPERATOR_ADDRESS=$(cast wallet address --private-key "$OPERATOR_PRIVATE_KEY")
cast call "$BASE_EXECUTOR_ADDRESS" \
  "operators(address)(bool)" "$OPERATOR_ADDRESS" \
  --rpc-url "$BASE_PUBLIC_RPC_URL"

./scripts/check_env.sh base live
```

정상 기대값:

```text
chain-id: 8453
owner: SAFE_OWNER
aavePool: BASE_AAVE_POOL
operator allowed: true
base/live env check: pass
```

## 9. 2026-04-14 중단 지점

Base simulate-only discovery를 시작했지만, 초기 discovery 캐시가 생성되기 전에 사용자가 중지했다.

확인된 상태:

```text
남아 있는 dex-arbitrage 프로세스: 없음
state 디렉터리 크기: 약 4KB
생성된 discovery cache: 없음
마지막 로그: bootstrapping discovery chain=base simulate_only=true
실매매 트랜잭션 전송: 없음
```

따라서 다음 실행은 기존 캐시를 이어받는 것이 아니라 Base 초기 discovery를 다시 시작한다.

내일 재개할 때 기본 순서:

```bash
source .env
./scripts/check_env.sh base live
RUST_LOG=info target/release/dex-arbitrage --chain base --once --simulate-only 2>&1 | tee state/discovery-run.log
```

실행 중 모니터링:

```bash
pgrep -af '[d]ex-arbitrage --chain base --once --simulate-only'
du -sh state
tail -n 30 state/discovery-run.log
```

## 10. 2026-04-15 simulate-only 재개 기록

Base 전체 지원 venue 기준 simulate-only bootstrap을 다시 진행했다.

첫 장시간 실행에서 확인된 최종 진행:

```text
discovery cache 생성: state/base_discovery_cache.json, 약 1.1GB
V2 pool state fetch 완료:
  scanned: 2,994,030
  admitted candidates: 2,922,794
  skipped: 71,236
V3 pool state fetch 완료:
  scanned: 1,859,400
  admitted candidates: 918,823
  skipped: 940,577
실매매 트랜잭션 전송: 없음
```

첫 실행은 V3 완료 직후 `eth_call` 단발 실패로 종료됐다.

```text
Error: bootstrap discovery failed
Caused by:
    rpc request failed for eth_call
```

해석:

```text
거래 실행 실패가 아니다.
V2/V3 대량 상태 조회는 완료됐지만, pool_state_cache 저장 전에 기타 pool 조회 또는 그 직후 RPC 1건이 실패했다.
당시 state/에는 base_discovery_cache.json만 있고 base_pool_state_cache는 없었다.
따라서 pool state fetch 결과는 재사용할 수 없었다.
```

재발 방지 코드 수정:

```text
V2 fetch 완료 후 pool_state_cache checkpoint 저장
V3 fetch 완료 후 pool_state_cache checkpoint 저장
non-v2/v3 fetch 중 500개마다 checkpoint 저장
Curve/Balancer 등 기타 pool fetch는 eth_call 실패 시 재시도
재시도 후에도 실패한 기타 pool은 전체 bootstrap을 중단하지 않고 skip
```

추가된 환경값:

```env
POOL_FETCH_MULTICALL_CHUNK_SIZE=300
POOL_FETCH_MAX_RETRIES=5
POOL_FETCH_RETRY_BASE_DELAY_MS=250
```

수정 검증:

```text
cargo fmt --check: 통과
cargo test discovery::admission: 통과
cargo build --release: 통과
```

수정 후 재실행 명령:

```bash
source .env
export PROMETHEUS_BIND=127.0.0.1:9899
RUST_LOG=info target/release/dex-arbitrage --chain base --once --simulate-only 2>&1 | tee state/discovery-rerun.log
```

재실행 시작 상태:

```text
discovery cache: 존재
pool state cache: 없음
fetch plan:
  cached: 0
  v2_to_fetch: 2,994,196
  v3_to_fetch: 1,859,420
  other_to_fetch: 1,317
```

최종 재실행 결과:

```text
명령:
  source .env
  export PROMETHEUS_BIND=127.0.0.1:9899
  RUST_LOG=info target/release/dex-arbitrage --chain base --once --simulate-only 2>&1 | tee state/discovery-rerun.log

종료 코드: 0
실매매 트랜잭션 전송: 없음
마지막 로그:
  bootstrap complete chain=base token_count=40636 pool_count=43423 snapshot_id=0
```

`--once` 실행은 discovery bootstrap을 끝낸 뒤 초기 `process_refresh`를 1회 실행하고 종료한다. 따라서 이번 실행은 Base discovery 캐시 생성, pool state 캐시 생성, token metadata 캐시 생성, 초기 simulate-only refresh 1회를 정상 통과했다.

저장된 캐시:

```text
state/base_discovery_cache.json: 약 1.1GB
state/base_pool_state_cache.json: 약 33MB
state/base_token_metadata_cache.json: 약 3.5MB
state/discovery-rerun.log: 약 1.3MB
```

최종 pool state 구성:

```text
UniswapV2Like: 29,893
UniswapV3Like: 13,062
CurvePlain: 49
BalancerWeighted: 419
총 pool_count: 43,423
```

non-v2/v3 진행 기록:

```text
fetched=500  admitted=42,981 skipped=375
fetched=1000 admitted=43,200 skipped=615
post_fetch checkpoint pools=43,423
```

재시도 후 스킵된 기타 pool:

```text
전체 skipped: 670
Curve skipped: 579
Balancer skipped: 91
retry 로그 수: 4,185
execution reverted 포함 로그 수: 124
rate limit 관련 로그 수: 84
```

해석:

```text
V2/V3는 Base 전체 factory 스캔 후 유동성이 있는 pool만 캐시에 남겼다.
Curve/Balancer는 현재 어댑터가 읽을 수 있는 pool만 캐시에 남기고, 인터페이스 불일치 또는 eth_call 실패 pool은 재시도 후 스킵했다.
이 스킵은 실매매 트랜잭션 실패가 아니라 discovery 상태 조회 단계의 unsupported/unreadable pool 제외다.
```

추가로 확인된 병목:

```text
최종 pool cache의 고유 token address 수: 약 40,635
token metadata cache 저장 항목: 40,631
token metadata 조회는 현재 코드상 순차 symbol/decimals eth_call이라 시간이 오래 걸린다.
다음 최적화 대상은 token metadata 조회의 multicall/batch화와 skipped pool tombstone 캐시다.
```

## 11. 2026-04-16 discovery 재실행 최적화 적용

Base simulate-only 검증 이후 발견된 병목을 줄이기 위해 discovery 재실행 최적화를 적용했다.

적용 내용:

```text
token metadata 조회를 multicall batch 방식으로 변경
V2/V3 batch fetch가 스킵된 pool과 reason을 반환하도록 변경
pool_state_cache에 skipped_pools tombstone 필드 추가
non-v2/v3 스킵 pool도 tombstone cache에 저장
execution reverted / decode 계열 오류는 불필요한 5회 재시도를 줄이도록 non-retryable 처리
```

새 환경값:

```env
TOKEN_METADATA_MULTICALL_CHUNK_SIZE=250
POOL_FETCH_OTHER_MAX_RETRIES=0
DISCOVERY_FETCH_UNSEEN_POOLS=false
DISABLE_SKIPPED_POOL_CACHE=false
SKIPPED_POOL_CACHE_TTL_SECS=2592000
USE_V3_RPC_QUOTER=false
VERIFY_V3_WITH_RPC_QUOTER=true
VERIFY_V3_REFINEMENT_POINTS=5
VERIFY_V3_EARLY_EXIT=true
VERIFY_V3_EARLY_PROBE_POINTS=1
SEARCH_MAX_CANDIDATES_PER_REFRESH=32
SEARCH_CANDIDATE_SELECTION_BUFFER_MULTIPLIER=4
SEARCH_DEDUP_TOKEN_PATHS=true
INITIAL_REFRESH_MAX_EDGES=0
BOOTSTRAP_REPLAY_CACHED_POOL_STATES=true
BOOTSTRAP_REPLAY_MAX_BLOCKS=900
EVENT_INGEST_MODE=wss
EVENT_WSS_FILTER_MODE=address_logs
EVENT_WSS_RECONCILE_MODE=topic_logs
EVENT_WSS_RECONCILE_STRATEGY=adaptive
EVENT_WSS_RECONCILE_INTERVAL_BLOCKS=2
EVENT_WSS_RECONCILE_INTERVAL_MS=10000
EVENT_WSS_AUDIT_INTERVAL_BLOCKS=64
EVENT_WSS_AUDIT_INTERVAL_MS=120000
EVENT_WSS_RECONCILE_CONFIRMATION_BLOCKS=1
EVENT_WSS_RECONCILE_BURST_THRESHOLD=16
EVENT_WSS_RECONCILE_BURST_MS=30000
EVENT_WSS_RECENT_LOG_CACHE=100000
EVENT_WSS_FILTER_STATS_INTERVAL_MS=30000
EVENT_LOG_CHUNK_BLOCKS=100
EVENT_LOG_ADDRESS_CONCURRENCY=16
EVENT_LOG_CHUNK_MAX_RETRIES=8
EVENT_LOG_MIN_CHUNK_BLOCKS=25
EVENT_LOG_RETRY_BASE_DELAY_MS=500
EVENT_RECEIPT_MAX_BLOCKS=64
EVENT_RECEIPT_CONCURRENCY=8
EVENT_RECEIPTS_FALLBACK_TO_TOPIC_LOGS=false
RPC_USAGE_LOG_INTERVAL_SECS=30
NO_ROUTE_SAMPLE_LOG_LIMIT=0
BASE_L1_FEE_TX_SIZE_OVERHEAD_BYTES=160
```

의미:

```text
TOKEN_METADATA_MULTICALL_CHUNK_SIZE:
  symbol/decimals 조회를 한 multicall에 몇 token씩 묶을지 정한다.
  token 1개당 symbol + decimals 2개 call이 들어간다.

POOL_FETCH_OTHER_MAX_RETRIES:
  V2/V3 batch fetch가 아닌 Curve/Balancer 개별 pool fetch의 재시도 횟수다.
  unsupported pool이 많은 구간에서 같은 실패를 오래 반복하지 않게 기본 0으로 둔다.

DISABLE_SKIPPED_POOL_CACHE:
  true로 두면 skipped pool tombstone cache를 사용하지 않는다.

DISCOVERY_FETCH_UNSEEN_POOLS:
  false면 pool_state_cache가 있을 때 기존에 검증된 pool만 재검증하고,
  아직 검증된 적 없는 discovered pool 수백만 개를 자동 fetch하지 않는다.
  전체 신규 pool까지 다시 검증하려면 true로 바꾼다.

SKIPPED_POOL_CACHE_TTL_SECS:
  스킵된 pool을 다시 조회하지 않을 TTL이다.
  기본 2592000초, 즉 30일이다.
  영구 제외가 아니라 TTL 이후 다시 검증한다.
  새로 생성된 pool은 discovery 증분 스캔으로 계속 발견되며,
  이 값은 이미 실패한 pool을 매 실행마다 다시 eth_call하지 않기 위한 비용 제한값이다.

STALENESS_TIMEOUT_MS:
  Base 전체 bootstrap은 수만 개 pool refresh 자체가 2분을 넘길 수 있다.
  120000ms로 두면 앞에서 갱신한 pool이 bootstrap 후반에 다시 stale 처리될 수 있어
  Base pre-live 기본값을 900000ms, 즉 15분으로 조정했다.

USE_V3_RPC_QUOTER:
  true면 V3 후보 quote마다 QuoterV2 eth_call이 발생한다.
  Base 전체 그래프에서는 후보 수가 커져 초기 구동 지연과 RPC 비용이 커지므로 false로 둔다.

VERIFY_V3_WITH_RPC_QUOTER:
  true면 빠른 fallback 탐색으로 고른 최종 후보 금액만 Uniswap V3/Aerodrome Slipstream quoter로 다시 검증한다.
  이 방식은 전체 탐색을 RPC quoter로 바꾸지 않으면서 V3 fallback quote의 false positive를 제거하기 위한 2단계 검증이다.
  2026-04-18 스모크에서 V3 fallback 기준 후보 34개가 정확 quoter 검증 후 모두 no_route 처리됐다.
  즉 현재 블록 기준으로 실행 가능한 양수 기대수익 후보가 없다는 뜻이지, 엔진이 멈춘 것은 아니다.

VERIFY_V3_REFINEMENT_POINTS:
  최종 후보 input amount 주변에서 정확 V3 quoter로 재검증할 금액 개수다.
  기본 5개이며, center 금액과 0.85x/1.15x/0.7x/1.3x 순서로 제한된 범위 안에서 확인한다.

VERIFY_V3_EARLY_EXIT:
  true면 V3가 포함된 후보는 초기 ladder에서 나온 best 금액을 바로 정확 quoter로 검증한다.
  정확 quoter에서 no_route면 수천 개 fallback 금액 탐색을 생략한다.
  Base smoke에서 V3 fallback false positive가 반복됐기 때문에 pre-live 기본값은 true다.
  이는 DEX/pool/symbol 범위를 줄이는 것이 아니라, 부정확한 fallback quote에 시간을 쓰는 순서를 줄이는 latency guard다.

VERIFY_V3_EARLY_PROBE_POINTS:
  early exit에서 정확 quoter로 먼저 확인할 금액 개수다.
  기본 1개는 fallback ladder의 best 금액만 확인한다.
  여기서 실제 실행 가능한 양수 후보가 나오면 그때 `VERIFY_V3_REFINEMENT_POINTS` 기준으로 추가 refinement를 수행한다.
  즉 false positive에는 빠르게 탈락시키고, 진짜 후보에만 더 비싼 정확 탐색을 쓴다.

SEARCH_MAX_CANDIDATES_PER_REFRESH:
  refresh 1회에서 exact sizing/validation까지 넘길 후보 수 상한이다.
  심볼, 거래소, pool discovery 범위를 줄이는 설정이 아니라,
  발견된 후보 중 screening score가 높은 상위 후보부터 처리하는 latency guard다.
  Base pre-live 기본값은 32다. 64개 관찰에서 steady refresh가 3.3-5.6초였으므로,
  Base block cadence에 더 가깝게 맞추기 위해 우선 32개를 검증한다.

SEARCH_CANDIDATE_SELECTION_BUFFER_MULTIPLIER:
  후보 처리 상한과 후보 선별 범위를 분리한다.
  예를 들어 max candidates 32, multiplier 4면 detector는 반복 중 처음 32개에서 멈추지 않고
  최대 128개 후보를 모은 뒤 screening score 상위 32개만 router/exact 검증으로 넘긴다.
  이는 expensive 검증 수는 유지하면서 반복 순서 때문에 더 좋은 후보가 밀리는 위험을 줄이기 위한 수익성 guard다.

SEARCH_DEDUP_TOKEN_PATHS:
  true면 같은 토큰 순환 경로는 pool ID만 다르더라도 한 후보로 합친다.
  현재 router는 각 hop에서 해당 token pair의 여러 pool을 다시 최적화하고 split할 수 있으므로,
  같은 token path 후보가 여러 슬롯을 차지하면 실질적으로 같은 경로를 반복 검증하는 비용이 커진다.
  이 값은 DEX/pool discovery 범위를 줄이는 설정이 아니라, 후보 슬롯을 더 다양한 token path에 쓰기 위한 selection guard다.

INITIAL_REFRESH_MAX_EDGES:
  bootstrap 직후 첫 detect 단계에서 한 번에 평가할 edge 수 상한이다.
  0이면 전체 edge를 평가한다.
  2026-04-19 Base smoke에서 전체 초기 edge 80783개 기준 detect_ms=182, refresh_total_ms=86이었으므로
  수익 후보 보존을 위해 pre-live 기본값은 0으로 둔다.
  값을 양수로 두면 이미 구성된 전체 Base 그래프에서 플래시론/앵커 도달성, USD 기준 유동성,
  풀 신뢰도가 높은 대표 pair부터 초기 후보 탐색에 넣어 첫 구동 지연을 제한한다.

EVENT_INGEST_MODE / EVENT_WSS_* / EVENT_RECEIPT_*:
  WSS 구독 자체는 address_logs로 둔다.
  이는 pool 주소 필터를 RPC에 싣기 때문에 Base 전체 topic stream을 모두 받는 것보다
  websocket bandwidth 비용을 줄이는 목적이다.
  반면 WSS 누락 보정은 topic_logs로 둔다.
  watched pool이 약 4.3만 개일 때 address_logs 보정은 주소 200개 단위로 쪼개져
  보정 1회당 eth_getLogs가 약 215회 발생했다.
  topic_logs 보정은 같은 블록 범위를 topic 기준 1회 조회한 뒤 로컬에서 watched pool만 필터링하므로,
  알파 보존을 유지하면서 RPC 호출 수를 크게 줄인다.

NO_ROUTE_SAMPLE_LOG_LIMIT:
  기본 0이면 no-route 후보별 상세 로그를 출력하지 않는다.
  진단할 때만 5-10 정도로 켜면 상위 탈락 후보의 route, dex_route, no-output 위치, AMM 종류를 로그에 남긴다.
  실매매 상시 실행에서는 로그 I/O 지연을 피하기 위해 0으로 둔다.
  2026-04-18 Base 검증에서 Alchemy는 eth_getBlockReceipts를 403 unsupported로 거부했다.
  그래서 pre-live 기본값은 WSS address filter + topic_logs reconcile이다.
  EVENT_WSS_FILTER_MODE=address_logs는 WSS 구독 자체에 watched pool 주소 필터를 싣는다.
  EVENT_WSS_RECONCILE_MODE=topic_logs는 누락 보정 시 Base-wide topic 조회 후 로컬 필터링을 사용한다.
  EVENT_LOG_ADDRESS_CONCURRENCY는 address_logs backfill의 주소 chunk 병렬도다.
  총 RPC 호출량을 늘리는 값이 아니라 같은 address chunk 조회를 더 빨리 끝내기 위한 값이다.
  EVENT_LOG_CHUNK_BLOCKS는 bootstrap replay와 WSS 보정에서 한 번에 조회할 block 범위다.
  Base topic-only log query는 큰 범위에서 503이 날 수 있어 pre-live 기본값을 100으로 낮췄다.
  실시간 WSS reconcile은 보통 2블록 단위라 지연시간에는 거의 영향을 주지 않고,
  장시간 꺼졌다 켜질 때만 더 작은 단위로 안정적으로 따라잡는다.
  EVENT_LOG_CHUNK_MAX_RETRIES / EVENT_LOG_MIN_CHUNK_BLOCKS / EVENT_LOG_RETRY_BASE_DELAY_MS는 eth_getLogs가
  503, rate limit, backend unhealthy 같은 일시 장애를 낼 때 같은 블록 구간을 재시도하는 설정이다.
  같은 큰 구간이 반복 실패하면 `EVENT_LOG_MIN_CHUNK_BLOCKS`까지 block chunk를 줄여 재시도한다.
  재시작 replay 중 provider가 순간적으로 실패해도 bot 전체가 중단되지 않게 하기 위한 값이다.
  EVENT_RECEIPT_CONCURRENCY / EVENT_RECEIPTS_FALLBACK_TO_TOPIC_LOGS는 receipt 모드를 별도 provider에서 재시험할 때만 사용한다.
  EVENT_RECEIPTS_FALLBACK_TO_TOPIC_LOGS=false는 receipt 조회 실패 시 다시 비싼 topic_logs로 새는 것을 막는다.
  WSS 검증 때는 EVENT_WSS_FILTER_STATS_INTERVAL_MS 로그로 raw/watched/discarded WSS 로그 수를 확인한다.
  RPC_USAGE_LOG_INTERVAL_SECS는 실행 중 method/provider별 RPC 요청 수와 추정 CU를 주기적으로 출력한다.

BOOTSTRAP_REPLAY_CACHED_POOL_STATES:
  true면 pool_state_cache에 저장된 기준 블록 이후 이벤트 로그만 replay해서 재시작 상태를 맞춘다.
  매번 4만 개 이상의 pool을 eth_call로 다시 읽지 않기 위한 설정이다.
  단, 이전 cache에 기준 블록이 없으면 안전하게 한 번은 full refresh가 필요하고,
  그 다음 저장된 cache부터 replay 방식이 적용된다.

BOOTSTRAP_REPLAY_MAX_BLOCKS:
  cached pool event replay를 허용할 최대 블록 gap이다.
  기본 900블록은 Base 기준 대략 30분 수준이다.
  gap이 이보다 크면 수만 블록의 `eth_getLogs`를 순차 replay하는 대신,
  cached pool의 현재 상태를 batch refresh한다.
  오래 꺼져 있다가 재시작할 때 RPC 비용과 시작 지연을 제한하기 위한 운영 guard다.
  0으로 두면 gap 제한 없이 항상 replay를 시도한다.

BASE_L1_FEE_TX_SIZE_OVERHEAD_BYTES:
  Base 트랜잭션 비용은 L2 실행비와 L1 데이터비로 나뉜다.
  실매매 순이익 계산에서는 gas limit * L2 gas price만 보면 부족하므로,
  Base GasPriceOracle의 getL1FeeUpperBound(txSize) 값도 gas_cost_wei에 포함한다.
  txSize는 서명 전에는 정확한 RLP tx 크기를 모르므로 calldata 길이에 이 overhead를 더해 추정한다.
  기본 160 bytes는 보수적인 pre-sign 추정값이며, 실제 체결 영수증을 수집한 뒤 조정한다.

USE_GAS_ESTIMATE_RPC:
  false가 기본이다.
  후보 검증에서는 revert가 정상적으로 많이 발생하므로 estimateGas를 매번 호출하면 latency와 CU가 낭비된다.
  기본 실행은 보수적 정적 gas limit + Base L1 data fee + fresh eth_call simulation으로 검증한다.

STATIC_GAS_LIMIT_BUFFER_BPS / FLASH_LOAN_GAS_OVERHEAD:
  estimateGas를 끈 상태에서 gas cost를 과소평가하지 않기 위한 보수 버퍼다.
  flash loan 경로는 Aave 호출 overhead를 추가한 뒤 buffer bps를 적용한다.

DISCOVERY_SAVE_AFTER_EACH_DEX:
  false가 기본이다.
  discovery_cache가 1GB를 넘을 수 있어 DEX마다 저장하면 재시작 시간이 크게 늘어난다.
  기본은 전체 DEX discovery 후 한 번만 저장한다.

DISCOVERY_REFRESH_CONFIGURED_SEEDS:
  false가 기본이다.
  configured-token seed pool은 cache가 없을 때 생성하고, 이후 재시작에서는 cache를 재사용한다.
  토큰 목록을 바꾼 뒤 seed를 강제로 다시 만들고 싶을 때만 true로 둔다.
```

주의:

```text
현재 이미 생성된 state/base_pool_state_cache.json은 이전 포맷이라 skipped_pools 필드가 없다.
새 코드는 이 이전 포맷을 기본값으로 읽을 수 있다.
다만 skipped_pools tombstone은 다음 discovery 실행에서 새로 저장된 뒤부터 효과가 있다.
현재 token metadata cache는 이미 생성되어 있으므로 다음 실행의 token metadata 구간은 기존보다 훨씬 짧아져야 한다.
2026-04-19에 Aerodrome V2 adapter와 최신 실행 안전장치를 포함한 새 executor를 배포했고,
Safe에서 새 executor에 operator 권한을 다시 등록했다.
현재 `.env`의 `BASE_EXECUTOR_ADDRESS`는 최신 executor인 `0xDDe9...7dCC`다.
```

검증:

```text
cargo fmt --check: 통과
cargo test --lib: 통과, 55 tests
cargo build --release: 통과
forge build: 통과, lint note/warning만 존재

2026-04-18 Base smoke:
  log: state/base-v3-verify-clean-smoke-20260419-002955.log
  pool state fetch plan:
    cached=45604
    v2_to_fetch=0
    aerodrome_v2_to_fetch=0
    v3_to_fetch=0
    other_to_fetch=3
    skipped_cache_hits=3817
  bootstrap:
    token_count=42567
    pool_count=45604
    total_edges=91208
  initial detection:
    selected_edges=4096
    candidate_count=34
    detect_ms=145
  exact V3 verification:
    no_route=34
    simulated=0
    refresh_total_ms=29651
  해석:
    현재 시점에는 V3 fallback 기준 후보가 있었지만,
    정확 quoter 검증 기준 실행 가능한 양수 기대수익 후보는 없었다.

2026-04-19 Base post-deploy smoke:
  log: state/base-post-deploy-smoke-20260419-015144.log
  executor:
    0xDDe9FDB14AF542B064334297808ec976E7cF7dCC
  operator allowed:
    true
  replay configuration:
    EVENT_LOG_CHUNK_BLOCKS=100
    EVENT_LOG_CHUNK_MAX_RETRIES=8
    EVENT_LOG_MIN_CHUNK_BLOCKS=25
  pool state fetch plan:
    cached=45607
    stale_cached_to_refresh=0
    v2_to_fetch=0
    aerodrome_v2_to_fetch=0
    v3_to_fetch=0
    other_to_fetch=3
    skipped_cache_hits=3817
    unseen_pool_skips=4835669
  event replay:
    topic log replay succeeded
    replay logs after watched-pool filtering=170638
    trigger_count=857
    non-patchable full_refresh_count=249
  bootstrap:
    token_count=42567
    pool_count=45607
    total_edges=91214
  initial detection:
    selected_edges=4096
    candidate_count=34
    detect_ms=148
  profitability refresh:
    no_route=34
    validation_rejected=0
    risk_rejected=0
    simulated=0
    submitted=0
    avg_candidate_ms=832
    refresh_total_ms=28303
  해석:
    새 executor 배포 및 Safe operator 등록 이후 simulate-only 실행 경로가 정상 통과했다.
    해당 블록에서는 정확 quoter/실행 가능성 기준 양수 기대수익 후보가 없어서 제출은 없었다.

2026-04-19 V3 early exact 검증 최적화:
  문제:
    V3 fallback quote 기준 후보는 계속 나오지만 exact quoter 기준에서는 no_route가 반복됐다.
    처음 구현은 fallback으로 수천 개 금액을 탐색한 뒤 마지막에 exact 검증을 해서 초기 refresh가 약 28.3초 걸렸다.
  1차 개선:
    VERIFY_V3_EARLY_EXIT=true로 초기 ladder best를 바로 exact 검증했다.
    금액 평가 수는 5360개에서 226개로 줄었지만, exact split optimizer가 후보마다 pair 전체를 다시 quote해서 약 61.5초로 악화됐다.
  최종 개선:
    early probe에서는 fallback이 이미 선택한 split/pool만 exact quote한다.
    이 split-level exact probe가 통과한 경우에만 전체 exact split optimizer/refinement를 수행한다.
  검증:
    cargo test --lib: 통과, 56 tests
    cargo build --release: 통과
    forge build: 통과, 기존 lint note/warning만 존재
  once smoke:
    log: state/base-v3-split-probe-smoke-20260419-020627.log
    candidate_count=34
    no_route=34
    simulated=0
    submitted=0
    avg_candidate_ms=78
    max_candidate_ms=357
    refresh_total_ms=2683
  steady 2분 관찰:
    log: state/base-steady-split-probe-observe-20260419-020701.log
    steady refresh samples=13
    avg_candidate_count=64
    avg_refresh_ms=4169
    min_refresh_ms=3265
    max_refresh_ms=5602
    avg_candidate_ms=64
    simulated=0
    submitted=0
    rpc usage summaries=3
    summarized requests=762
    summarized CU=19890
  해석:
    Base 전체 venue/symbol 범위는 유지했다.
    후보는 충분히 나오지만 현재 관찰 구간에서는 exact 기준 실행 가능한 양수 기대수익 후보가 없었다.
    핵심 latency는 이전 steady 12-20초대에서 3.3-5.6초대로 감소했다.

2026-04-19 후보 상한 32 steady 관찰:
  설정:
    SEARCH_MAX_CANDIDATES_PER_REFRESH=32
    SEARCH_CANDIDATE_SELECTION_BUFFER_MULTIPLIER=1
    Base 전체 venue/symbol/pool discovery 범위는 그대로 유지
  log:
    state/base-steady-c32-observe-20260419-021336.log
  검증:
    cargo fmt --check: 통과
    cargo test --lib: 통과, 56 tests
    cargo build --release: 통과
  steady 2분 관찰:
    steady refresh samples=7
    avg_candidate_count=32
    avg_refresh_ms=1643
    min_refresh_ms=1238
    max_refresh_ms=2299
    avg_candidate_ms=50
    avg_max_candidate_ms=536
    simulated=0
    submitted=0
    no_route=224
    rpc usage summaries=3
    summarized requests=264
    summarized CU=7020
  해석:
    후보 64개 관찰 대비 refresh 지연은 4.169초 평균에서 1.643초 평균으로 감소했다.
    요약된 RPC CU도 19890에서 7020으로 감소했다.
    이는 알파 범위 자체를 줄인 것이 아니라, 한 refresh에서 exact 검증할 상위 후보 예산을 Base block cadence에 맞춘 것이다.
    관찰 구간에서는 여전히 exact 기준 실행 가능한 양수 기대수익 후보가 없었다.

2026-04-19 후보 선별 버퍼 개선:
  문제:
    기존 detector는 후보가 max_candidates_per_refresh에 도달하면 즉시 반환했다.
    따라서 실제로는 "전체 후보 중 상위 32개"가 아니라 "반복 순서상 처음 발견된 32개를 정렬한 결과"가 될 수 있었다.
  수정:
    SEARCH_CANDIDATE_SELECTION_BUFFER_MULTIPLIER를 추가했다.
    Base 기본값은 4이므로 detector는 최대 128개 후보를 먼저 모으고, screening score 상위 32개만 expensive 검증으로 넘긴다.
  의도:
    router/exact 검증 지연은 32개 후보 예산으로 제한하면서,
    반복 순서 때문에 더 좋은 후보가 뒤에 있어도 놓칠 위험을 줄인다.
  buffer 4 관찰:
    log: state/base-steady-c32-buffer4-observe-20260419-021838.log
    steady refresh samples=25
    avg_candidate_count=32
    avg_refresh_ms=1489
    min_refresh_ms=242
    max_refresh_ms=3115
    avg_candidate_ms=45
    avg_max_candidate_ms=474
    simulated=0
    submitted=0
    no_route=800
    route_profitable_plans=401
    rpc usage summaries=3
    summarized requests=688
    summarized CU=17742
  buffer 2 비교:
    log: state/base-steady-c32-buffer2-observe-20260419-022115.log
    steady refresh samples=4
    avg_refresh_ms=4638
    min_refresh_ms=915
    max_refresh_ms=7680
    summarized requests=195
    summarized CU=5180
  결론:
    buffer 4는 buffer 1 대비 RPC CU는 증가하지만, detector 반복 순서 때문에 후보 품질이 떨어지는 위험을 줄인다.
    수익성 우선 pre-live 기본값은 4로 유지한다.
    관찰 구간에서는 여전히 exact 기준 실행 가능한 양수 기대수익 후보가 없어 제출은 없었다.

2026-04-19 env check 범위 보정:
  수정:
    scripts/check_env.sh의 base/live 필수값에 Aerodrome V2, Aerodrome Slipstream legacy/caps/current factory와 quoter env를 추가했다.
  검증:
    scripts/check_env.sh base live: 통과
  이유:
    Base 실매매 범위에 Aerodrome을 포함했으므로 env 검증도 같은 범위를 확인해야 한다.

2026-04-19 token path dedup 및 warm smoke:
  문제:
    no-route sample에서 같은 token path가 pool ID만 바뀐 형태로 여러 candidate slot을 차지했다.
    현재 router는 각 hop의 token pair에 대해 여러 pool을 다시 quote/split하므로,
    같은 token path를 pool ID별 후보로 반복 검증하는 것은 대부분 중복 비용이었다.
  수정:
    SEARCH_DEDUP_TOKEN_PATHS=true를 추가했다.
    detector는 pool-specific cycle_key 중복 제거를 유지하되, 기본적으로 token path 단위로 후보를 한 번 더 합친다.
    quantity search의 route capacity도 특정 후보 pool 하나가 아니라 해당 token pair의 상위 split 대상 pool 용량 합을 기준으로 잡는다.
    따라서 token path dedup이 최적 진입금액 상한을 불필요하게 낮추는 위험을 줄였다.
  replay gap guard 검증:
    log: state/base-no-route-samples-gapguard-c300-20260419-170209.log
    replay_gap_blocks=26280, BOOTSTRAP_REPLAY_MAX_BLOCKS=900
    replay_cached_pools=false로 전환되어 장시간 eth_getLogs replay 대신 direct state refresh가 실행됐다.
    pool_count=45628, skipped_pools=3827, selected_edges=4096, candidate_count=32
  token dedup cold-ish smoke:
    log: state/base-token-dedup-smoke-20260419-171608.log
    replay_gap_blocks=688, replay_cached_pools=true
    candidate_count=32, detect_ms=150, refresh_total_ms=4876
    no_route=32, simulated=0, submitted=0
    route_evaluated_amounts=84, route_profitable_plans=21
    route_no_output_v2=24, route_no_output_aerodrome_v2=3, route_no_output_v3=29
  token dedup warm smoke:
    log: state/base-token-dedup-warm-smoke-20260419-171732.log
    replay_gap_blocks=40, stale_cached_to_refresh=0, stale_cached_to_replay=0
    bootstrap complete pool_count=45628, token_count=42565
    selected_edges=4096, candidate_count=32, detect_ms=154
    refresh_total_ms=1745, avg_candidate_ms=54, max_candidate_ms=339
    no_route=32, simulated=0, submitted=0
  V3 early probe 비교:
    log: state/base-v3-probe3-smoke-20260419-172015.log
    VERIFY_V3_EARLY_PROBE_POINTS=3, refresh_total_ms=4532, simulated=0, submitted=0
    log: state/base-v3-probe5-smoke-20260419-172037.log
    VERIFY_V3_EARLY_PROBE_POINTS=5, refresh_total_ms=7323, simulated=0, submitted=0
    추가 probe는 실행 가능 후보를 찾지 못하고 지연만 늘렸으므로 기본값 1을 유지한다.
  후보 64 비교:
    log: state/base-c64-token-dedup-smoke-20260419-172115.log
    SEARCH_MAX_CANDIDATES_PER_REFRESH=64, candidate_count=64, refresh_total_ms=3810
    no_route=64, simulated=0, submitted=0
    후보 64도 실행 가능 후보를 찾지 못했고, 지연만 늘어 기본값 32를 유지한다.
  후보 256 비교:
    log: state/base-c256-full-initial-smoke-20260419-172437.log
    SEARCH_MAX_CANDIDATES_PER_REFRESH=256, INITIAL_REFRESH_MAX_EDGES=0
    selected_edges=80783, candidate_count=256, detect_ms=41772, refresh_total_ms=11537
    no_route=256, simulated=0, submitted=0
    후보 256은 detector 시간이 41.7초까지 늘었고 실행 가능 후보도 없었으므로 실매매 기본값으로 부적합하다.
  전체 초기 edge 비교:
    log: state/base-full-initial-edges-smoke-20260419-172148.log
    INITIAL_REFRESH_MAX_EDGES=0
    selected_edges=80783, total_edges=91256, candidate_count=32, detect_ms=182
    refresh_total_ms=86, no_route=32, simulated=0, submitted=0
    전체 초기 edge 평가 비용이 작았으므로 초기 검증 범위 축소를 제거하고 기본값을 0으로 변경했다.
  V3 exact quoter price limit 정렬:
    문제:
      기존 V3 RPC quoter 검증은 sqrtPriceLimitX96=0으로 quote했지만,
      실제 calldata는 V3_SQRT_PRICE_LIMIT_BPS 기준 sqrtPriceLimitX96를 넣었다.
      따라서 검증은 통과했지만 실제 실행에서는 price limit 때문에 실패하는 후보가 생길 수 있었다.
    수정:
      Uniswap V3와 Aerodrome Slipstream RPC quoter 검증도 실행 calldata와 같은 price limit을 사용하도록 맞췄다.
    검증:
      cargo test router::exact_quoter::tests::v3_quoter_price_limit_matches_execution_default_bps: 통과
      cargo test --lib: 통과, 58 tests
      cargo build --release: 통과
      log: state/base-default-after-v3-limit-smoke-20260419-172759.log
      selected_edges=80783, candidate_count=32, detect_ms=183, refresh_total_ms=95
      no_route=32, simulated=0, submitted=0
  기본 설정 3분 연속 simulate-only 관찰:
    log: state/base-default-steady-after-v3-limit-20260419-172908.log
    refresh_samples=31
    avg_refresh_ms=2088
    min_refresh_ms=115
    max_refresh_ms=3253
    avg_candidate_count=32.0
    no_route_total=992
    simulated_total=0
    submitted_total=0
    route_profitable_plans_total=307
    rpc_summaries=5
    summarized_requests=1144
    summarized_cu=29340
    해석:
      기본 설정은 연속 루프에서 대체로 2-3초 refresh 범위에 머문다.
      관찰 구간에서는 fallback 단계의 양수 후보가 있었지만 exact quote, 수수료, 실행 가능성 검증 후 제출 가능한 후보는 없었다.
  결론:
    Base 전체 venue/symbol/pool discovery 범위는 유지했다.
    후보 처리 지연은 warm 기준 1.745초로 Base block cadence에 근접했다.
    현재 관찰 블록에서는 exact quote와 비용 계산 후 실행 가능한 양수 기대수익 후보가 없어서 제출은 없었다.
    다음 개선 초점은 no-output의 실제 원인 분해다.
    특히 V3 exact quote 탈락, V2/AerodromeV2 hop no-output, 낮은 용량으로 인한 amount range missing을 분리해서
    후보 품질과 수익 후보 발견률을 더 개선해야 한다.
```

## 2026-04-19 추가 라우터 최적화

목표:
- Base 전체 venue/symbol 범위는 유지한다.
- fallback V3 가격 근사 때문에 생기는 false-positive 후보가 실제 exact quote 단계에서 계속 RPC와 시간을 소모하는 문제를 줄인다.
- 실행 가능한 후보를 놓치지 않도록, V3 검증 시 fallback이 고른 단일 split만 재검증하지 않고 같은 token pair의 대체 pool까지 exact quoter로 다시 최적화한다.

수정:
- `VERIFY_V3_EARLY_REOPTIMIZE=true`
  - V3 후보의 early exact 검증에서 기존 split 고정 검증 대신 exact quoter 기반 pair 재최적화를 수행한다.
- `ROUTE_PAIR_EDGE_SCAN_LIMIT=24`
  - pair별 실제 실행 split 수(`MAX_SPLIT_PARALLEL_POOLS=5`)는 유지하면서, 후보 pool 탐색 폭만 넓힌다.
  - 상위 false-price pool 5개가 모두 실패해도 같은 token pair의 다른 pool을 확인할 수 있다.
- `V3_DIRECTION_REVERT_CACHE_THRESHOLD=2`
  - 같은 snapshot에서 동일 V3 pool/direction이 expected revert를 2회 발생시키면 이후 금액은 즉시 0으로 처리한다.
  - 반복 RPC 호출을 줄이고, 같은 실패 방향이 후보 처리 시간을 계속 잡아먹는 것을 막는다.
- detector 단계에 route-level 최소 거래금액 capacity filter를 추가했다.
  - hop 개별 capacity는 충분하지만 전체 route 기준으로 시작 자산 최소 거래금액을 처리할 수 없는 후보를 router 전에 제거한다.
- no-route sample 로그에 `first_no_output_hop`, `first_no_output_pool`, `first_no_output_amount_in`을 추가했다.

검증:
- `cargo test`: 통과, 62 tests
- `cargo build --release`: 통과
- `state/base-capacity-filter-reopt-smoke-20260419-202805.log`
  - `route_amount_range_missing=0`
  - `refresh_total_ms=4142`
  - `no_route=32`, `simulated=0`, `submitted=0`
- `state/base-edge-scan24-smoke-20260419-203023.log`
  - pool scan 폭을 넓혔지만 direction cache 전에는 `refresh_total_ms=18272`로 지연이 커졌다.
- `state/base-v3-direction-cache-smoke-20260419-203509.log`
  - direction cache 적용 후 `refresh_total_ms=5759`
  - `route_profitable_plans=29`
  - `no_route=32`, `simulated=0`, `submitted=0`
- `state/base-candidate128-scan24-smoke-20260419-203608.log`
  - 후보 예산을 128로 넓혔지만 실제 생성 후보는 57개였다.
  - `refresh_total_ms=10166`
  - `no_route=57`, `simulated=0`, `submitted=0`

해석:
- 이번 변경은 후보 범위를 임의 심볼/거래소 allowlist로 줄인 것이 아니다.
- 현재 관찰 블록에서는 exact quote 기준 실행 가능한 양수 기대수익 후보가 없었다.
- 후보 수를 57개까지 넓혀도 simulated 후보가 나오지 않았으므로, 해당 시점에 검증 가능한 수익 기회가 없었거나 V3 tick-level 근사가 아직 detector 단계에서 false-positive를 많이 만들고 있다.
- 적용 가치가 확인된 기본값은 `VERIFY_V3_EARLY_REOPTIMIZE=true`, `ROUTE_PAIR_EDGE_SCAN_LIMIT=24`, `V3_DIRECTION_REVERT_CACHE_THRESHOLD=2`다.
- 실매매 전 다음 개선 후보는 V3 pool/direction revert cache를 장기 상태로 승격하거나, V3 후보에 대해 cheap exact prefilter를 도입하는 것이다. 단, 이 작업은 RPC 비용과 후보 발견률 사이의 균형 검증이 필요하다.

## 2026-04-19 Base simulate-only 지연/정확도 최적화

목표:
- Base 전체 venue/symbol/pool 범위는 유지한다.
- 실제 알파 후보를 심볼, 커넥터, DEX allowlist로 잘라내지 않는다.
- fallback 가격에서 생기는 false-positive는 정확한 RPC quote와 실행 비용 계산으로 제거한다.
- 느린 후보 때문에 전체 snapshot refresh가 밀리지 않도록 후보 평가 지연을 제한한다.

수정:
- `USE_BALANCER_RPC_QUOTER=true`
  - Balancer fallback quote만으로 후보를 통과시키지 않고 `queryBatchSwap` 기반 exact quote를 사용한다.
  - 이전 smoke에서 발생한 `BAL#507` 검증 실패를 제거했다.
- `ROUTE_SEARCH_TIMEOUT_MS=1000`
  - 후보 하나가 exact quote 단계에서 1초를 넘기면 stale 후보로 간주하고 해당 후보만 버린다.
  - Base 차익매매에서는 1초 넘게 quote가 끝나지 않는 후보는 이미 블록 경쟁력이 낮다고 판단했다.
- `ROUTE_CANDIDATE_CONCURRENCY=16`
  - simulate-only 후보 평가는 동시에 최대 16개까지 수행한다.
  - live 제출 경로는 nonce/risk 보호를 위해 기존 순차 제출 흐름을 유지한다.
- `CANDIDATE_SELECTION_LARGE_REFRESH_THRESHOLD=8192`
  - 최초 대형 refresh에서는 후보 selection buffer multiplier를 1로 낮춰 전체 초기 edge scan을 빠르게 끝낸다.
  - 일반 live refresh에서는 기존 `SEARCH_CANDIDATE_SELECTION_BUFFER_MULTIPLIER=4`를 유지한다.
- `ROUTE_CAPACITY_PREFILTER_MAX_CHANGED_EDGES=8192`
  - 작은 refresh에서는 route-level 최소 거래금액 capacity prefilter를 적용한다.
  - 최초 대형 refresh에서는 prefilter 자체가 detector 병목이 되지 않도록 건너뛴다.
- profitability summary에 `router_timeouts`를 추가했다.
  - no-route 원인이 실제 quote 실패인지, gross nonpositive인지, timeout인지 분리해서 볼 수 있다.

검증:
- `cargo fmt`: 통과
- `cargo test`: 통과, 63 lib tests + integration tests
- `cargo build --release`: 통과
- `state/base-balancer-exact-steady-2m-20260419-204557.log`
  - `BAL#507` 재발 없음
  - validation reject 없음
  - simulated=0, submitted=0
- `state/base-current-env-timeout-smoke-20260419-211028.log`
  - timeout 전 순차 처리 기준:
    - snapshot 0: `refresh_total_ms=6888`
    - snapshot 1: `refresh_total_ms=26643`
  - 느린 후보가 전체 refresh를 밀어내는 문제가 확인됐다.
- `state/base-parallel-candidates-smoke-20260419-211449.log`
  - `ROUTE_SEARCH_TIMEOUT_MS=1000`, `ROUTE_CANDIDATE_CONCURRENCY=8`
  - snapshot 0: `refresh_total_ms=1120`
  - 이후 refresh는 대체로 2.0-3.0초 범위
  - simulated=0, submitted=0
- `state/base-concurrency16-smoke-20260419-211950.log`
  - `ROUTE_CANDIDATE_CONCURRENCY=16`
  - snapshot 0: `refresh_total_ms=1106`
  - 이후 refresh는 대체로 1.0-2.0초 범위
  - 30초 RPC summary:
    - 첫 구간: `total_requests=467`, `total_cu=12246`
    - 둘째 구간: `total_requests=1380`, `total_cu=35934`
  - simulated=0, submitted=0
- `state/base-no-route-score-samples-20260419-212131.log`
  - no-route 샘플의 주 원인은 V3/소형 풀 경유 중간 hop `no_output`, gross nonpositive, 일부 router timeout이었다.
  - 특정 DEX나 특정 심볼만의 문제가 아니므로 범위 축소형 allowlist는 적용하지 않았다.

해석:
- 실매매 범위를 줄이지 않고도 후보 처리 지연을 순차 26초대에서 1-2초대로 낮췄다.
- 후보는 여전히 Base 전체 discovery 결과에서 생성되며, 특정 심볼/DEX/커넥터 전용으로 축소하지 않았다.
- 관찰 구간에서는 exact quote, flash fee, gas, validator까지 통과한 실행 가능 수익 후보가 없었다.
- 이 결과는 “수익 없음”을 증명하지 않는다. 해당 관찰 블록들에서 실행 가능한 후보가 없었고, 현재 세팅은 false-positive를 빠르게 제거하도록 조정됐다는 의미다.
- 다음 실매매 전 확인 포인트:
  - 더 긴 simulate-only 관찰에서 `simulated > 0` 후보가 실제로 발생하는지 확인한다.
  - 발생 시 `best_net_profit_usd_e8`, `best_gas_to_gross_bps`, `capital_source`를 기준으로 최소 수익/최대 포지션을 재조정한다.
  - live 전환 전에는 `SIMULATION_ONLY=false` 변경, 보호 RPC/프라이빗 제출 경로, 손실 한도, executor allowlist 정책을 별도로 재검증해야 한다.
