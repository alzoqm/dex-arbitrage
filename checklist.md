# 검증 체크리스트

작성일: 2026-04-12
기준: `report.md` 문제 항목 검토, 현재 코드 검증, QA 테스트 보강 결과

상태 표기는 요청에 따라 두 가지로만 사용한다.

- `수정 완료`: 실제 재현 가능하거나 구조상 결함으로 확인되어 코드, 설정, 테스트를 수정한 항목
- `수정 불필요`: 현재 구현상 문제가 아니거나 테스트 보강으로 동작을 검증한 항목

## 수정 완료

| 상태 | 항목 | 검증 및 조치 |
| --- | --- | --- |
| 수정 완료 | 사이클 앵커와 플래시론 가능 토큰 의미가 섞이는 문제 | `flash_loan_enabled` 대신 `is_cycle_anchor`를 그래프 시작점 기준으로 사용하도록 수정하고, Aave reserve 적재가 기존 앵커 설정을 덮어쓰지 않도록 보존했다. |
| 수정 완료 | 자체 자금 사용 허용 여부가 라우팅과 자본 선택에 일관되게 반영되지 않는 문제 | `allow_self_funded`를 자본 선택 로직에서 강제하고, 자체 자금만으로 가능한 경로는 Aave pool 설정 없이도 허용하도록 수정했다. |
| 수정 완료 | 혼합 플래시론에서 전체 입력 금액 기준으로 수수료와 cap을 판단하는 문제 | 실제 부족분만 플래시론 금액으로 계산하도록 라우터, validator, capital selector의 수수료 산정을 정리했다. |
| 수정 완료 | Aave pool 미설정 시 자체 자금 경로까지 막힐 수 있는 문제 | 런타임에서 Aave pool을 항상 요구하지 않고, 플래시론이 필요한 경로에서만 요구하도록 수정했다. |
| 수정 완료 | discovery admission이 풀 health 상태를 충분히 반영하지 않는 문제 | `PoolHealth::healthy()` 기반 admission을 적용하고, 풀 패치 갱신 시 paused, quarantined, confidence 같은 health 속성을 보존하도록 수정했다. |
| 수정 완료 | 이벤트 스트림이 주소 필터, 백필, 채널 압력 제어를 충분히 갖추지 못한 문제 | WSS 로그 주소 필터, bounded channel, 백필 동작을 추가하고 기본 ingest mode를 `address_logs`로 정리했다. |
| 수정 완료 | Aave reserve config와 multicall 실패 처리의 견고성 부족 | active, unpaused, flash-enabled reserve만 인정하고 multicall 실패 시 개별 호출 fallback으로 복구하도록 보강했다. |
| 수정 완료 | Curve underlying 지원 여부 기본값이 과도하게 낙관적인 문제 | `supports_underlying` 기본값을 false로 변경해 명시 확인되지 않은 underlying 경로를 사용하지 않도록 했다. |
| 수정 완료 | V3/Curve quote 단계에서 토큰 인덱스를 조용히 0으로 대체할 수 있는 문제 | 토큰 인덱스가 없으면 즉시 실패하도록 변경해 잘못된 quote와 calldata 생성을 막았다. |
| 수정 완료 | Uniswap V3 sqrt price limit extraData 인코딩이 누락되거나 폭이 깨질 수 있는 문제 | 32바이트 big-endian 인코딩과 sqrt price limit 계산을 추가하고 tx builder 테스트로 고정했다. |
| 수정 완료 | private/protected submit 경로가 설정된 RPC method와 receipt 상태를 충분히 추적하지 않는 문제 | 채널별 submit method 사용, tx hash 파싱, receipt polling, timeout 처리를 추가했다. |
| 수정 완료 | detector가 confidence나 staleness가 낮은 edge를 후보로 사용할 수 있는 문제 | pool health 기반 edge 사용성 판단을 추가하고 low-confidence edge 필터 테스트를 보강했다. |
| 수정 완료 | 운영 기본값이 외부 노출 또는 오동작 위험을 남기는 문제 | Prometheus bind 기본값을 localhost로 제한하고, target allowlist를 strict 기본 운영에 맞춰 정리했다. |
| 수정 완료 | Polygon private mempool RPC와 read preconfirmation RPC 의미가 섞이는 문제 | `POLYGON_PRECONF_RPC_URL`을 별도 도입하고 private submit 경로와 public/read 경로를 분리했다. |
| 수정 완료 | 토큰 메타데이터 cache 저장이 과도하게 반복될 수 있는 문제 | discovery 적재 중 cache 저장을 batch 처리하도록 조정했다. |
| 수정 완료 | 테스트케이스 부족 | capital selector, graph, detector, discovery, submitter, RPC, tx builder, AMM math, quantity search, risk valuation, Forge mixed flash loan 테스트를 대폭 보강했다. |
| 수정 완료 | 오래된 계획/완료 문서가 현재 구현 상태와 충돌할 수 있는 문제 | stale 문서 `FLASH_LOAN_FLEXIBLE_START_END_PLAN.md`, `PHASE2_IMPLEMENTATION_COMPLETE.md`를 제거했다. |

## 수정 불필요

| 상태 | 항목 | 검증 결과 |
| --- | --- | --- |
| 수정 불필요 | Uniswap V2, Uniswap V3, Curve, Balancer fallback quote 기본 수학 | 추가 AMM 테스트로 정상 quote, fee 증가 시 감소, zero input, invalid reserve/index 방어를 확인했다. 프로덕션 로직 수정은 필요하지 않았다. |
| 수정 불필요 | risk valuation의 zero price, overflow, floor rounding 처리 | 단위 테스트를 추가해 `None` 반환과 floor rounding 동작을 확인했다. 프로덕션 로직 수정은 필요하지 않았다. |
| 수정 불필요 | quantity search의 USD 최소 거래액과 토큰별 position cap 적용 | 신규 통합 테스트로 price 누락 fallback, cap 적용, refinement point 범위를 확인했다. 프로덕션 로직 수정은 필요하지 않았다. |
| 수정 불필요 | graph snapshot의 pair/pool index 구성 | graph 테스트로 pair index와 pool index 구성을 확인했다. 앵커 의미 수정 외 별도 snapshot 구조 변경은 필요하지 않았다. |
| 수정 불필요 | mixed flash loan Solidity executor 동작 | Forge 테스트로 full flash, partial flash, min profit revert, shortfall revert, V2 fee extraData 동작을 확인했다. 컨트랙트 수정은 필요하지 않았다. |

## 완료 검증

- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`
- `forge test`

