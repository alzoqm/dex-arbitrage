// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {IERC20} from "./interfaces/IERC20.sol";
import {IAavePool} from "./interfaces/IAavePool.sol";
import {IFlashLoanSimpleReceiver} from "./interfaces/IFlashLoanSimpleReceiver.sol";
import {IUniswapV2Pair} from "./interfaces/IUniswapV2Pair.sol";
import {IAerodromeV2Pool} from "./interfaces/IAerodromeV2Pool.sol";
import {IUniswapV3Pool} from "./interfaces/IUniswapV3Pool.sol";
import {IUniswapV3SwapCallback} from "./interfaces/IUniswapV3SwapCallback.sol";
import {ICurvePool} from "./interfaces/ICurvePool.sol";
import {IBalancerPool} from "./interfaces/IBalancerPool.sol";
import {IBalancerVault} from "./interfaces/IBalancerVault.sol";

contract ArbitrageExecutor is IFlashLoanSimpleReceiver, IUniswapV3SwapCallback {
    enum AdapterType {
        UniswapV2Like,
        UniswapV3Like,
        CurvePlain,
        BalancerWeighted,
        AerodromeV2Like
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

    struct V3CallbackData {
        address tokenIn;
    }

    address public immutable aavePool;
    address public owner;
    bool public paused;
    bool public strictTargetAllowlist;

    mapping(address => bool) public operators;
    mapping(address => bool) public allowedTargets;

    uint256 private _locked = 1;
    uint160 private constant MIN_SQRT_RATIO_PLUS_ONE = 4295128740;
    uint160 private constant MAX_SQRT_RATIO_MINUS_ONE = 1461446703485210103287273052203988822378723970341;

    event OwnerTransferred(address indexed previousOwner, address indexed newOwner);
    event OperatorSet(address indexed operator, bool allowed);
    event AllowedTargetSet(address indexed target, bool allowed);
    event StrictTargetAllowlistSet(bool enabled);
    event PausedSet(bool enabled);
    event ExecutionStarted(
        address indexed caller,
        uint64 indexed snapshotId,
        address indexed inputToken,
        uint256 inputAmount,
        bool flashLoan
    );
    event SplitExecuted(
        uint64 indexed snapshotId,
        AdapterType indexed adapterType,
        address indexed target,
        address tokenIn,
        address tokenOut,
        uint256 amountIn,
        uint256 amountOut
    );
    event ExecutionFinished(
        uint64 indexed snapshotId, address indexed inputToken, uint256 endingBalance, uint256 realizedProfit
    );

    modifier onlyOwner() {
        require(msg.sender == owner, "NOT_OWNER");
        _;
    }

    modifier onlyOperator() {
        require(msg.sender == owner || operators[msg.sender], "NOT_OPERATOR");
        _;
    }

    modifier notPaused() {
        require(!paused, "PAUSED");
        _;
    }

    modifier nonReentrant() {
        require(_locked == 1, "REENTRANT");
        _locked = 2;
        _;
        _locked = 1;
    }

    constructor(address aavePool_, address owner_) {
        require(aavePool_ != address(0), "ZERO_AAVE_POOL");
        aavePool = aavePool_;
        owner = owner_ == address(0) ? msg.sender : owner_;
        emit OwnerTransferred(address(0), owner);
    }

    receive() external payable {}

    function transferOwnership(address newOwner) external onlyOwner {
        require(newOwner != address(0), "ZERO_OWNER");
        emit OwnerTransferred(owner, newOwner);
        owner = newOwner;
    }

    function setOperator(address operator, bool allowed) external onlyOwner {
        operators[operator] = allowed;
        emit OperatorSet(operator, allowed);
    }

    function setAllowedTargets(address[] calldata targets, bool allowed) external onlyOwner {
        for (uint256 i = 0; i < targets.length; ++i) {
            allowedTargets[targets[i]] = allowed;
            emit AllowedTargetSet(targets[i], allowed);
        }
    }

    function setStrictTargetAllowlist(bool enabled) external onlyOwner {
        strictTargetAllowlist = enabled;
        emit StrictTargetAllowlistSet(enabled);
    }

    function setPaused(bool enabled) external onlyOwner {
        paused = enabled;
        emit PausedSet(enabled);
    }

    function rescueTokens(address token, address to, uint256 amount) external onlyOwner {
        require(to != address(0), "ZERO_TO");
        _safeTransfer(token, to, amount);
    }

    function rescueNative(address payable to, uint256 amount) external onlyOwner {
        require(to != address(0), "ZERO_TO");
        (bool ok,) = to.call{value: amount}("");
        require(ok, "NATIVE_TRANSFER_FAILED");
    }

    function executeSelfFunded(ExecutionParams calldata params) external onlyOperator notPaused nonReentrant {
        require(params.hops.length > 0, "EMPTY_HOPS");
        require(block.timestamp <= params.deadline, "DEADLINE_EXPIRED");

        uint256 startBalance = IERC20(params.inputToken).balanceOf(address(this));
        require(startBalance >= params.inputAmount, "INSUFFICIENT_INPUT_BALANCE");

        emit ExecutionStarted(msg.sender, params.snapshotId, params.inputToken, params.inputAmount, false);
        uint256 endingBalance = _execute(params);
        require(endingBalance >= startBalance, "MIN_PROFIT");
        uint256 realizedProfit = endingBalance - startBalance;
        require(realizedProfit >= params.minProfit, "MIN_PROFIT");

        emit ExecutionFinished(params.snapshotId, params.inputToken, endingBalance, realizedProfit);
    }

    function executeFlashLoan(FlashLoanParams calldata params) external onlyOperator notPaused nonReentrant {
        require(aavePool != address(0), "FLASH_DISABLED");
        require(params.execution.hops.length > 0, "EMPTY_HOPS");
        require(params.execution.inputToken == params.loanAsset, "INPUT_ASSET_MISMATCH");
        require(params.loanAmount > 0, "ZERO_LOAN_AMOUNT");
        require(params.execution.inputAmount >= params.loanAmount, "LOAN_EXCEEDS_INPUT");
        require(block.timestamp <= params.execution.deadline, "DEADLINE_EXPIRED");

        emit ExecutionStarted(
            msg.sender, params.execution.snapshotId, params.loanAsset, params.execution.inputAmount, true
        );
        IAavePool(aavePool)
            .flashLoanSimple(address(this), params.loanAsset, params.loanAmount, abi.encode(params.execution), 0);
    }

    function executeOperation(address asset, uint256 amount, uint256 premium, address initiator, bytes calldata data)
        external
        override
        returns (bool)
    {
        require(msg.sender == aavePool, "NOT_AAVE_POOL");
        require(initiator == address(this), "BAD_INITIATOR");

        ExecutionParams memory execution = abi.decode(data, (ExecutionParams));
        require(execution.inputToken == asset, "FLASH_ASSET_MISMATCH");
        require(execution.inputAmount >= amount, "FLASH_EXCEEDS_INPUT");
        require(block.timestamp <= execution.deadline, "DEADLINE_EXPIRED");

        uint256 startBalance = IERC20(asset).balanceOf(address(this));
        require(startBalance >= execution.inputAmount, "INSUFFICIENT_INPUT_BALANCE");

        uint256 endingBalance = _execute(execution);
        uint256 amountOwed = amount + premium;
        require(endingBalance >= startBalance + premium + execution.minProfit, "MIN_PROFIT");

        _forceApprove(asset, aavePool, 0);
        _forceApprove(asset, aavePool, amountOwed);

        emit ExecutionFinished(
            execution.snapshotId, execution.inputToken, endingBalance, endingBalance - startBalance - premium
        );
        return true;
    }

    function uniswapV3SwapCallback(int256 amount0Delta, int256 amount1Delta, bytes calldata data) external override {
        require(amount0Delta > 0 || amount1Delta > 0, "NO_CALLBACK_PAYMENT");
        _checkTarget(msg.sender);

        V3CallbackData memory callbackData = abi.decode(data, (V3CallbackData));
        uint256 payment = amount0Delta > 0 ? uint256(amount0Delta) : uint256(amount1Delta);
        _safeTransfer(callbackData.tokenIn, msg.sender, payment);
    }

    function _execute(ExecutionParams memory params) internal returns (uint256 endingBalance) {
        address currentToken = params.inputToken;
        uint256 expectedAvailable = params.inputAmount;

        for (uint256 i = 0; i < params.hops.length; ++i) {
            Hop memory hop = params.hops[i];
            require(hop.splits.length > 0, "EMPTY_HOP");

            uint256 configuredInputSum = 0;
            uint256 hopOutputSum = 0;
            address nextToken = hop.splits[0].tokenOut;

            for (uint256 j = 0; j < hop.splits.length; ++j) {
                Split memory split = hop.splits[j];
                require(split.tokenIn == currentToken, "HOP_TOKEN_IN_MISMATCH");
                require(split.tokenOut == nextToken, "HOP_TOKEN_OUT_MISMATCH");
                configuredInputSum += split.amountIn;
            }
            require(configuredInputSum > 0, "ZERO_HOP_INPUT");

            uint256 remainingInput = expectedAvailable;
            for (uint256 j = 0; j < hop.splits.length; ++j) {
                Split memory split = hop.splits[j];
                uint256 originalAmountIn = split.amountIn;

                if (configuredInputSum != expectedAvailable) {
                    if (j + 1 == hop.splits.length) {
                        split.amountIn = remainingInput;
                    } else {
                        split.amountIn = originalAmountIn * expectedAvailable / configuredInputSum;
                        remainingInput -= split.amountIn;
                    }
                    split.minAmountOut = originalAmountIn == 0 ? 0 : split.minAmountOut * split.amountIn / originalAmountIn;
                } else {
                    remainingInput -= split.amountIn;
                }

                if (split.amountIn == 0) {
                    continue;
                }

                uint256 amountOut = _executeSplit(params.snapshotId, split);
                require(amountOut >= split.minAmountOut, "SPLIT_SLIPPAGE");
                hopOutputSum += amountOut;
            }

            require(hopOutputSum > 0, "ZERO_HOP_OUTPUT");
            currentToken = nextToken;
            expectedAvailable = hopOutputSum;
        }

        require(currentToken == params.inputToken, "NOT_CYCLIC");
        endingBalance = IERC20(params.inputToken).balanceOf(address(this));
    }

    function _executeSplit(uint64 snapshotId, Split memory split) internal returns (uint256 amountOut) {
        _checkTarget(split.target);

        if (split.adapterType == AdapterType.UniswapV2Like) {
            amountOut = _swapV2(split);
        } else if (split.adapterType == AdapterType.AerodromeV2Like) {
            amountOut = _swapAerodromeV2(split);
        } else if (split.adapterType == AdapterType.UniswapV3Like) {
            amountOut = _swapV3(split);
        } else if (split.adapterType == AdapterType.CurvePlain) {
            amountOut = _swapCurve(split);
        } else if (split.adapterType == AdapterType.BalancerWeighted) {
            amountOut = _swapBalancer(split);
        } else {
            revert("UNSUPPORTED_ADAPTER");
        }

        emit SplitExecuted(
            snapshotId, split.adapterType, split.target, split.tokenIn, split.tokenOut, split.amountIn, amountOut
        );
    }

    function _swapV2(Split memory split) internal returns (uint256 amountOut) {
        IUniswapV2Pair pair = IUniswapV2Pair(split.target);
        address token0 = pair.token0();
        address token1 = pair.token1();

        bool zeroForOne = false;
        if (split.tokenIn == token0 && split.tokenOut == token1) {
            zeroForOne = true;
        } else if (split.tokenIn == token1 && split.tokenOut == token0) {
            zeroForOne = false;
        } else {
            revert("BAD_V2_TOKEN_PAIR");
        }

        (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast) = pair.getReserves();
        blockTimestampLast;
        uint256 reserveIn = zeroForOne ? uint256(reserve0) : uint256(reserve1);
        uint256 reserveOut = zeroForOne ? uint256(reserve1) : uint256(reserve0);
        uint256 quotedOut = _getAmountOut(split.amountIn, reserveIn, reserveOut, _decodeV2FeePpm(split.extraData));

        uint256 balanceBefore = IERC20(split.tokenOut).balanceOf(address(this));
        _safeTransfer(split.tokenIn, split.target, split.amountIn);
        pair.swap(zeroForOne ? 0 : quotedOut, zeroForOne ? quotedOut : 0, address(this), "");
        amountOut = IERC20(split.tokenOut).balanceOf(address(this)) - balanceBefore;
    }

    function _swapV3(Split memory split) internal returns (uint256 amountOut) {
        IUniswapV3Pool pool = IUniswapV3Pool(split.target);
        address token0 = pool.token0();
        address token1 = pool.token1();

        bool zeroForOne = false;
        if (split.tokenIn == token0 && split.tokenOut == token1) {
            zeroForOne = true;
        } else if (split.tokenIn == token1 && split.tokenOut == token0) {
            zeroForOne = false;
        } else {
            revert("BAD_V3_TOKEN_PAIR");
        }

        uint160 sqrtPriceLimitX96 = _decodeV3PriceLimit(split.extraData, zeroForOne);
        uint256 balanceBefore = IERC20(split.tokenOut).balanceOf(address(this));
        (int256 amount0Delta, int256 amount1Delta) = pool.swap(
            address(this),
            zeroForOne,
            int256(split.amountIn),
            sqrtPriceLimitX96,
            abi.encode(V3CallbackData({tokenIn: split.tokenIn}))
        );
        require(zeroForOne ? amount1Delta < 0 : amount0Delta < 0, "V3_NO_OUTPUT");
        amountOut = IERC20(split.tokenOut).balanceOf(address(this)) - balanceBefore;
    }

    function _swapAerodromeV2(Split memory split) internal returns (uint256 amountOut) {
        IAerodromeV2Pool pool = IAerodromeV2Pool(split.target);
        address token0 = pool.token0();
        address token1 = pool.token1();

        bool zeroForOne = false;
        if (split.tokenIn == token0 && split.tokenOut == token1) {
            zeroForOne = true;
        } else if (split.tokenIn == token1 && split.tokenOut == token0) {
            zeroForOne = false;
        } else {
            revert("BAD_AERO_V2_TOKEN_PAIR");
        }

        uint256 quotedOut = pool.getAmountOut(split.amountIn, split.tokenIn);
        require(quotedOut > 0, "AERO_V2_NO_OUTPUT");

        uint256 balanceBefore = IERC20(split.tokenOut).balanceOf(address(this));
        _safeTransfer(split.tokenIn, split.target, split.amountIn);
        pool.swap(zeroForOne ? 0 : quotedOut, zeroForOne ? quotedOut : 0, address(this), "");
        amountOut = IERC20(split.tokenOut).balanceOf(address(this)) - balanceBefore;
    }

    function _swapCurve(Split memory split) internal returns (uint256 amountOut) {
        (int128 i, int128 j, bool underlying) = _decodeCurveExtra(split.extraData);
        uint256 balanceBefore = IERC20(split.tokenOut).balanceOf(address(this));
        _forceApprove(split.tokenIn, split.target, 0);
        _forceApprove(split.tokenIn, split.target, split.amountIn);

        uint256 returnedAmount = underlying
            ? ICurvePool(split.target).exchange_underlying(i, j, split.amountIn, split.minAmountOut)
            : ICurvePool(split.target).exchange(i, j, split.amountIn, split.minAmountOut);
        require(returnedAmount >= split.minAmountOut, "CURVE_MIN_OUT");

        amountOut = IERC20(split.tokenOut).balanceOf(address(this)) - balanceBefore;
    }

    function _swapBalancer(Split memory split) internal returns (uint256 amountOut) {
        bytes32 poolId = split.extraData.length >= 32
            ? abi.decode(split.extraData, (bytes32))
            : IBalancerPool(split.target).getPoolId();
        address vault = IBalancerPool(split.target).getVault();
        _checkTarget(vault);

        _forceApprove(split.tokenIn, vault, 0);
        _forceApprove(split.tokenIn, vault, split.amountIn);

        uint256 balanceBefore = IERC20(split.tokenOut).balanceOf(address(this));
        uint256 returnedAmount = IBalancerVault(vault)
            .swap(
                IBalancerVault.SingleSwap({
                    poolId: poolId,
                    kind: IBalancerVault.SwapKind.GIVEN_IN,
                    assetIn: split.tokenIn,
                    assetOut: split.tokenOut,
                    amount: split.amountIn,
                    userData: ""
                }),
                IBalancerVault.FundManagement({
                    sender: address(this),
                    fromInternalBalance: false,
                    recipient: payable(address(this)),
                    toInternalBalance: false
                }),
                split.minAmountOut,
                block.timestamp
            );
        require(returnedAmount >= split.minAmountOut, "BALANCER_MIN_OUT");
        amountOut = IERC20(split.tokenOut).balanceOf(address(this)) - balanceBefore;
    }

    function _decodeV3PriceLimit(bytes memory extraData, bool zeroForOne) internal pure returns (uint160) {
        if (extraData.length >= 32) {
            uint256 raw = abi.decode(extraData, (uint256));
            if (raw > 0 && raw <= type(uint160).max) {
                return uint160(raw);
            }
        }
        return zeroForOne ? MIN_SQRT_RATIO_PLUS_ONE : MAX_SQRT_RATIO_MINUS_ONE;
    }

    function _decodeV2FeePpm(bytes memory extraData) internal pure returns (uint256) {
        if (extraData.length >= 32) {
            uint256 feePpm = abi.decode(extraData, (uint256));
            require(feePpm < 1_000_000, "BAD_V2_FEE");
            return feePpm;
        }
        return 3000;
    }

    function _decodeCurveExtra(bytes memory extraData) internal pure returns (int128 i, int128 j, bool underlying) {
        require(extraData.length >= 33, "BAD_CURVE_EXTRA");
        i = _readInt128(extraData, 0);
        j = _readInt128(extraData, 16);
        underlying = uint8(extraData[32]) != 0;
    }

    function _readInt128(bytes memory data, uint256 offset) internal pure returns (int128 value) {
        require(data.length >= offset + 16, "OUT_OF_BOUNDS");
        uint128 raw = 0;
        for (uint256 i = 0; i < 16; ++i) {
            raw = (raw << 8) | uint8(data[offset + i]);
        }
        value = int128(raw);
    }

    function _checkTarget(address target) internal view {
        if (strictTargetAllowlist) {
            require(allowedTargets[target], "TARGET_NOT_ALLOWED");
        }
    }

    function _getAmountOut(uint256 amountIn, uint256 reserveIn, uint256 reserveOut, uint256 feePpm)
        internal
        pure
        returns (uint256)
    {
        require(amountIn > 0, "ZERO_INPUT");
        require(reserveIn > 0 && reserveOut > 0, "BAD_RESERVES");
        uint256 feeDenominator = 1_000_000;
        uint256 amountInWithFee = amountIn * (feeDenominator - feePpm);
        uint256 numerator = amountInWithFee * reserveOut;
        uint256 denominator = reserveIn * feeDenominator + amountInWithFee;
        return numerator / denominator;
    }

    function _safeTransfer(address token, address to, uint256 amount) internal {
        (bool ok, bytes memory data) = token.call(abi.encodeWithSelector(IERC20.transfer.selector, to, amount));
        require(ok && (data.length == 0 || abi.decode(data, (bool))), "TRANSFER_FAILED");
    }

    function _forceApprove(address token, address spender, uint256 amount) internal {
        (bool ok, bytes memory data) = token.call(abi.encodeWithSelector(IERC20.approve.selector, spender, amount));
        require(ok && (data.length == 0 || abi.decode(data, (bool))), "APPROVE_FAILED");
    }
}
