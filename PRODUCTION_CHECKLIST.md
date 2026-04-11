# Production checklist

## 필수 환경 변수

- operator / deployer private key
- chain RPC endpoints
- executor contract address
- Aave pool address
- token addresses
- DEX factory / quoter / registry / vault addresses

## 배포 후 설정

- owner 확인
- `setOperator(OPERATOR, true)`
- 필요 시 `setAllowedTargets([...], true)`
- strict allowlist 사용 시 Balancer pool + vault 모두 allowlist 등록
- self-funded 모드면 executor에 자금 예치

## 런타임 시작

```bash
./scripts/check_env.sh base
cargo run --release -- --chain base --simulate-only
```

실매매 전환:

```bash
SIMULATION_ONLY=false cargo run --release -- --chain base
```
