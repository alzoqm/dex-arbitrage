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
venues = ["uniswap_v2", "sushiswap_v2", "baseswap_v2", "uniswap_v3", "curve", "balancer"]
symbols = []
```

의미:

- `symbols = []`라서 설정상 특정 심볼만으로 제한하지 않는다.
- 현재 코드가 지원하고 주소 검증이 끝난 Base venue 전체를 대상으로 한다.
- Aerodrome은 아직 adapter/주소/실행 호환성 검증이 끝나지 않아 비활성이다.

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
Executor: 0x39cFf9ff02aE6dE82553a611c30D943847F2De55
Executor deploy tx: 0x0ab4a52cc00aac49b9fba150544c3fa5271a8878a0039ad488556bdd5cb8fc85
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
POOL_FETCH_MULTICALL_CHUNK_SIZE=150
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
DISABLE_SKIPPED_POOL_CACHE=false
SKIPPED_POOL_CACHE_TTL_SECS=86400
```

의미:

```text
TOKEN_METADATA_MULTICALL_CHUNK_SIZE:
  symbol/decimals 조회를 한 multicall에 몇 token씩 묶을지 정한다.
  token 1개당 symbol + decimals 2개 call이 들어간다.

DISABLE_SKIPPED_POOL_CACHE:
  true로 두면 skipped pool tombstone cache를 사용하지 않는다.

SKIPPED_POOL_CACHE_TTL_SECS:
  스킵된 pool을 다시 조회하지 않을 TTL이다.
  기본 86400초, 즉 24시간이다.
  영구 제외가 아니라 TTL 이후 다시 검증한다.
```

주의:

```text
현재 이미 생성된 state/base_pool_state_cache.json은 이전 포맷이라 skipped_pools 필드가 없다.
새 코드는 이 이전 포맷을 기본값으로 읽을 수 있다.
다만 skipped_pools tombstone은 다음 discovery 실행에서 새로 저장된 뒤부터 효과가 있다.
현재 token metadata cache는 이미 생성되어 있으므로 다음 실행의 token metadata 구간은 기존보다 훨씬 짧아져야 한다.
```

검증:

```text
cargo fmt --check: 통과
cargo test discovery:: --lib: 통과
cargo test --lib: 통과, 52 tests
cargo build --release: 통과
```
