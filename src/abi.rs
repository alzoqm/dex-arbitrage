use alloy::sol;

sol! {
    interface IERC20 {
        function symbol() external view returns (string memory);
        function decimals() external view returns (uint8);
        function balanceOf(address account) external view returns (uint256);
        function allowance(address owner, address spender) external view returns (uint256);
        function approve(address spender, uint256 amount) external returns (bool);
        function transfer(address to, uint256 amount) external returns (bool);
        function transferFrom(address from, address to, uint256 amount) external returns (bool);
    }

    interface IUniswapV2Factory {
        function allPairsLength() external view returns (uint256);
        function allPairs(uint256 index) external view returns (address);
    }

    interface IUniswapV2Pair {
        event Sync(uint112 reserve0, uint112 reserve1);

        function token0() external view returns (address);
        function token1() external view returns (address);
        function getReserves()
            external
            view
            returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast);
    }

    interface IUniswapV3Factory {
        event PoolCreated(
            address indexed token0,
            address indexed token1,
            uint24 indexed fee,
            int24 tickSpacing,
            address pool
        );
    }

    interface IUniswapV3Pool {
        function token0() external view returns (address);
        function token1() external view returns (address);
        function fee() external view returns (uint24);
        function tickSpacing() external view returns (int24);
        function liquidity() external view returns (uint128);
        function slot0()
            external
            view
            returns (
                uint160 sqrtPriceX96,
                int24 tick,
                uint16 observationIndex,
                uint16 observationCardinality,
                uint16 observationCardinalityNext,
                uint8 feeProtocol,
                bool unlocked
            );
    }

    interface IV3QuoterV2 {
        struct QuoteExactInputSingleParams {
            address tokenIn;
            address tokenOut;
            uint256 amountIn;
            uint24 fee;
            uint160 sqrtPriceLimitX96;
        }

        function quoteExactInputSingle(
            QuoteExactInputSingleParams memory params
        ) external returns (
            uint256 amountOut,
            uint160 sqrtPriceX96After,
            uint32 initializedTicksCrossed,
            uint256 gasEstimate
        );
    }

    interface ICurveRegistry {
        function pool_count() external view returns (uint256);
        function pool_list(uint256 index) external view returns (address);
    }

    interface ICurvePool {
        function A() external view returns (uint256);
        function fee() external view returns (uint256);
        function coins(uint256 i) external view returns (address);
        function balances(uint256 i) external view returns (uint256);
        function get_dy(int128 i, int128 j, uint256 dx) external view returns (uint256);
        function get_dy_underlying(int128 i, int128 j, uint256 dx) external view returns (uint256);
    }

    interface IBalancerPool {
        function getPoolId() external view returns (bytes32);
        function getNormalizedWeights() external view returns (uint256[] memory);
        function getSwapFeePercentage() external view returns (uint256);
        function getPausedState() external view returns (bool paused, uint256 pauseWindowEndTime, uint256 bufferPeriodEndTime);
    }

    interface IBalancerVault {
        event PoolRegistered(bytes32 indexed poolId, address indexed poolAddress, uint8 specialization);
        event Swap(bytes32 indexed poolId, address indexed tokenIn, address indexed tokenOut, uint256 amountIn, uint256 amountOut);
        event PoolBalanceChanged(bytes32 indexed poolId, address indexed liquidityProvider, address[] tokens, int256[] deltas, uint256[] protocolFeeAmounts);

        struct BatchSwapStep {
            bytes32 poolId;
            uint256 assetInIndex;
            uint256 assetOutIndex;
            uint256 amount;
            bytes userData;
        }

        struct FundManagement {
            address sender;
            bool fromInternalBalance;
            address recipient;
            bool toInternalBalance;
        }

        function getPoolTokens(bytes32 poolId)
            external
            view
            returns (address[] memory tokens, uint256[] memory balances, uint256 lastChangeBlock);

        function queryBatchSwap(
            uint8 kind,
            BatchSwapStep[] memory swaps,
            address[] memory assets,
            FundManagement memory funds
        ) external returns (int256[] memory assetDeltas);
    }

    interface IAavePool {
        struct ReserveConfigurationMap {
            uint256 data;
        }

        function FLASHLOAN_PREMIUM_TOTAL() external view returns (uint128);
        function getReservesList() external view returns (address[] memory);
        function getConfiguration(address asset) external view returns (ReserveConfigurationMap memory);
    }

    interface IMulticall3 {
        struct Call3 {
            address target;
            bool allowFailure;
            bytes callData;
        }

        struct Result {
            bool success;
            bytes returnData;
        }

        function aggregate3(Call3[] calldata calls)
            external
            payable
            returns (Result[] memory returnData);
    }

    interface IArbitrageExecutor {
        enum AdapterType {
            UniswapV2Like,
            UniswapV3Like,
            CurvePlain,
            BalancerWeighted
        }

        struct Split {
            AdapterType adapterType;
            address target;
            address tokenIn;
            address tokenOut;
            uint256 amountIn;
            uint256 minAmountOut;
            bytes extraData;
        }

        struct Hop {
            Split[] splits;
        }

        struct ExecutionParams {
            address inputToken;
            uint256 inputAmount;
            Hop[] hops;
            uint256 minProfit;
            uint256 deadline;
            uint64 snapshotId;
        }

        struct FlashLoanParams {
            address loanAsset;
            uint256 loanAmount;
            ExecutionParams execution;
        }

        function executeSelfFunded(ExecutionParams calldata params) external;
        function executeFlashLoan(FlashLoanParams calldata params) external;
    }
}
