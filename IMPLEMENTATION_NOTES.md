# Implementation notes

이 저장소는 업로드된 설계서를 기준으로 Rust 런타임, Solidity executor, 테스트, 배포 스크립트까지 포함한 프로젝트 골격과 핵심 로직을 채운 결과물이다.

## 현재 포함된 것

- Base / Polygon 체인별 TOML + `.env` 기반 설정
- discovery / graph snapshot / detector / router / validator / submitter 파이프라인
- self-funded / Aave V3 flash loan 분기
- Uniswap V2/V3, Curve, Balancer용 실행 어댑터 로직이 포함된 executor 계약
- Rust 단위 테스트 샘플
- Foundry 배포 스크립트 / 실행 스크립트 / env 점검 스크립트

## 확인이 필요한 부분

이 컨테이너에는 `cargo`, `rustc`, `forge`, `solc` 가 설치되어 있지 않아 다음 항목은 여기서 실제로 수행하지 못했다.

- `cargo check`
- `cargo test`
- `forge build`
- `forge test`
- 실제 체인 또는 fork RPC 대상 dry-run

따라서 실사용 전에는 반드시 로컬/CI 환경에서 위 검증을 먼저 실행해야 한다.

## 우선 검증 순서

1. `.env` 작성
2. `./scripts/check_env.sh base` 또는 `./scripts/check_env.sh polygon`
3. `cargo fmt && cargo clippy && cargo test`
4. `forge build`
5. fork 환경에서 `--simulate-only`
6. 소액 self-funded
7. protected/private submit
8. flash loan
