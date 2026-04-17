**먼저 결론**
Base로 진행하면 Polygon보다 private submission 준비가 단순하다. Alchemy 공식 문서 기준으로 Base는 Alchemy standard RPC 자체에 MEV Protection이 자동 적용된다. 즉 Polygon처럼 별도 Private Mempool access를 요청하는 흐름이 아니라, Base 앱의 HTTPS RPC를 `BASE_PUBLIC_RPC_URL`과 `BASE_PROTECTED_RPC_URL`에 같이 넣고 `eth_sendRawTransaction`으로 제출하면 된다.

현재 프로젝트 기준으로 Base 실매매에 바로 맞출 수 있는 범위는 다음이다.

```text
Base + Aave V3 flash loan
+ Uniswap V2
+ SushiSwap V2
+ BaseSwap V2
+ Uniswap V3
+ Curve MetaRegistry
+ Balancer Vault
```

아래는 아직 현재 코드에서 실매매 범위에 넣지 않는 것이 맞다.

```text
Aerodrome V2 = Solidly/ve(3,3) 계열이라 stable pool 수학과 router/adapter 검증이 별도로 필요
Aerodrome Slipstream/V3 = Uniswap V3와 비슷하지만 factory/quoter/콜백 호환성을 별도 검증해야 함
Base 전체 DEX = 각 DEX adapter와 quote/execute 검증이 필요
Base 전체 토큰 = 가격/디페그/flash-loan 가능 여부/토큰 동작 리스크 검증이 필요
```

따라서 지금 세팅은 “Base 체인 기준으로 현재 코드가 지원하고 온체인 검증한 주요 venue 전체”다. 특정 심볼만 제한하지는 않는다. `config/base.toml`의 `symbols = []`라서 심볼 필터는 풀려 있고, Aave reserve와 발견된 pool 토큰은 discovery 단계에서 확장될 수 있다. 다만 venue는 현재 코드가 지원하는 DEX adapter 범위로 제한된다.

진입/종료 anchor는 Aave에서 active, unpaused, flash-loan-enabled인 reserve가 되도록 맞춘다. stable이라는 이유만으로 anchor가 되지 않게 `USDC`, `USDT`의 `is_cycle_anchor = false`를 명시했고, Aave reserve 확인 단계에서 flash loan 가능한 자산만 다시 anchor로 승격된다. 또한 `allow_self_funded = false`로 맞춰 진입 자산은 executor 잔고 우선 사용이 아니라 전액 flash loan 기준으로 선택된다.

N-hop 후보가 생기면 router는 V2-only 경로부터 constant-product 합성식으로 최적 투입량을 먼저 계산한다. 이때 Base 전액 flash-loan 진입에서는 Aave flash loan premium까지 marginal cost에 포함한다. 계산된 지점과 주변점만 exact quote로 검증하므로, 단순 ladder/refinement/dense search를 무조건 반복하는 방식보다 지연시간이 낮다.

같은 token pair에 V2 풀이 여러 개 있으면 slice를 반복해서 나누는 대신 각 풀의 marginal output이 같아지도록 water-filling allocation을 계산한다. V3/Curve/Balancer 또는 혼합 경로처럼 닫힌형 수식이 아직 없는 경우에는 기존 exact quote 기반 탐색으로 fallback한다. 이후 validator가 gas, USD risk limit, simulation까지 다시 통과시킨다.

**1. 현재 프로젝트에 반영된 Base 변경**
이미 수정한 파일은 다음이다.

```text
.env
.env.example
config/base.toml
scripts/check_env.sh
src/config.rs
docs/base-live-env-a-to-z.md
```

핵심 변경은 다음이다.

```text
CHAIN=base
Base 검증 대상 DEX env 확장
Base token/DEX 주소 중 온체인 검증 가능한 값 채움
Base WETH 가격 e8 값 채움
config/base.toml의 Base venue를 현재 코드 지원 범위로 지정
설정 파서가 "확인 필요", "추가 세팅 필요" 마커를 빈 값처럼 처리하도록 보강
```

**2. 최종 .env 목표값**
아래 블록을 기준으로 `.env`의 Base/운영 부분을 맞추면 된다. `<...>`는 네가 직접 발급하거나 배포 후 채워야 하는 값이다. Alchemy key, private key는 절대 채팅창에 붙여넣지 말고 로컬 `.env`에만 넣는다.

```env
# =========================
# Global
# =========================
CHAIN=base
SIMULATION_ONLY=true
STRICT_TARGET_ALLOWLIST=true
ALLOW_PUBLIC_FALLBACK=false
MAX_CONCURRENT_TX=1

OPERATOR_PRIVATE_KEY=<새로 만든 Base 운영 hot wallet private key>
DEPLOYER_PRIVATE_KEY=<새로 만든 Base 배포 wallet private key>
SAFE_OWNER=<Base Safe 주소>

# =========================
# Base RPC / protected submission
# =========================
BASE_PUBLIC_RPC_URL=https://base-mainnet.g.alchemy.com/v2/<ALCHEMY_API_KEY>
BASE_FALLBACK_RPC_URL=https://mainnet.base.org
BASE_PRECONF_RPC_URL=https://base-mainnet.g.alchemy.com/v2/<ALCHEMY_API_KEY>
BASE_WSS_URL=wss://base-mainnet.g.alchemy.com/v2/<ALCHEMY_API_KEY>

# Alchemy Base는 standard RPC에 MEV Protection이 자동 적용된다.
# 따라서 Alchemy를 쓸 때는 PUBLIC과 같은 HTTPS endpoint를 넣어도 된다.
BASE_PROTECTED_RPC_URL=https://base-mainnet.g.alchemy.com/v2/<ALCHEMY_API_KEY>
BASE_PRIVATE_SUBMIT_METHOD=eth_sendRawTransaction
BASE_SIMULATE_METHOD=eth_call

# 배포 후 채움
BASE_EXECUTOR_ADDRESS=<배포된 ArbitrageExecutor 주소>

# Aave V3 Base Pool
BASE_AAVE_POOL=0xA238Dd80C259a72e81d7e4664a9801593F98d1c5

# =========================
# Base tokens
# =========================
BASE_USDC=0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913
BASE_USDT=0xfde4C96c8593536E31F229EA8f37b2ADa2699bb2
BASE_WETH=0x4200000000000000000000000000000000000006
BASE_DAI=0x50c5725949A6F0c72E6C4a641F24049A917DB0Cb
BASE_CBETH=0x2Ae3F1Ec7F1F5012CFEab0185bfc7aa3cf0DEc22

# =========================
# Base DEX infra
# =========================
BASE_UNISWAP_V2_FACTORY=0x8909Dc15e40173Ff4699343b6eB8132c65e18eC6
BASE_SUSHISWAP_V2_FACTORY=0x71524B4f93c58fcbF659783284E38825f0622859
BASE_BASESWAP_FACTORY=0xFDa619b6d20975be80A10332cD39b9a4b0FAa8BB

# 현재 코드에서는 Aerodrome을 production venue로 켜지 않는다.
BASE_AERODROME_V2_FACTORY="추가 세팅 필요"
BASE_AERODROME_V3_FACTORY="추가 세팅 필요"
BASE_AERODROME_V3_QUOTER="추가 세팅 필요"

BASE_UNISWAP_V3_FACTORY=0x33128a8fC17869897dcE68Ed026d694621f6FDfD
BASE_UNISWAP_V3_QUOTER=0x3d4e44Eb1374240CE5F1B871ab261CD16335B76a
BASE_CURVE_REGISTRY=0x87DD13Dd25a1DBde0E1EdcF5B8Fa6cfff7eABCaD
BASE_BALANCER_VAULT=0xBA12222222228d8Ba445958a75a0704d566BF2C8

# =========================
# Base prices, USD e8
# =========================
BASE_USDC_PRICE_E8=100000000
BASE_USDT_PRICE_E8=100000000
BASE_DAI_PRICE_E8=100000000

# 실매매 전 최신값으로 갱신한다. 현재 값은 온체인 Chainlink ETH/USD 조회값 기준.
BASE_WETH_PRICE_E8=218717853053

# =========================
# Discovery / cache runtime
# =========================
POOL_FETCH_MULTICALL_CHUNK_SIZE=150
POOL_FETCH_MAX_RETRIES=5
POOL_FETCH_OTHER_MAX_RETRIES=0
POOL_FETCH_RETRY_BASE_DELAY_MS=250
TOKEN_METADATA_MULTICALL_CHUNK_SIZE=250
DISABLE_POOL_STATE_CACHE=false
DISCOVERY_FETCH_UNSEEN_POOLS=false
DISABLE_SKIPPED_POOL_CACHE=false
SKIPPED_POOL_CACHE_TTL_SECS=86400
USE_V3_RPC_QUOTER=false
SEARCH_MAX_CANDIDATES_PER_REFRESH=64
INITIAL_REFRESH_MAX_EDGES=1024
```

**3. 아직 네가 직접 채워야 하는 값**
현재 자동으로 채울 수 없거나, 네 지갑 서명이 필요한 값은 이것들이다.

```env
SAFE_OWNER=<추가 세팅 필요>
BASE_EXECUTOR_ADDRESS=<추가 세팅 필요>
```

`DEPLOYER_PRIVATE_KEY`는 로컬에서 새 배포 wallet을 생성해 `.env`에 넣을 수 있다. 이 프로젝트에서는 이미 로컬 `.env`에 생성해 넣었다. private key는 파일 밖으로 출력하지 않는다. 배포하려면 해당 public address에 Base ETH를 보내야 한다.

배포 wallet public address 확인:

```bash
source .env
cast wallet address --private-key "$DEPLOYER_PRIVATE_KEY"
```

상황에 따라 아래도 확인해야 한다.

```env
OPERATOR_PRIVATE_KEY=<이미 있더라도 Base 운영 전용 hot wallet인지 확인 필요>
BASE_WETH_PRICE_E8=<실매매 직전 최신 가격으로 갱신 필요>
BASE_MIN_NET_PROFIT_USD_E8=<전략 수익 기준 확인 필요>
BASE_MAX_POSITION_USD_E8=<초기 운용 한도 확인 필요>
BASE_MAX_FLASH_LOAN_USD_E8=<초기 flash loan 한도 확인 필요>
DAILY_LOSS_LIMIT_USD_E8=<일 손실 제한 확인 필요>
```

**4. Alchemy Base RPC 얻는 방법**
이미 Alchemy에 가입되어 있으면 다시 가입할 필요 없다. Base 앱이 없거나 endpoint를 확인하려면 다음처럼 한다.

1. https://dashboard.alchemy.com 접속.
2. 로그인.
3. `Create new app` 클릭.
4. Chain은 `Base`, Network는 `Mainnet` 선택.
5. 앱 생성 후 `API Key` 또는 `Endpoints` 클릭.
6. `HTTPS` endpoint를 복사해서 `BASE_PUBLIC_RPC_URL`에 넣는다.
7. 같은 `HTTPS` endpoint를 `BASE_PROTECTED_RPC_URL`에도 넣는다.
8. `WebSocket` endpoint를 복사해서 `BASE_WSS_URL`에 넣는다.
9. `BASE_PRECONF_RPC_URL`은 별도 preconfirm provider를 쓰지 않는 한 같은 HTTPS endpoint를 넣어도 된다.
10. 터미널에서 확인한다.

```bash
source .env
cast chain-id --rpc-url "$BASE_PUBLIC_RPC_URL"
```

정상 결과는 `8453`이다.

**5. Base MEV Protection은 별도 access 요청이 필요한가**
Alchemy Base를 쓰는 경우에는 별도 access 요청이 필요 없다. Alchemy 문서 기준으로 MEV Protection은 Base 지원 네트워크에 포함되어 있고, standard RPC endpoint로 트랜잭션을 보내면 자동 적용된다.

실무적으로는 이렇게 이해하면 된다.

```text
Polygon: Alchemy standard RPC만으로는 private mempool이 아니어서 별도 private submission endpoint가 필요
Base: Alchemy standard RPC 자체가 MEV Protection 대상이라 별도 요청 없이 사용 가능
```

비용은 다음처럼 보면 된다.

```text
MEV Protection 기능 자체: Alchemy 문서상 추가 비용 없음
RPC 사용량/요금제: 기존 Alchemy plan, request/compute 사용량은 그대로 적용
Base gas: 모든 실제 트랜잭션에는 ETH gas 비용 발생
Aave flash loan: flash loan premium 발생
```

Aave Base flash loan premium은 아래처럼 직접 확인한다.

```bash
source .env
cast call "$BASE_AAVE_POOL" "FLASHLOAN_PREMIUM_TOTAL()(uint128)" \
  --rpc-url "$BASE_PUBLIC_RPC_URL"
```

현재 조회값은 `5`였다. Aave에서는 bps 단위로 해석하므로 0.05%로 보면 된다. 실매매 직전 다시 조회한다.

**6. 운영 wallet 만들기**
운영 wallet은 bot이 트랜잭션을 서명하는 hot wallet이다. 여기에 큰 자금을 넣지 않는다. Base gas용 ETH만 소량 넣는다.

터미널에서 실행한다.

```bash
cast wallet new
```

출력에서 private key를 `.env`에 넣는다.

```env
OPERATOR_PRIVATE_KEY=0x...
```

주소도 확인한다.

```bash
source .env
OPERATOR_ADDRESS=$(cast wallet address --private-key "$OPERATOR_PRIVATE_KEY")
echo "$OPERATOR_ADDRESS"
```

이 주소로 Base ETH를 소량 보낸다. 처음에는 배포/실매매 테스트 전송용으로 아주 작은 금액만 둔다.

```bash
cast balance "$OPERATOR_ADDRESS" --rpc-url "$BASE_PUBLIC_RPC_URL"
```

**7. 배포 wallet 만들기**
배포 wallet은 `ArbitrageExecutor`를 배포할 때만 쓴다. 운영 wallet과 분리한다.

```bash
cast wallet new
```

private key를 `.env`에 넣는다.

```env
DEPLOYER_PRIVATE_KEY=0x...
```

주소 확인:

```bash
source .env
DEPLOYER_ADDRESS=$(cast wallet address --private-key "$DEPLOYER_PRIVATE_KEY")
echo "$DEPLOYER_ADDRESS"
cast balance "$DEPLOYER_ADDRESS" --rpc-url "$BASE_PUBLIC_RPC_URL"
```

이 주소에도 Base ETH를 소량 넣는다. 배포 gas가 부족하면 `forge script --broadcast` 단계에서 실패한다.

**8. Base Safe 만들기**
`SAFE_OWNER`는 executor의 owner가 될 주소다. 개인 EOA 하나보다 Safe를 쓰는 게 맞다.

1. https://app.safe.global 접속.
2. `Create new Account` 클릭.
3. Network를 `Base`로 선택.
4. Safe 이름 입력.
5. Owner를 최소 2개 넣는다. 예: 하드웨어 wallet 1개, 백업 wallet 1개.
6. Threshold는 처음에는 `1/2`도 가능하지만, 자금이 커지면 `2/2` 또는 더 보수적으로 설정한다.
7. 생성 트랜잭션 서명.
8. 생성된 Safe 주소를 복사해서 `.env`에 넣는다.

```env
SAFE_OWNER=0x...
```

검증:

```bash
source .env
cast code "$SAFE_OWNER" --rpc-url "$BASE_PUBLIC_RPC_URL"
cast call "$SAFE_OWNER" "getOwners()(address[])" --rpc-url "$BASE_PUBLIC_RPC_URL"
cast call "$SAFE_OWNER" "getThreshold()(uint256)" --rpc-url "$BASE_PUBLIC_RPC_URL"
```

`cast code` 결과가 `0x`면 Safe 주소가 아니거나 체인을 잘못 선택한 것이다.

**9. executor 배포**
배포 전 `.env`에 최소한 아래가 있어야 한다.

```env
CHAIN=base
DEPLOYER_PRIVATE_KEY=0x...
SAFE_OWNER=0x...
BASE_PUBLIC_RPC_URL=https://base-mainnet.g.alchemy.com/v2/<ALCHEMY_API_KEY>
BASE_AAVE_POOL=0xA238Dd80C259a72e81d7e4664a9801593F98d1c5
```

배포 전 점검:

```bash
source .env
./scripts/check_env.sh base deploy
forge build
```

배포:

```bash
./scripts/deploy_base.sh
```

출력에 배포된 contract 주소가 나온다. 그 값을 `.env`에 넣는다.

```env
BASE_EXECUTOR_ADDRESS=0x...
```

검증:

```bash
source .env
cast code "$BASE_EXECUTOR_ADDRESS" --rpc-url "$BASE_PUBLIC_RPC_URL"
cast call "$BASE_EXECUTOR_ADDRESS" "aavePool()(address)" --rpc-url "$BASE_PUBLIC_RPC_URL"
cast call "$BASE_EXECUTOR_ADDRESS" "owner()(address)" --rpc-url "$BASE_PUBLIC_RPC_URL"
cast call "$BASE_EXECUTOR_ADDRESS" "paused()(bool)" --rpc-url "$BASE_PUBLIC_RPC_URL"
```

정상 조건:

```text
cast code 결과가 0x가 아님
aavePool() = 0xA238Dd80C259a72e81d7e4664a9801593F98d1c5
owner() = SAFE_OWNER
paused() = false
```

**10. operator 등록**
executor owner는 Safe다. bot hot wallet이 executor를 호출하려면 operator로 등록해야 한다.

먼저 operator 주소를 확인한다.

```bash
source .env
OPERATOR_ADDRESS=$(cast wallet address --private-key "$OPERATOR_PRIVATE_KEY")
echo "$OPERATOR_ADDRESS"
```

Safe UI에서 transaction builder를 사용한다.

```text
to: BASE_EXECUTOR_ADDRESS
function: setOperator(address operator, bool allowed)
operator: OPERATOR_ADDRESS
allowed: true
```

Safe에서 서명/실행 후 확인한다.

```bash
cast call "$BASE_EXECUTOR_ADDRESS" "operators(address)(bool)" "$OPERATOR_ADDRESS" \
  --rpc-url "$BASE_PUBLIC_RPC_URL"
```

결과가 `true`여야 한다.

만약 처음부터 `SAFE_OWNER`를 EOA로 설정했다면 터미널에서 직접 보낼 수 있다. 단, 운영에서는 Safe를 권장한다.

```bash
cast send "$BASE_EXECUTOR_ADDRESS" "setOperator(address,bool)" "$OPERATOR_ADDRESS" true \
  --private-key "$DEPLOYER_PRIVATE_KEY" \
  --rpc-url "$BASE_PUBLIC_RPC_URL"
```

**11. target allowlist 설정**
현재 `.env`는 `STRICT_TARGET_ALLOWLIST=true`다. 이 설정은 executor가 허용된 pool/vault로만 swap을 실행하게 막는다. 실매매에서는 필요한 안전장치다.

문제는 arbitrage target이 고정 DEX factory 하나가 아니라 실제 pool/pair/vault 주소라는 점이다.

```text
Uniswap V2 / SushiSwap V2 / BaseSwap V2: pair 주소
Uniswap V3: pool 주소
Curve: pool 주소
Balancer: 보통 Vault 또는 pool 관련 target
```

따라서 순서는 이렇게 한다.

1. `SIMULATION_ONLY=true` 상태로 discovery/simulation을 먼저 돌린다.
2. 실제 candidate에 등장하는 target 주소 목록을 추출한다.
3. Safe에서 `setAllowedTargets(address[] targets, bool allowed)`를 실행한다.
4. allowlist가 충분히 채워졌을 때만 live로 전환한다.

Safe transaction builder 입력:

```text
to: BASE_EXECUTOR_ADDRESS
function: setAllowedTargets(address[] targets, bool allowed)
targets: [0xpool1, 0xpool2, 0xpool3, ...]
allowed: true
```

검증:

```bash
cast call "$BASE_EXECUTOR_ADDRESS" "allowedTargets(address)(bool)" 0xPOOL_ADDRESS \
  --rpc-url "$BASE_PUBLIC_RPC_URL"
```

초기 canary에서 allowlist 때문에 실행이 전부 막히면 `STRICT_TARGET_ALLOWLIST=false`로 풀고 테스트하고 싶을 수 있다. 하지만 실매매에서는 권장하지 않는다. 최소 금액 canary에서만 짧게 사용하고, 바로 target allowlist 방식으로 되돌린다.

**12. Base Aave flash loan 검증**
Aave Pool 주소 검증:

```bash
source .env
cast code "$BASE_AAVE_POOL" --rpc-url "$BASE_PUBLIC_RPC_URL"
```

Base Aave reserve 목록 확인:

```bash
cast call "$BASE_AAVE_POOL" "getReservesList()(address[])" \
  --rpc-url "$BASE_PUBLIC_RPC_URL"
```

USDC/WETH/cbETH flash loan 가능 여부 확인:

```bash
for addr in "$BASE_USDC" "$BASE_WETH" "$BASE_CBETH"; do
  raw=$(cast call "$BASE_AAVE_POOL" "getConfiguration(address)((uint256))" "$addr" \
    --rpc-url "$BASE_PUBLIC_RPC_URL" | sed -E 's/[^0-9]*([0-9]+).*/\1/')
  node -e 'const x=BigInt(process.argv[1]); const b=n=>((x>>BigInt(n))&1n)===1n; console.log({active:b(56), frozen:b(57), paused:b(60), flash:b(63)})' "$raw"
done
```

정상 조건:

```text
active=true
frozen=false
paused=false
flash=true
```

현재 조회에서 USDC, WETH, cbETH는 위 조건을 만족했다.

**13. Base DEX 주소 검증**
Base 전체를 검증한다는 말은 “현재 코드가 지원하는 Base DEX venue 전체”를 검증한다는 뜻이다. 아래 값들은 현재 `.env`에 채웠다.

```env
BASE_UNISWAP_V2_FACTORY=0x8909Dc15e40173Ff4699343b6eB8132c65e18eC6
BASE_SUSHISWAP_V2_FACTORY=0x71524B4f93c58fcbF659783284E38825f0622859
BASE_BASESWAP_FACTORY=0xFDa619b6d20975be80A10332cD39b9a4b0FAa8BB
BASE_UNISWAP_V3_FACTORY=0x33128a8fC17869897dcE68Ed026d694621f6FDfD
BASE_UNISWAP_V3_QUOTER=0x3d4e44Eb1374240CE5F1B871ab261CD16335B76a
BASE_CURVE_REGISTRY=0x87DD13Dd25a1DBde0E1EdcF5B8Fa6cfff7eABCaD
BASE_BALANCER_VAULT=0xBA12222222228d8Ba445958a75a0704d566BF2C8
```

검증:

```bash
source .env
cast code "$BASE_UNISWAP_V2_FACTORY" --rpc-url "$BASE_PUBLIC_RPC_URL"
cast call "$BASE_UNISWAP_V2_FACTORY" "allPairsLength()(uint256)" --rpc-url "$BASE_PUBLIC_RPC_URL"

cast code "$BASE_SUSHISWAP_V2_FACTORY" --rpc-url "$BASE_PUBLIC_RPC_URL"
cast call "$BASE_SUSHISWAP_V2_FACTORY" "allPairsLength()(uint256)" --rpc-url "$BASE_PUBLIC_RPC_URL"

cast code "$BASE_BASESWAP_FACTORY" --rpc-url "$BASE_PUBLIC_RPC_URL"
cast call "$BASE_BASESWAP_FACTORY" "allPairsLength()(uint256)" --rpc-url "$BASE_PUBLIC_RPC_URL"

cast code "$BASE_UNISWAP_V3_FACTORY" --rpc-url "$BASE_PUBLIC_RPC_URL"
cast call "$BASE_UNISWAP_V3_FACTORY" "getPool(address,address,uint24)(address)" \
  "$BASE_USDC" "$BASE_WETH" 500 \
  --rpc-url "$BASE_PUBLIC_RPC_URL"

cast code "$BASE_UNISWAP_V3_QUOTER" --rpc-url "$BASE_PUBLIC_RPC_URL"

cast code "$BASE_CURVE_REGISTRY" --rpc-url "$BASE_PUBLIC_RPC_URL"
cast call "$BASE_CURVE_REGISTRY" "pool_count()(uint256)" --rpc-url "$BASE_PUBLIC_RPC_URL"

cast code "$BASE_BALANCER_VAULT" --rpc-url "$BASE_PUBLIC_RPC_URL"
cast call "$BASE_BALANCER_VAULT" "getAuthorizer()(address)" --rpc-url "$BASE_PUBLIC_RPC_URL"
```

현재 온체인 검증에서 확인된 값:

```text
Uniswap V2 allPairsLength: 2,978,359
SushiSwap V2 allPairsLength: 5,981
BaseSwap V2 allPairsLength: 8,118
Curve pool_count: 731
Balancer Vault getAuthorizer(): 0xA69E0Ccf150a29369D8Bbc0B3f510849dB7E8EEE
Uniswap V3 USDC/WETH 0.05% pool: 0xd0b53D9277642d899DF5C87A3966A349A798F224
```

**14. config/base.toml 범위**
현재 Base config의 policy는 이렇게 맞췄다.

```toml
[policy]
venues = ["uniswap_v2", "sushiswap_v2", "baseswap_v2", "uniswap_v3", "curve", "balancer"]
symbols = []
```

의미는 다음이다.

```text
venues: 현재 코드가 지원하고 env를 채운 Base venue만 사용
symbols: 빈 배열이므로 특정 심볼만으로 제한하지 않음
```

Base 설정의 configured token 중 `USDC`, `USDT`는 아래처럼 둔다.

```toml
[[tokens]]
symbol = "USDC"
address_env = "BASE_USDC"
decimals = 6
is_stable = true
is_cycle_anchor = false
allow_self_funded = false
price_env = "BASE_USDC_PRICE_E8"

[[tokens]]
symbol = "USDT"
address_env = "BASE_USDT"
decimals = 6
is_stable = true
is_cycle_anchor = false
allow_self_funded = false
price_env = "BASE_USDT_PRICE_E8"
```

이렇게 해야 stable token이라는 이유만으로 start/end가 되지 않고, Aave flash reserve로 확인된 자산만 cycle anchor가 된다.

각 DEX의 `start_block`도 0이 아니라 온체인 코드 생성 확인 블록으로 맞췄다.

```text
Balancer Vault: 1196036
Uniswap V3 Factory: 1371680
BaseSwap V2 Factory: 2059124
Aave Pool: 2357134
SushiSwap V2 Factory: 2631214
Uniswap V2 Factory: 6601915
Curve MetaRegistry: 14591073
```

Aave Pool은 DEX discovery 대상이 아니라 executor/flash loan 대상이라 `config/base.toml`의 DEX 목록에는 들어가지 않는다.

**15. 가격값 갱신**
Stablecoin은 기본적으로 e8 기준 1달러를 사용한다.

```env
BASE_USDC_PRICE_E8=100000000
BASE_USDT_PRICE_E8=100000000
BASE_DAI_PRICE_E8=100000000
```

WETH 가격은 실매매 전에 최신값으로 갱신한다. Base Chainlink ETH/USD feed로 확인한다.

```bash
source .env
cast call 0x71041dddad3595F9CEd3DcCFBe3D1F4b0a16Bb70 \
  "latestRoundData()(uint80,int256,uint256,uint256,uint80)" \
  --rpc-url "$BASE_PUBLIC_RPC_URL"
```

두 번째 값이 answer다. e8 값 그대로 `BASE_WETH_PRICE_E8`에 넣는다.

```env
BASE_WETH_PRICE_E8=<answer 값>
```

현재 조회값은 `218717853053`이었다. 실매매 직전 다시 갱신한다.

**16. risk limit 초기값**
처음 실매매를 바로 큰 금액으로 시작하면 안 된다. 최소 canary는 이렇게 시작한다.

```env
SIMULATION_ONLY=true
ALLOW_PUBLIC_FALLBACK=false
MAX_CONCURRENT_TX=1

BASE_MIN_NET_PROFIT_USD_E8=10000000
BASE_MIN_TRADE_USD_E8=1000000000
BASE_MAX_POSITION_USD_E8=10000000000
BASE_MAX_FLASH_LOAN_USD_E8=10000000000
DAILY_LOSS_LIMIT_USD_E8=5000000000
```

e8 단위 해석:

```text
100000000 = $1
10000000 = $0.10
1000000000 = $10
10000000000 = $100
5000000000 = $50
```

초기에는 max position/flash loan을 $100 수준으로 제한하고, 로그와 실패 원인을 확인한 뒤 점진적으로 올린다.

**17. 실행 전 검증 순서**
아래 순서 그대로 한다.

```bash
source .env
./scripts/check_env.sh base verify
```

이건 Base RPC, Aave, token, DEX, WETH 가격까지 채워졌는지 확인한다.

배포 전:

```bash
./scripts/check_env.sh base deploy
forge build
forge test
```

배포 후:

```bash
./scripts/check_env.sh base run
```

live 제출 전:

```bash
./scripts/check_env.sh base live
```

`base live`에서 `BASE_PROTECTED_RPC_URL`이 요구된다. Alchemy Base를 쓰면 `BASE_PUBLIC_RPC_URL`과 같은 endpoint를 넣으면 된다.

**18. simulate-only dry run**
처음에는 반드시 simulation-only로 돌린다.

```bash
SIMULATION_ONLY=true cargo run --release -- --chain base --once --simulate-only
```

또는 스크립트를 쓴다.

```bash
SIMULATION_ONLY=true ./scripts/run_base.sh --once --simulate-only
```

이 단계에서 봐야 하는 것:

```text
config parse 성공
RPC chain id 정상
pool discovery가 과도하게 느리지 않은지
candidate가 생기는지
gas/price valuation이 정상인지
executor address가 설정된 상태에서 제출 없이 simulation만 수행되는지
```

**19. live 전환**
아래 조건이 모두 만족되기 전에는 live로 전환하지 않는다.

```text
Base Safe 생성 완료
ArbitrageExecutor 배포 완료
BASE_EXECUTOR_ADDRESS 검증 완료
operator 등록 완료
target allowlist 등록 완료
BASE_PROTECTED_RPC_URL 설정 완료
ALLOW_PUBLIC_FALLBACK=false 유지
작은 risk limit 설정 완료
SIMULATION_ONLY=true 상태에서 dry run 정상
```

그 다음 `.env`를 바꾼다.

```env
SIMULATION_ONLY=false
```

그리고 run:

```bash
source .env
./scripts/check_env.sh base live
./scripts/run_base.sh
```

처음 live는 반드시 아주 작은 한도로 한다.

**20. 장애 시 즉시 멈추는 방법**
가장 빠른 정지는 executor pause다. Safe에서 아래를 실행한다.

```text
to: BASE_EXECUTOR_ADDRESS
function: setPaused(bool enabled)
enabled: true
```

검증:

```bash
cast call "$BASE_EXECUTOR_ADDRESS" "paused()(bool)" --rpc-url "$BASE_PUBLIC_RPC_URL"
```

결과가 `true`면 executor 실행이 막힌다.

bot 프로세스도 중지한다.

```bash
pkill -f dex-arbitrage
```

필요하면 operator 권한도 끈다.

```text
to: BASE_EXECUTOR_ADDRESS
function: setOperator(address operator, bool allowed)
operator: OPERATOR_ADDRESS
allowed: false
```

**21. 최종 체크리스트**
아래를 전부 통과한 뒤에만 Base 실매매로 간다.

```text
[ ] CHAIN=base
[ ] BASE_PUBLIC_RPC_URL chain-id 8453 확인
[ ] BASE_PROTECTED_RPC_URL 설정
[ ] BASE_AAVE_POOL code 확인
[ ] BASE_USDC/WETH/cbETH Aave flash=true 확인
[ ] Base DEX factory/registry/vault code 확인
[ ] BASE_WETH_PRICE_E8 최신값 갱신
[ ] DEPLOYER_PRIVATE_KEY 새 wallet으로 설정
[ ] SAFE_OWNER Base Safe로 설정
[ ] executor 배포
[ ] BASE_EXECUTOR_ADDRESS code/aavePool/owner/paused 확인
[ ] operator 등록
[ ] target allowlist 등록
[ ] SIMULATION_ONLY=true dry run 정상
[ ] risk limit $100 수준 canary로 제한
[ ] SIMULATION_ONLY=false 전환
[ ] live 후 첫 tx는 직접 basescan에서 확인
```

**22. 공식/검증 출처**
- Alchemy MEV Protection: https://www.alchemy.com/docs/reference/mev-protection
- Alchemy Base API quickstart: https://www.alchemy.com/docs/reference/base-api-quickstart
- Base ecosystem contracts, Uniswap V2/V3: https://docs.base.org/base-chain/network-information/ecosystem-contracts
- Aave V3 Base address book: https://github.com/aave-dao/aave-address-book/blob/main/src/AaveV3Base.sol
- Aave V3 Pool docs: https://aave.com/docs/aave-v3/smart-contracts/pool
- Curve deployed contracts: https://docs.curve.finance/references/deployed-contracts/
- Balancer deployments package: https://github.com/balancer/balancer-deployments
- Base RPC overview and public endpoints: https://docs.base.org/base-chain/api-reference/rpc-overview
