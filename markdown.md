# 외부 검토 요청용 문제 정리: Base와 Avalanche에서 실제 실매매 후보를 찾지 못한 이유

작성일: 2026-04-21

## 0. 문서 작성 목적

이 문서는 `dex-arbitrage` 프로젝트를 다른 AI 또는 외부 전문가에게 검토시키기 위한 자료입니다.

사용자는 현재 저의 판단과 설계를 신뢰하지 못하는 상태입니다. 이유는 제가 여러 번 “후보가 있다”, “개선됐다”, “다음은 이것이다”라고 말했지만, 실제로는 **Base와 Avalanche 모두에서 fork/executor simulation 기준 실매매 가능한 후보를 찾지 못했기 때문**입니다.

따라서 이 문서는 변명이나 요약 보고서가 아니라, 다음 검토자가 바로 판단할 수 있도록 다음을 사실 위주로 정리합니다.

```text
1. 프로젝트의 목표
2. 현재까지 변경한 코드/설정
3. Base에서 시도한 것과 실패한 이유
4. Avalanche에서 시도한 것과 실패한 이유
5. 제가 반복해서 잘못 접근한 부분
6. 현재 남은 핵심 문제
7. 다음 검토자가 판단해야 할 질문
```

핵심 문제는 하나입니다.

```text
Base와 Avalanche 모두에서 실제 실매매 가능한 후보를 아직 찾지 못했다.
```

여기서 “실제 실매매 가능한 후보”는 아래를 모두 만족하는 후보를 의미합니다.

```text
1. route가 생성됨
2. AMM별 exact quote 기준으로 output이 양수
3. flash loan premium 포함 후 이익
4. gas 비용 포함 후 이익
5. executor calldata 생성 가능
6. fork 또는 RPC simulation 통과
7. 실제 토큰 transfer/revert 문제 없음
```

단순히 그래프상 가격 차이가 있어 보이는 “탐색 후보”는 실매매 후보가 아닙니다.

## 1. 프로젝트 목표

프로젝트 목표는 EVM 체인에서 DEX 간 N-hop cyclic arbitrage를 찾아 Aave flash loan 기반으로 실행하는 것입니다.

주요 구성은 다음과 같습니다.

```text
언어: Rust + Solidity
실행 컨트랙트: contracts/ArbitrageExecutor.sol
체인: 처음 Base 중심, 이후 Avalanche 중심으로 전환
자금: Base ETH 일부를 AVAX로 변환하여 Avalanche deployer/operator에 분배 완료
flash loan: Aave V3
초기 목표: 실매매 전 simulate-only/fork 검증으로 실제 후보 확보
```

현재 지원하거나 일부 지원하도록 추가한 AMM/DEX 계열:

```text
Uniswap V2-like
Uniswap V3-like
Aerodrome V2-like
Aerodrome/Slipstream CL
Curve
Balancer
Trader Joe Liquidity Book 일부
```

## 2. 현재 저장소 상태 요약

최근 주요 변경:

```text
Base Flashblocks / pending_logs 실험
QuickNode vs Alchemy 비교 스크립트
Avalanche, Sonic, Gnosis, Celo, Linea, BSC config 추가
Avalanche 중심 전환
Trader Joe LB discovery / fetch / execution 일부 추가
targeted replay 스크립트 추가
```

주요 추가 파일:

```text
config/avalanche.toml
config/bsc.toml
config/celo.toml
config/gnosis.toml
config/linea.toml
config/sonic.toml
scripts/scout_chains.py
scripts/replay_avalanche_route.py
scripts/run_avalanche.sh
scripts/deploy_avalanche.sh
docs/avalanche-live-setup.md
docs/base-provider-compare.md
src/amm/trader_joe_lb.rs
contracts/interfaces/ITraderJoeLBPair.sol
```

현재 Avalanche 자금 상태:

```text
Avalanche operator: 약 10.533675269 AVAX
Avalanche deployer: 4.5 AVAX
Base operator: 약 0.006306488 ETH
Base deployer: 약 0.005966672 ETH
```

자금 이동:

```text
Base -> Avalanche bridge/swap:
0xf52525a6a723a66c8c4abaa9a6bfb581fe19fc2fe9046e016e149f2e2604196f

Avalanche operator -> deployer transfer:
0x67c008be8353aaa7b7fd340543b2663e8d15e515f4ca53d7526b43a08a3a5f25
```

주의:

```text
AVALANCHE_EXECUTOR_ADDRESS는 아직 mainnet 배포하지 않는 것이 맞음.
이유: fork 기준 실매매 후보가 아직 없고, executor 재수정 가능성이 높음.
```

## 3. Base에서 시도한 것

### 3.1 Base 기본 세팅

Base에서 설정한 주요 범위:

```text
Uniswap V2
SushiSwap V2
BaseSwap V2
Aerodrome V2
Uniswap V3
Aerodrome Slipstream/V3
Curve
Balancer
Aave V3 flash loan
```

Base에 대해 다음을 진행했습니다.

```text
1. Base config 정리
2. Aave Pool 확인
3. executor 배포
4. Safe owner/operator 설정
5. Base simulate-only 실행
6. Alchemy 비용/요청량 관찰
7. QuickNode Base endpoint 추가
8. Base Flashblocks / pending block / eth_simulateV1 확인
9. event ingest를 pending_logs 방향으로 변경
10. candidate verification 전처리 추가
```

### 3.2 Base에서 나온 주요 결과

Base에서 보수적인 trusted-token 설정을 사용하면 후보가 거의 나오지 않았습니다.

```text
candidate_count=0
```

토큰 제한을 풀고 전체 범위에 가깝게 보면 후보는 생겼습니다. 하지만 다음 단계에서 전부 사라졌습니다.

대표 패턴:

```text
탐색 후보 있음
route 후보 일부 있음
exact quote / validator / fork 기준 실매매 후보 없음
```

Base에서 여러 로그와 실험을 통해 확인한 문제:

```text
1. V3/Slipstream 경로가 많음
2. V3 fallback quote가 실제 Quoter/fork와 불일치
3. 롱테일 토큰 기반 spot 차익이 많음
4. 후보 top-k가 실제 수익 기준이 아니라 spot/heuristic 영향을 많이 받음
5. Base는 경쟁이 강하고 지연시간에 민감함
6. public RPC/일반 pending_logs 수준으로는 실질 알파를 잡기 어려움
```

### 3.3 Base에서 QuickNode를 붙인 이유

Alchemy가 “이미 일어난 것만 보는 서비스”라는 의미는 아니었습니다. 하지만 Base에서 Flashblocks/preconfirm 경로가 중요하므로 QuickNode를 비교했습니다.

QuickNode endpoint 확인:

```text
chain-id=8453
pending block 조회 정상
```

Provider 비교 결과:

```text
Alchemy:
  bootstrap_s 약 30초
  snapshots 12
  candidates 0

QuickNode Free:
  bootstrap_s 약 18초
  snapshots 20
  candidates 0
```

결론:

```text
QuickNode가 일부 더 빠른 신호를 줄 수는 있음.
하지만 후보 0 문제의 본질은 provider가 아니라 탐색/quote/execution 모델 문제였음.
```

### 3.4 Base에서 제가 잘못한 점

제가 잘못한 접근:

```text
1. 후보가 안 나오자 후보 수/beam/top-k 같은 수치를 조정함
2. trusted token 제한으로 false positive를 줄이려 함
3. candidate_count=0을 단순히 시장 문제로 설명함
4. candidate_count가 생기면 후보가 있다고 표현함
5. 실제 fork/executor simulation 기준 실매매 후보와 탐색 후보를 혼동함
```

실제 문제는 다음이었습니다.

```text
탐색 모델과 실행 모델이 일치하지 않았다.
```

Base에서의 결론:

```text
Base는 추가 인프라 없이 진행하기 어렵다.
정확한 V3/Slipstream local simulator와 raw Flashblocks급 ingest가 없으면 경쟁력이 부족하다.
```

## 4. Avalanche로 옮긴 이유

Base가 너무 경쟁적이고 후보가 계속 사라졌기 때문에 다른 체인을 검토했습니다.

비교한 체인:

```text
Avalanche
Sonic
Gnosis
Celo
Linea
BSC
```

초기 scout 결과:

```text
Avalanche:
  탐색 후보 있음

Sonic:
  탐색 후보 0

Gnosis:
  탐색 후보 0

Celo:
  탐색 후보 0

Linea:
  탐색 후보 0 또는 DEX 범위 부족

BSC:
  탐색 후보는 있으나 route 통과 0
```

그래서 Avalanche를 1순위로 선택했습니다.

## 5. Avalanche에서 시도한 것

### 5.1 Avalanche V2 계열 추가

추가한 DEX:

```text
Uniswap V2 on Avalanche
Pangolin V2
Trader Joe V1
```

추가한 토큰:

```text
USDC
USDT
WAVAX
BTCb
EURC
```

초기 결과:

```text
pool_count 약 56,427
changed_edges 약 108,869
candidate_count 149
```

좀 더 넓게 보면:

```text
candidate_count 1463
routed 일부 발생
더미 executor 기준 simulated처럼 보이는 후보 발생
```

하지만 fork executor를 실제로 배포해서 확인하자:

```text
simulated=0
대표 실패: Joe: TRANSFER_FAILED
```

### 5.2 Avalanche V2 후보의 문제

실패 route를 분해하니 중간에 이런 토큰이 있었습니다.

```text
"TEST TOKEN"
```

즉, 후보처럼 보였던 것 중 일부는 명백한 허수였습니다.

문제:

```text
V2 reserve 기반 spot 계산은 롱테일/테스트 토큰에서 허수 차익을 쉽게 생성함.
실제 executor fork에서 transfer 실패 또는 수익성 소멸 발생.
```

### 5.3 Trader Joe LB 추가

Avalanche 핵심 유동성이 Trader Joe Liquidity Book 쪽에 있다고 판단해 LB 지원을 추가했습니다.

추가한 것:

```text
TraderJoeLb AMM kind
TraderJoeLbConfiguredPairs
TraderJoeLbFactoryAllPairs
LB pair discovery
LB pool state fetch
LB batch fetch
LB direct pair execution in ArbitrageExecutor
LB getSwapOut exact quote
LB activeId 저장
LB active bin 기반 edge price
LB targeted replay
```

관련 인터페이스:

```text
ITraderJoeLBFactory
  getNumberOfLBPairs()
  getLBPairAtIndex()
  getLBPairInformation()

ITraderJoeLBPair
  getTokenX()
  getTokenY()
  getBinStep()
  getActiveId()
  getReserves()
  getSwapOut()
  swap()
```

### 5.4 LB 전체 pool discovery 결과

Trader Joe LB factory 전체:

```text
V2.2 factory pools: 1282
V2.1 factory pools: 2072
총 factory pools: 3354
live pools fetched: 825
edges: 1463
```

초기 구현에서는 per-pool fetch 때문에 15분 timeout이 났습니다.

이후 batch fetch를 구현했습니다.

개선 결과:

```text
3354 LB pools fetch
825 live pools
약 23초
```

### 5.5 LB 전체 scout 결과

LB 전체 universe 기준:

```text
candidate_count 620~1275
routed 5~10
더미 executor 기준 simulated 4~8
```

하지만 실제 fork executor 기준:

```text
simulated=0
```

### 5.6 LB 후보에 섞인 롱테일 토큰

LB 전체 후보에서 반복 등장한 토큰:

```text
HUIAL
TOPIA
PTOP600
PTOP700
AUSD
```

예:

```text
HUIAL: 0x6dE6a962C52484B5533C6C11A2217769Ea36830f
TOPIA: 0xDf50aD73b92C758bBF94869b4B7b9128bBe4a475
PTOP700: 0x46b03ddaE1dd0D0Dd049c1A9622c8FD0ad947929
PTOP600: 0xe9cbB999B78A01C63499e4778d785Fba52AdEf18
AUSD: 0x00000000eFE302BEAA2b3e6e1b18d08D69a9012a
```

제가 한때 이런 토큰을 제외하려 했지만, 사용자가 지적한 것처럼 “토큰 제한으로 해결”하면 안 됩니다.

올바른 방향:

```text
토큰을 제한하지 않고 전체 토큰을 본다.
그 대신 실제 quote/execution/fork 결과로 선별해야 한다.
```

## 6. BSC에서 시도한 것

BSC도 추가했습니다.

지원한 DEX:

```text
ApeSwap V2
BiSwap V2
```

PancakeSwap V2는 pair가 250만 개 이상이라 현재 방식으로 전체 스캔하기 어렵다고 판단해 제외했습니다.

BSC 결과:

```text
pools=9338
edges=16272
candidate_count=801
routed=0
simulated=0
```

즉 BSC도 현재 방식으로는 실제 후보가 나오지 않았습니다.

## 7. 현재 핵심 문제

Base와 Avalanche 모두 같은 패턴이 반복됩니다.

```text
탐색 후보는 만들 수 있음
route 후보도 일부 만들 수 있음
하지만 exact/fork/executor simulation에서 실매매 후보가 0
```

핵심 문제는 다음입니다.

```text
후보 생성 단계의 가격/수익 모델이 실제 실행 결과와 다르다.
```

세부적으로는:

```text
1. V2 reserve 기반 후보는 롱테일에서 허수가 많음
2. V3/LB는 AMM 구조가 복잡해서 단순 spot/active bin 가격만으로 부족함
3. getSwapOut exact quote를 쓰면 후보 대부분이 사라짐
4. fork executor까지 가면 추가로 transfer/revert/minProfit 문제가 발생함
5. 전체 후보를 먼저 많이 만든 뒤 나중에 제거하는 구조라 비용이 큼
```

## 8. 제가 반복해서 잘못한 것

### 8.1 후보 수를 성과처럼 해석함

`candidate_count`가 늘어난 것을 진전처럼 말했습니다.

하지만 실제로는 다음이 중요했습니다.

```text
candidate_count보다 simulated/fork-passed count가 중요함.
```

실제 실매매 후보는 아직 0입니다.

### 8.2 수치 조정에 의존함

다음을 반복해서 바꿨습니다.

```text
beam width
top-k
candidate limit
trusted token
min trade
timeout
route verify limit
```

하지만 구조 문제는 해결되지 않았습니다.

### 8.3 토큰 제한으로 문제를 숨기려 함

허수 토큰이 많아지자 trusted token을 제한하려 했습니다.

하지만 사용자의 요구는 명확합니다.

```text
전체 토큰을 허용하고,
실제 실행 가능성으로 후보를 선별해야 한다.
```

### 8.4 외부 자료 기반 설계를 충분히 하지 않음

Trader Joe LB는 bin 기반 AMM입니다.

그런데 초기에 V2식 reserve/spot 사고방식으로 접근했습니다.

이는 잘못입니다.

### 8.5 전체 스캔 반복으로 시간을 낭비함

전체 pool을 매번 훑으면서 결과를 봤습니다.

이제는 targeted replay가 필요합니다.

## 9. 현재까지 만든 targeted replay 도구

추가한 것:

```text
INITIAL_REFRESH_POOL_IDS
pool_route 로그 출력
scripts/replay_avalanche_route.py
```

사용법:

```bash
python3 scripts/replay_avalanche_route.py \
  --from-log state/chain-scout/<pool_route가 있는 로그>.log \
  --index 0
```

또는:

```bash
python3 scripts/replay_avalanche_route.py \
  --pool-route 0xPoolA,0xPoolB,0xPoolC
```

이 도구의 목적:

```text
전체 스캔 반복 금지
특정 route만 빠르게 fork에서 재현
각 route가 왜 죽는지 빠르게 확인
```

## 10. 다음 검토자가 봐야 할 질문

다음 검토자는 아래 질문을 우선 검토해야 합니다.

### 질문 1. 현재 LB local model이 맞는가?

현재 구현:

```text
activeId 기반 spot rate
getSwapOut 기반 exact quote
bounded direct search
```

하지만 여전히 exact/fork에서 수익 후보가 사라집니다.

검토할 것:

```text
active bin 가격 계산이 tokenX/tokenY decimals를 제대로 반영하는가?
LB getSwapOut 결과를 candidate scoring에 충분히 반영하고 있는가?
route generation 단계에서 active bin 가격만으로 허수 cycle을 만들고 있지는 않은가?
```

### 질문 2. 전체 토큰 허용 상태에서 어떻게 선별해야 하는가?

사용자 요구:

```text
토큰 제한 금지
전체 토큰 허용
실제 후보를 제대로 선별
```

검토할 것:

```text
token symbol allowlist가 아니라 token behavior/risk/execution 결과 기반 필터 설계
fee-on-transfer/rebase/blacklist/honeypot/callback token 탐지
transfer simulation 선검사
pool liquidity/depth 기반 후보 점수
```

### 질문 3. 후보 생성 단계에서 fork/exact 정보를 어떻게 싸게 반영할 것인가?

지금은 다음 구조입니다.

```text
그래프 기반 후보 생성
route search
exact verification
fork simulation
대부분 탈락
```

검토할 방향:

```text
AMM별 exact quote를 더 앞단에 반영
LB route는 getSwapOut 기반으로 후보 생성
route-level expected output cache
pool-direction failure cache
targeted replay 결과를 scoring에 반영
```

### 질문 4. Avalanche에서 Trader Joe LB 전체를 보는 것이 맞는가?

현재 결과:

```text
전체 LB pool 825 live pools
candidate 많이 생성
실제 후보 0
```

검토할 것:

```text
LB 전체 pool universe가 너무 넓은가?
풀을 제한하지 않고도 pool quality score로 선별할 수 있는가?
active liquidity 기준으로 candidate graph를 만들 수 있는가?
```

### 질문 5. Base와 Avalanche 중 어디를 계속해야 하는가?

Base:

```text
경쟁이 강함
Flashblocks/raw stream/local V3 simulator 없으면 어려움
```

Avalanche:

```text
후보는 더 많이 보임
LB 전체 pool도 볼 수 있음
하지만 아직 실매매 후보 0
```

현재로서는 Avalanche가 더 가능성이 있어 보이지만, 실매매 후보는 아직 없습니다.

## 11. 다음 액션 제안

제가 임의로 계속 설계를 진행하기보다, 다음 검토자는 아래 순서로 판단해야 합니다.

```text
1. Trader Joe LB whitepaper/interface 기준으로 local quote model 검증
2. activeId/binStep/token decimals 반영이 맞는지 검증
3. getSwapOut 결과와 executor swap 결과가 같은지 단일 pool 단위로 테스트
4. 2-hop/3-hop targeted route에서 hop별 output 로그 추가
5. route가 gross_nonpositive로 사라지는 정확한 hop 식별
6. 그 결과를 candidate scoring에 반영
7. 그 후 전체 pool/token 탐색 재개
```

성공 조건은 단 하나입니다.

```text
fork executor simulation 기준 simulated > 0
```

그 전까지는 실매매 배포를 하면 안 됩니다.

## 12. 현재 결론

현재 프로젝트는 많은 기능이 추가됐지만, 목표 달성에는 아직 실패했습니다.

목표:

```text
실매매 가능한 차익거래 후보 발견
```

현재 상태:

```text
Base: 실패
Avalanche: 실패
BSC/Sonic/Gnosis/Celo/Linea: 현재 지원 범위 기준 실패
```

가장 중요한 사실:

```text
탐색 후보는 많다.
실제 후보는 없다.
```

따라서 다음 작업은 후보를 더 많이 만드는 것이 아니라, **후보 생성 모델과 실제 실행 모델을 일치시키는 것**입니다.

이 문서는 더 뛰어난 AI 또는 외부 전문가가 이 프로젝트를 이어서 검토하기 위한 기준 자료입니다.
제가 한 시도와 실패를 숨기지 않고 남기는 것이 목적입니다.
