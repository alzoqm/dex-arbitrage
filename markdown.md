# 프로젝트 진행 중 반복된 문제와 교정 기준

작성일: 2026-04-21

이 문서는 현재 `dex-arbitrage` 프로젝트를 Base에서 Avalanche 중심으로 옮겨가며 진행하는 과정에서, 제가 반복적으로 잘못 접근했던 부분을 정리한 기록입니다. 목적은 단순 회고가 아니라 앞으로 같은 방식의 시행착오를 막기 위한 작업 기준을 명확히 남기는 것입니다.

## 1. 가장 큰 문제: 후보의 의미를 혼동함

초기에 저는 “후보가 있다”는 말을 너무 느슨하게 사용했습니다.

실제로는 후보가 최소 세 단계로 나뉩니다.

```text
1. 탐색 후보
   그래프/spot 가격상 차익처럼 보이는 경로

2. 라우팅 후보
   입력 수량을 넣었을 때 각 hop의 quote가 계산되는 경로

3. 실매매 후보
   실제 executor calldata를 만들고, gas/flash loan fee/slippage/min profit까지 반영한 뒤,
   fork 또는 RPC simulation에서 통과하는 경로
```

제가 “후보 있음”이라고 말한 것은 여러 번 1번 또는 2번 기준이었습니다. 하지만 사용자가 원하는 것은 당연히 3번입니다.

앞으로는 아래처럼 표현해야 합니다.

```text
탐색 후보: N개
route 통과 후보: N개
validator/fork simulation 통과 후보: N개
실매매 가능 후보: N개
```

이 구분 없이 “후보 있음”이라고 말하면 판단을 흐립니다.

## 2. 파라미터 조정으로 구조 문제를 해결하려고 함

Base에서 후보가 계속 사라졌을 때, 저는 여러 번 다음과 같은 값을 바꿨습니다.

```text
SEARCH_MAX_CANDIDATES_PER_REFRESH
SEARCH_TOP_K_PATHS_PER_SIDE
SEARCH_PATH_BEAM_WIDTH
ROUTE_VERIFY_SCAN_LIMIT
min_trade_usd_e8
trusted token 설정
```

이런 값들은 탐색 폭이나 비용을 조절하는 데 필요하지만, 근본 문제를 해결하지 않습니다.

근본 문제는 다음이었습니다.

```text
AMM별 실제 가격/수량 계산 모델이 정확하지 않음
후보 생성 단계의 가격 모델과 executor 실행 단계의 실제 결과가 다름
route/rank 단계에서 수익처럼 보이지만 exact/fork에서 사라짐
```

즉, 수치 조정으로 해결할 문제가 아니라 AMM별 정확한 quote/sizing 모델을 구현해야 하는 문제였습니다.

교정 기준:

```text
수치 조정은 마지막 단계다.
먼저 quote model == fork execution 결과가 맞는지 검증한다.
```

## 3. 토큰 제한으로 문제를 회피하려고 함

Base와 Avalanche에서 롱테일 토큰이 너무 많이 섞이자 저는 `trusted token` 제한을 강하게 걸었습니다.

예:

```env
REQUIRE_TRUSTED_ROUTE_TOKENS=true
EXECUTION_TRUSTED_SYMBOLS=USDC,USDT,WETH,DAI,cbBTC
```

이 방식은 허수 후보를 줄이지만, 동시에 실제 기회도 같이 제거할 수 있습니다. 사용자가 지적한 것처럼 “전체 토큰을 본 뒤 제대로 선별”해야지, 처음부터 토큰을 제한해서 후보를 없애면 안 됩니다.

올바른 방향은 다음입니다.

```text
전체 토큰/전체 pool을 최대한 본다.
단, token allowlist로 막는 것이 아니라 execution feasibility로 선별한다.
```

허수 후보를 제거하는 기준은 symbol allowlist가 아니라 아래여야 합니다.

```text
실제 quote 가능 여부
실제 calldata simulation 가능 여부
실제 output >= min output 여부
flash loan repayment 가능 여부
gas 포함 net profit > 0 여부
token transfer 실패 여부
fee-on-transfer/rebasing/callback token 여부
pool liquidity/depth
```

`TEST`, `FAKE`, `MOCK`, `SCAM` 같은 명백한 테스트 토큰 제외는 안전장치로 둘 수 있지만, 일반적인 후보 발굴 전략이 되어서는 안 됩니다.

## 4. 외부 자료 기반 설계를 충분히 하지 않음

Trader Joe Liquidity Book을 붙이기 전, 저는 초기에 기존 V2/V3 사고방식으로 접근했습니다.

이것은 잘못된 접근입니다.

Trader Joe LB는 단순 constant product AMM이 아닙니다. 가격은 bin 구조, activeId, binStep, bin별 유동성에 의해 결정됩니다. 전체 reserve 비율만 보고 spot price를 만들면 실제 swap 결과와 크게 달라질 수 있습니다.

외부 자료에서 확인한 핵심:

```text
Trader Joe LB는 discrete bin 기반 구조다.
active bin 주변의 유동성이 실제 가격과 slippage를 결정한다.
getSwapOut은 amountIn, swapForY 기준으로 실제 swap 가능 output과 남는 input을 반환한다.
전체 reserve 합계는 실제 marginal price가 아니다.
```

관련 근거:

```text
Trader Joe joe-v2 interfaces:
ILBFactory
ILBPair
ILBRouter

핵심 함수:
getNumberOfLBPairs()
getLBPairAtIndex()
getLBPairInformation()
getTokenX()
getTokenY()
getBinStep()
getActiveId()
getReserves()
getSwapOut()
swap()
```

교정 기준:

```text
새 AMM을 추가할 때는 먼저 공식 컨트랙트/interface/whitepaper를 확인한다.
그 다음 최소 구현을 한다.
그 다음 fork execution과 quote가 일치하는지 확인한다.
```

## 5. “많이 보기”와 “정확히 보기”의 순서를 혼동함

사용자의 요구는 전체 토큰과 전체 pool을 넓게 보라는 것이었습니다. 그 자체는 맞습니다.

하지만 제가 한동안 잘못한 점은 “많이 보기”를 먼저 하고, 잘못된 quote 모델로 candidate를 대량 생성한 것입니다.

그 결과:

```text
candidate_count는 증가
routed도 일부 증가
하지만 exact/fork에서 전부 사라짐
RPC 비용과 시간만 증가
```

올바른 구조는 다음입니다.

```text
1. 전체 pool/token을 가져온다.
2. 각 AMM별로 실제 실행과 일치하는 quote model을 만든다.
3. 그 quote model로 net profit을 먼저 계산한다.
4. 그 결과를 기준으로 top-k를 만든다.
5. top-k만 fork/RPC simulation으로 보낸다.
```

즉:

```text
잘못된 방식:
spot/top-k -> exact 검증 -> 대부분 탈락

올바른 방식:
AMM별 실제 quote/sizing -> gas/fee/slippage 포함 net profit -> top-k -> simulation
```

## 6. Base에서의 실패 원인

Base에서는 아래 현상이 반복됐습니다.

```text
trusted token 기준: 후보 0개
토큰 제한 해제: 후보는 생김
route/exact/fork 기준: 실매매 후보 0개
```

주요 원인:

```text
경쟁이 너무 강함
Flashblocks/MEV 환경에서 latency 우위가 부족함
V3/Slipstream exact quote가 충분히 빠르고 정확하지 않음
롱테일 토큰 기반 false positive가 많음
전체 후보를 정확히 계산하기 전에 top-k를 뽑는 구조가 남아 있었음
```

Base에서 얻은 교훈:

```text
Base는 public infra와 일반 후보 탐색으로 실매매 알파를 잡기 어렵다.
Base를 계속 하려면 raw Flashblocks stream + local V3 exact simulator가 필요하다.
```

## 7. Avalanche로 옮긴 이유

Avalanche는 Base보다 경쟁이 약할 수 있고, Aave V3 flash loan과 주요 DEX가 있습니다.

초기 scout 결과:

```text
Avalanche:
  탐색 후보 있음

Sonic/Gnosis/Celo/Linea:
  현재 지원 범위에서는 실질 후보 없음

BSC:
  탐색 후보는 있으나 route 통과 없음
```

Avalanche에서 후보가 가장 많이 나왔기 때문에 현재 1순위 체인으로 전환했습니다.

## 8. Avalanche에서 확인한 문제

Avalanche V2 계열에서는 후보가 나왔지만 fork executor에서 실패했습니다.

대표 실패:

```text
Joe: TRANSFER_FAILED
```

원인 분석 중 확인한 사실:

```text
일부 route에 TEST TOKEN 같은 명백한 허위 토큰이 포함됨
V2 reserve 기반 후보는 route/risk 단계에서 수익처럼 보여도 실제 executor에서 실패 가능
Trader Joe V1만으로는 Avalanche 핵심 유동성을 충분히 커버하지 못함
```

그래서 Trader Joe LB 지원을 추가했습니다.

## 9. Trader Joe LB 구현에서 확인한 사실

구현한 것:

```text
TraderJoeLb AMM kind
TraderJoeLbConfiguredPairs
TraderJoeLbFactoryAllPairs
LB pair discovery
LB pool state fetch
LB batch fetch
LB direct pair execution
LB activeId 기반 spot price
LB getSwapOut exact quote
LB targeted replay 기반
```

개선된 점:

```text
LB 전체 factory pool을 볼 수 있음
3354개 LB pool 중 825개 live pool 확인
LB pool fetch 속도 개선
targeted replay 가능
```

남은 문제:

```text
LB 전체 기준 후보는 생기지만 exact verification/fork 기준 실매매 후보는 아직 0
일부 후보는 롱테일 토큰에서 발생
major token만 보면 route 통과가 없음
getSwapOut 기반 정확 계산 후 수익성이 사라짐
```

## 10. targeted replay가 필요한 이유

전체 pool을 매번 훑으면 느리고 비용이 듭니다.

그래서 특정 route만 재현할 수 있도록 다음을 추가했습니다.

```text
INITIAL_REFRESH_POOL_IDS
scripts/replay_avalanche_route.py
pool_route 로그 출력
```

이제 다음처럼 특정 route만 재현할 수 있습니다.

```bash
python3 scripts/replay_avalanche_route.py \
  --from-log state/chain-scout/<pool_route_있는_로그>.log \
  --index 0
```

또는:

```bash
python3 scripts/replay_avalanche_route.py \
  --pool-route 0xPoolA,0xPoolB,0xPoolC
```

이 구조를 통해 앞으로는 다음을 빠르게 확인할 수 있습니다.

```text
특정 route가 왜 gross_nonpositive가 되는지
어느 hop에서 output이 줄어드는지
어느 pool의 quote가 과대평가되는지
fork executor에서 어떤 revert가 나는지
```

## 11. 현재 상태 요약

현재 프로젝트 상태:

```text
Avalanche 자금 준비 완료
Avalanche 설정 추가 완료
Avalanche V2 + Trader Joe LB discovery 가능
Trader Joe LB 전체 pool fetch 가능
LB 후보 생성 가능
targeted replay 가능
실매매 후보는 아직 0
```

현재 자금 상태:

```text
Avalanche operator: 약 10.53 AVAX
Avalanche deployer: 4.5 AVAX
Base operator: 약 0.0063 ETH
Base deployer: 약 0.006 ETH
```

배포는 아직 하지 않는 것이 맞습니다.

이유:

```text
현재 executor는 LB 실행 경로가 추가됐지만,
실제 실매매 후보가 아직 fork 기준으로 통과하지 않았음.
지금 배포하면 다시 재배포할 가능성이 큼.
```

## 12. 앞으로의 원칙

앞으로는 아래 원칙을 지켜야 합니다.

```text
1. 외부 자료 기반으로 AMM 구조를 먼저 확인한다.
2. 후보라는 말을 단계별로 구분한다.
3. 토큰 제한으로 문제를 숨기지 않는다.
4. 전체 토큰/전체 pool을 보되, 실행 검증으로 선별한다.
5. 파라미터 조정보다 quote/execution 모델 정합성을 우선한다.
6. 전체 스캔 반복보다 targeted replay를 우선한다.
7. fork executor simulation 통과 전에는 실배포하지 않는다.
```

## 13. 다음 작업

다음 작업은 수치 조정이 아닙니다.

해야 할 일:

```text
1. 상위 LB route를 targeted replay로 하나씩 재현한다.
2. 각 hop의 getSwapOut 결과를 기록한다.
3. route 전체 output 변화와 fork 결과를 비교한다.
4. 수익성이 사라지는 정확한 hop/pool을 찾는다.
5. 그 결과로 LB route scoring과 sizing을 수정한다.
```

성공 조건:

```text
fork executor simulation 기준 simulated > 0
net_profit_usd_e8 > 0
gas 포함 후에도 profit 유지
토큰 전송/revert 없음
```

그 전까지는 live deployment를 하면 안 됩니다.
