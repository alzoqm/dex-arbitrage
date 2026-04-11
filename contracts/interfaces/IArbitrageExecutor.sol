// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

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
