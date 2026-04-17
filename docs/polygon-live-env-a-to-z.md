**먼저 결론**
지금 네가 원하는 “Polygon 전체 실매매 범위”는 **env만 채운다고 완성되지 않는다**. 현재 코드 기준으로 env만으로 넓힐 수 있는 최대 실매매 범위는 다음 정도다.

```text
Polygon + Aave V3 flash loan
+ Uniswap V3
+ QuickSwap V2
+ SushiSwap V2
+ Curve plain registry
+ Balancer weighted vault
```

다만 아래는 **현재 코드에서 그대로 실매매 범위에 넣으면 안 된다**.

```text
QuickSwap V3 = Algebra 계열이라 현재 Uniswap V3 adapter와 ABI가 맞지 않음
Polygon 전체 토큰 = env만으로는 가격/리스크 검증이 안 됨
진짜 모든 DEX = 각 DEX별 adapter가 필요함
```

즉, “Polygon 체인 전체의 모든 DEX/모든 토큰”은 env 세팅 문제가 아니라 **코드 지원 범위 문제**다. 아래는 “현재 프로젝트가 지원 가능한 범위 안에서 실매매 수준으로 넓히기 위해 채워야 하는 env” 기준이다.

**1. 최종 .env 목표값**
아래 블록을 기준으로 `.env`의 Polygon/운영 부분을 맞추면 된다. 비밀값은 직접 넣어야 한다.

```env
# =========================
# Global
# =========================
CHAIN=polygon
SIMULATION_ONLY=true
STRICT_TARGET_ALLOWLIST=true

OPERATOR_PRIVATE_KEY=<새로 만든 Polygon 운영 hot wallet private key>
DEPLOYER_PRIVATE_KEY=<새로 만든 Polygon 배포 wallet private key>
SAFE_OWNER=<Polygon Safe 주소>

# =========================
# Polygon RPC / submission
# =========================
POLYGON_PUBLIC_RPC_URL=https://polygon-mainnet.g.alchemy.com/v2/<ALCHEMY_API_KEY>
POLYGON_FALLBACK_RPC_URL=https://polygon-bor-rpc.publicnode.com
POLYGON_PRECONF_RPC_URL=
POLYGON_WSS_URL=wss://polygon-mainnet.g.alchemy.com/v2/<ALCHEMY_API_KEY>

# 실매매에서는 반드시 채우는 것을 권장.
# Polygon Private Mempool 또는 다른 private submission endpoint.
POLYGON_PRIVATE_MEMPOOL_URL=<Polygon private mempool RPC URL>
POLYGON_PRIVATE_SUBMIT_METHOD=eth_sendRawTransaction
POLYGON_SIMULATE_METHOD=eth_call

# 배포 후 채움
POLYGON_EXECUTOR_ADDRESS=<배포된 ArbitrageExecutor 주소>

# Aave V3 Polygon Pool
POLYGON_AAVE_POOL=0x794a61358D6845594F94dc1DB02A252b5b4814aD

# =========================
# Polygon tokens
# =========================
# Native USDC
POLYGON_USDC=0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359
POLYGON_USDT=0xc2132D05D31c914a87C6611C10748AEb04B58e8F
# Polygon wrapped native. symbol()은 WPOL이지만 현재 코드 키는 POLYGON_WMATIC.
POLYGON_WMATIC=0x0d500B1d8E8eF31E21C99d1Db9A6444d3ADf1270
POLYGON_WETH=0x7ceB23fD6bC0adD59E62ac25578270cFf1b9f619
POLYGON_DAI=0x8f3Cf7ad23Cd3CaDbD9735AFf958023239c6A063

# Polygon 주요 liquidity 때문에 강력 권장: config에 토큰도 추가해야 실제 사용됨
POLYGON_USDCE=0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174
POLYGON_USDCE_PRICE_E8=100000000

# =========================
# Polygon DEX infra
# =========================
# 공식 Polygon Uniswap V2 factory는 현재 이 프로젝트에서 쓸 값이 없음. 비워두고 disabled 유지 권장.
POLYGON_UNISWAP_V2_FACTORY=

POLYGON_QUICKSWAP_V2_FACTORY=0x5757371414417b8C6CAad45bAeF941aBc7d3Ab32
POLYGON_SUSHISWAP_V2_FACTORY=0xc35DADB65012eC5796536bD9864eD8773aBc74C4

POLYGON_UNISWAP_V3_FACTORY=0x1F98431c8aD98523631AE4a59f267346ea31F984
POLYGON_UNISWAP_V3_QUOTER=0x61fFE014bA17989E743c5F6cB21bF9697530B21e

# 주소는 공식값이지만 현재 코드 adapter 미지원. 값은 보관 가능, enabled=false 유지.
POLYGON_QUICKSWAP_V3_FACTORY=0x411b0fAcC3489691f28ad58c47006AF5E3Ab3A28
POLYGON_QUICKSWAP_V3_QUOTER=0xa15F0D7377B2A0C0c10db057f641beD21028FC89

POLYGON_CURVE_REGISTRY=0x094d12e5b541784701FD8d65F11fc0598FBC6332
POLYGON_BALANCER_VAULT=0xBA12222222228d8Ba445958a75a0704d566BF2C8

# =========================
# Polygon prices, USD e8
# =========================
POLYGON_USDC_PRICE_E8=100000000
POLYGON_USDT_PRICE_E8=100000000
POLYGON_DAI_PRICE_E8=100000000

# 아래 2개는 실매매 전 매번 최신값으로 갱신
POLYGON_WMATIC_PRICE_E8=<POL/USD 또는 MATIC/USD 가격 * 1e8>
POLYGON_WETH_PRICE_E8=<ETH/USD 가격 * 1e8>
```

**2. Alchemy RPC 얻는 방법**
1. https://dashboard.alchemy.com 접속.
2. 가입 또는 로그인.
3. `Create new app` 클릭.
4. Chain은 `Polygon PoS`, Network는 `Mainnet` 선택.
5. 앱 생성 후 `API Key` 또는 `Endpoints` 클릭.
6. `HTTPS` 값을 복사해서 `POLYGON_PUBLIC_RPC_URL`에 넣는다.
7. `WebSocket` 값을 복사해서 `POLYGON_WSS_URL`에 넣는다.
8. `cast chain-id --rpc-url "$POLYGON_PUBLIC_RPC_URL"` 실행해서 `137`이 나오면 정상이다.

중요: Alchemy MEV Protection 공식 문서의 지원 네트워크에는 Polygon이 없다. Polygon 실매매용 private submission은 Alchemy standard RPC가 아니라 Polygon Private Mempool 같은 별도 endpoint를 써야 한다.

**3. Polygon private mempool URL 얻는 방법**
1. Polygon 공식 글에 있는 Private Mempool 안내로 이동: https://polygon.technology/blog/polygon-launches-private-mempool-mev-protection-is-now-a-one-line-integration
2. 글 하단의 `reach out` 또는 Polygon contact 링크로 문의한다.
3. 문의 내용은 이렇게 쓰면 된다.

```text
I need access to Polygon Private Mempool for Polygon PoS mainnet.
Use case: automated DEX arbitrage transaction submission.
Required interface: standard JSON-RPC eth_sendRawTransaction endpoint.
I will keep read RPC on Alchemy and use the private endpoint only for transaction submission.
Please provide endpoint URL, rate limits, authentication method, and production SLA options.
```

4. 받은 private submit RPC를 `POLYGON_PRIVATE_MEMPOOL_URL`에 넣는다.
5. 아직 못 받았으면 실매매는 하지 말고 `SIMULATION_ONLY=true` 유지한다.

정말 public mempool로라도 보내겠다면 `ALLOW_PUBLIC_FALLBACK=true`가 필요하지만, arbitrage에서는 샌드위치/프론트런/백런 노출이 커서 권장하지 않는다.

**4. 운영 wallet 만들기**
터미널에서 실행한다.

```bash
cast wallet new
```

출력에서 private key를 `OPERATOR_PRIVATE_KEY`에 넣는다. address는 따로 기록한다.

```bash
OPERATOR_ADDRESS=$(cast wallet address --private-key "$OPERATOR_PRIVATE_KEY")
echo "$OPERATOR_ADDRESS"
```

이 주소에는 실매매 트랜잭션 gas용 POL만 소량 넣는다. 큰 자금은 넣지 않는다.

**5. 배포 wallet 만들기**
다시 새 wallet을 만든다.

```bash
cast wallet new
```

이 private key를 `DEPLOYER_PRIVATE_KEY`에 넣는다. 이 주소에도 Polygon POL을 넣는다. 처음에는 배포 gas + 여유분만 넣으면 된다.

확인:

```bash
DEPLOYER_ADDRESS=$(cast wallet address --private-key "$DEPLOYER_PRIVATE_KEY")
cast balance "$DEPLOYER_ADDRESS" --rpc-url "$POLYGON_PUBLIC_RPC_URL"
```

**6. Polygon Safe 만들기**
1. https://app.safe.global 접속.
2. `Create new Account` 클릭.
3. Network를 `Polygon`으로 선택.
4. Safe 이름 입력.
5. Owner를 최소 2개 넣는다. 예: 하드웨어 wallet 1개, 백업 wallet 1개.
6. Threshold는 처음에는 `1/2` 또는 더 안전하게 `2/2` 선택.
7. 생성 트랜잭션 서명.
8. 생성된 Safe 주소를 복사해서 `SAFE_OWNER`에 넣는다.

검증:

```bash
cast code "$SAFE_OWNER" --rpc-url "$POLYGON_PUBLIC_RPC_URL"
cast call "$SAFE_OWNER" "getOwners()(address[])" --rpc-url "$POLYGON_PUBLIC_RPC_URL"
cast call "$SAFE_OWNER" "getThreshold()(uint256)" --rpc-url "$POLYGON_PUBLIC_RPC_URL"
```

`cast code`가 `0x`면 잘못된 주소다.

**7. executor 배포**
`.env`에 최소한 아래가 있어야 한다.

```env
DEPLOYER_PRIVATE_KEY=...
SAFE_OWNER=...
POLYGON_PUBLIC_RPC_URL=...
POLYGON_AAVE_POOL=0x794a61358D6845594F94dc1DB02A252b5b4814aD
```

실행:

```bash
source .env
./scripts/check_env.sh polygon deploy
forge build
./scripts/deploy_polygon.sh
```

출력에 배포 주소가 나온다. 그 주소를 넣는다.

```env
POLYGON_EXECUTOR_ADDRESS=<방금 배포된 주소>
```

검증:

```bash
cast code "$POLYGON_EXECUTOR_ADDRESS" --rpc-url "$POLYGON_PUBLIC_RPC_URL"
cast call "$POLYGON_EXECUTOR_ADDRESS" "aavePool()(address)" --rpc-url "$POLYGON_PUBLIC_RPC_URL"
cast call "$POLYGON_EXECUTOR_ADDRESS" "owner()(address)" --rpc-url "$POLYGON_PUBLIC_RPC_URL"
cast call "$POLYGON_EXECUTOR_ADDRESS" "paused()(bool)" --rpc-url "$POLYGON_PUBLIC_RPC_URL"
```

`aavePool()` 결과가 `0x794a...4814aD`이고 `owner()`가 `SAFE_OWNER`여야 한다.

**8. operator 등록**
executor owner를 Safe로 배포했다면, `setOperator`는 deployer key로 바로 못 한다. Safe UI에서 트랜잭션을 만들어야 한다.

Safe에서 실행할 call:

```text
to: POLYGON_EXECUTOR_ADDRESS
function: setOperator(address,bool)
args:
  operator = OPERATOR_ADDRESS
  allowed = true
```

EOA owner로 테스트 배포한 경우에만 아래처럼 가능하다.

```bash
OPERATOR_ADDRESS=$(cast wallet address --private-key "$OPERATOR_PRIVATE_KEY")

cast send "$POLYGON_EXECUTOR_ADDRESS" "setOperator(address,bool)" "$OPERATOR_ADDRESS" true \
  --rpc-url "$POLYGON_PUBLIC_RPC_URL" \
  --private-key "$DEPLOYER_PRIVATE_KEY"
```

실매매는 Safe owner 기준으로 가는 게 맞다.

**9. DEX 주소 검증**
env를 채운 뒤 아래를 그대로 실행한다.

```bash
source .env

cast call "$POLYGON_QUICKSWAP_V2_FACTORY" "allPairsLength()(uint256)" \
  --rpc-url "$POLYGON_PUBLIC_RPC_URL"

cast call "$POLYGON_SUSHISWAP_V2_FACTORY" "allPairsLength()(uint256)" \
  --rpc-url "$POLYGON_PUBLIC_RPC_URL"

cast call "$POLYGON_UNISWAP_V3_FACTORY" \
  "getPool(address,address,uint24)(address)" \
  "$POLYGON_USDC" "$POLYGON_WETH" 500 \
  --rpc-url "$POLYGON_PUBLIC_RPC_URL"

cast call "$POLYGON_CURVE_REGISTRY" "pool_count()(uint256)" \
  --rpc-url "$POLYGON_PUBLIC_RPC_URL"

cast call "$POLYGON_BALANCER_VAULT" "getAuthorizer()(address)" \
  --rpc-url "$POLYGON_PUBLIC_RPC_URL"
```

각 호출이 revert 없이 숫자/주소를 반환해야 한다.

**10. Aave flash loan 가능 여부 확인**
주요 토큰별로 확인한다.

```bash
source .env

for token in POLYGON_USDC POLYGON_USDT POLYGON_WMATIC POLYGON_WETH POLYGON_DAI; do
  addr="${!token}"
  raw=$(cast call "$POLYGON_AAVE_POOL" "getConfiguration(address)((uint256))" "$addr" \
    --rpc-url "$POLYGON_PUBLIC_RPC_URL" | awk '{print $1}' | tr -d '()')
  python3 - <<PY
n=int("$raw", 0)
print("$token", "active", bool((n>>56)&1), "paused", bool((n>>60)&1), "flash", bool((n>>63)&1))
PY
done
```

정상 조건:

```text
active True
paused False
flash True
```

**11. 가격 env 갱신**
stable은 아래처럼 고정해도 된다.

```env
POLYGON_USDC_PRICE_E8=100000000
POLYGON_USDT_PRICE_E8=100000000
POLYGON_DAI_PRICE_E8=100000000
```

WETH/POL은 실매매 직전에 Chainlink feed에서 가져온 값을 넣는다.

```bash
# POL 또는 MATIC / USD
cast call 0xAB594600376Ec9fD91F8e885dADF0CE036862dE0 \
  "latestRoundData()(uint80,int256,uint256,uint256,uint80)" \
  --rpc-url "$POLYGON_PUBLIC_RPC_URL"

# ETH / USD
cast call 0xF9680D99D6C9589e2a93a78A04a279e509205945 \
  "latestRoundData()(uint80,int256,uint256,uint256,uint80)" \
  --rpc-url "$POLYGON_PUBLIC_RPC_URL"
```

출력의 두 번째 값이 가격이다. 예를 들어 ETH/USD가 `218950052000`이면 그대로:

```env
POLYGON_WETH_PRICE_E8=218950052000
```

**12. 전체 지원 범위로 config도 바꿔야 함**
env만 채우면 끝이 아니다. [config/polygon.toml](/Users/yun/workspace/dex-arbitrage/config/polygon.toml)의 policy를 이렇게 해야 특정 심볼/특정 거래소 제한이 풀린다.

```toml
[policy]
venues = ["quickswap_v2", "sushiswap_v2", "uniswap_v3", "curve", "balancer"]
symbols = []
```

그리고 각 dex는 이렇게 맞춘다.

```toml
uniswap_v2      enabled = false
quickswap_v2    enabled = true
sushiswap_v2    enabled = true
uniswap_v3      enabled = true
quickswap_v3    enabled = false
curve           enabled = true
balancer        enabled = true
```

`quickswap_v3`는 주소가 있어도 현재 adapter가 Algebra를 지원하지 않으므로 `enabled=false`가 맞다.

**13. USDC.e를 추가하려면 config 수정 필요**
Polygon에는 native USDC `0x3c499...`와 bridged USDC.e `0x2791...`가 같이 있다. Aave도 둘 다 reserve에 있다. 실매매 범위를 넓히려면 `.env`만으로는 부족하고 `config/polygon.toml`에 토큰을 추가해야 한다.

`.env`:

```env
POLYGON_USDCE=0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174
POLYGON_USDCE_PRICE_E8=100000000
```

`config/polygon.toml`에 추가:

```toml
[[tokens]]
symbol = "USDC.e"
address_env = "POLYGON_USDCE"
decimals = 6
is_stable = true
price_env = "POLYGON_USDCE_PRICE_E8"
```

이걸 안 하면 Polygon의 큰 USDC.e 유동성 일부를 제대로 anchor로 쓰지 못한다.

**14. allowlist 설정**
`STRICT_TARGET_ALLOWLIST=true`면 실제 target pool을 executor에 등록해야 한다. 처음에는 discovery 결과를 보고 pool 주소를 뽑아 등록해야 한다.

Uniswap V3 주요 pool 예시:

```bash
for fee in 100 500 3000 10000; do
  cast call "$POLYGON_UNISWAP_V3_FACTORY" \
    "getPool(address,address,uint24)(address)" \
    "$POLYGON_USDC" "$POLYGON_WETH" "$fee" \
    --rpc-url "$POLYGON_PUBLIC_RPC_URL"
done
```

Safe에서 실행할 call:

```text
to: POLYGON_EXECUTOR_ADDRESS
function: setAllowedTargets(address[],bool)
args:
  targets = [풀주소1, 풀주소2, 풀주소3]
  allowed = true
```

allowlist를 풀고 싶으면 `STRICT_TARGET_ALLOWLIST=false`로 갈 수 있지만, 실매매에서는 권장하지 않는다.

**15. 최종 검증 순서**
아래 순서 그대로 간다.

```bash
source .env

rg '추가 세팅 필요|확인 필요' .env

./scripts/check_env.sh polygon verify
./scripts/check_env.sh polygon deploy
./scripts/check_env.sh polygon run

forge test
cargo test --all

forge test --match-test testPolygonConfiguredContractsHaveCodeWhenForkEnvSet

SIMULATION_ONLY=true cargo run --release -- --chain polygon --once --simulate-only
```

여기까지 통과하면 live 직전 단계다.

**16. live 전 마지막 변경**
처음 실매매는 작은 금액으로만 시작한다.

```env
SIMULATION_ONLY=false
ALLOW_PUBLIC_FALLBACK=false
MAX_CONCURRENT_TX=1

POLYGON_MAX_FLASH_LOAN_USD_E8=10000000000
POLYGON_MAX_POSITION_USD_E8=10000000000
POLYGON_MIN_NET_PROFIT_USD_E8=100000000
```

의미:

```text
MAX_FLASH_LOAN = $100
MAX_POSITION = $100
MIN_NET_PROFIT = $1
```

이후 최소 24시간 로그를 보고 리버트율, 실제 gas, private mempool inclusion, 후보 수익률을 확인한 뒤 금액을 올린다.

**공식/검증 출처**
- Aave Pool은 flash loan을 Pool contract에서 실행한다고 명시: https://aave.com/docs/aave-v3/smart-contracts/pool
- Uniswap V3 Polygon factory/quoter 공식 배포 주소: https://docs.uniswap.org/contracts/v3/reference/deployments/polygon-deployments
- QuickSwap Polygon V2/V3 공식 주소: https://docs.quickswap.exchange/overview/contracts-and-addresses
- Curve Address Provider Polygon 공식 주소: https://docs.curve.finance/references/deployed-contracts/
- Balancer Polygon deployment docs: https://docs-v2.balancer.fi/reference/contracts/deployment-addresses/polygon.html
- Polygon Private Mempool 공식 발표: https://polygon.technology/blog/polygon-launches-private-mempool-mev-protection-is-now-a-one-line-integration
- Alchemy MEV Protection 지원 네트워크에는 Polygon이 없음: https://www.alchemy.com/docs/reference/mev-protection
