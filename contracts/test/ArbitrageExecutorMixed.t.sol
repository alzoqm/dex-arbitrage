// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {ArbitrageExecutor} from "../ArbitrageExecutor.sol";
import {IFlashLoanSimpleReceiver} from "../interfaces/IFlashLoanSimpleReceiver.sol";

contract MockERC20 {
    string public symbol;
    uint8 public decimals;
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    constructor(string memory symbol_, uint8 decimals_) {
        symbol = symbol_;
        decimals = decimals_;
    }

    function mint(address to, uint256 amount) external {
        balanceOf[to] += amount;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        return true;
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        require(balanceOf[msg.sender] >= amount, "BALANCE");
        balanceOf[msg.sender] -= amount;
        balanceOf[to] += amount;
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        require(balanceOf[from] >= amount, "BALANCE");
        uint256 allowed = allowance[from][msg.sender];
        require(allowed >= amount, "ALLOWANCE");
        allowance[from][msg.sender] = allowed - amount;
        balanceOf[from] -= amount;
        balanceOf[to] += amount;
        return true;
    }
}

contract MockAavePool {
    uint128 public constant FLASH_PREMIUM_BPS = 9;

    function flashLoanSimple(address receiverAddress, address asset, uint256 amount, bytes calldata params, uint16)
        external
    {
        uint256 premium = amount * FLASH_PREMIUM_BPS / 10_000;
        require(MockERC20(asset).transfer(receiverAddress, amount), "LOAN_TRANSFER");
        bool ok =
            IFlashLoanSimpleReceiver(receiverAddress).executeOperation(asset, amount, premium, receiverAddress, params);
        require(ok, "CALLBACK");
        require(MockERC20(asset).transferFrom(receiverAddress, address(this), amount + premium), "REPAY");
    }
}

contract MockV2Pair {
    address public token0;
    address public token1;
    uint112 private reserve0;
    uint112 private reserve1;

    constructor(address token0_, address token1_, uint112 reserve0_, uint112 reserve1_) {
        token0 = token0_;
        token1 = token1_;
        reserve0 = reserve0_;
        reserve1 = reserve1_;
    }

    function getReserves() external view returns (uint112, uint112, uint32) {
        return (reserve0, reserve1, 0);
    }

    function swap(uint256 amount0Out, uint256 amount1Out, address to, bytes calldata) external {
        if (amount0Out > 0) {
            require(MockERC20(token0).transfer(to, amount0Out), "TOKEN0_TRANSFER");
        }
        if (amount1Out > 0) {
            require(MockERC20(token1).transfer(to, amount1Out), "TOKEN1_TRANSFER");
        }
    }
}

contract ArbitrageExecutorMixedTest {
    MockERC20 private tokenA;
    MockERC20 private tokenB;
    MockAavePool private aave;
    ArbitrageExecutor private executor;
    MockV2Pair private pairAb;
    MockV2Pair private pairBa;

    function setUp() public {
        tokenA = new MockERC20("A", 6);
        tokenB = new MockERC20("B", 6);
        aave = new MockAavePool();
        executor = new ArbitrageExecutor(address(aave), address(this));

        pairAb = new MockV2Pair(address(tokenA), address(tokenB), 1_000_000_000, 2_000_000_000);
        pairBa = new MockV2Pair(address(tokenB), address(tokenA), 1_000_000_000, 700_000_000);

        tokenA.mint(address(aave), 10_000_000);
        tokenB.mint(address(pairAb), 2_000_000_000);
        tokenA.mint(address(pairBa), 700_000_000);
    }

    function testPartialFlashLoanUsesOnlyShortfallAndKeepsOwnBalance() public {
        setUp();
        tokenA.mint(address(executor), 400_000);

        uint256 inputAmount = 1_000_000;
        uint256 loanAmount = 600_000;
        uint256 minProfit = 100_000;

        ArbitrageExecutor.FlashLoanParams memory params = _flashParams(inputAmount, loanAmount, minProfit);
        executor.executeFlashLoan(params);

        uint256 premium = loanAmount * 9 / 10_000;
        assert(tokenA.balanceOf(address(aave)) == 10_000_000 + premium);
        assert(tokenA.balanceOf(address(executor)) >= 400_000 + minProfit);
    }

    function testFullFlashLoanStillWorks() public {
        setUp();

        uint256 inputAmount = 1_000_000;
        ArbitrageExecutor.FlashLoanParams memory params = _flashParams(inputAmount, inputAmount, 1);
        executor.executeFlashLoan(params);

        assert(tokenA.balanceOf(address(executor)) > 1);
    }

    function testPartialFlashLoanRevertsWhenOwnBalanceDoesNotCoverShortfall() public {
        setUp();
        tokenA.mint(address(executor), 100_000);

        uint256 inputAmount = 1_000_000;
        uint256 loanAmount = 600_000;

        try executor.executeFlashLoan(_flashParams(inputAmount, loanAmount, 1)) {
            assert(false);
        } catch {}
    }

    function testPartialFlashLoanRevertsWhenMinProfitIsTooHigh() public {
        setUp();
        tokenA.mint(address(executor), 400_000);

        uint256 inputAmount = 1_000_000;
        uint256 loanAmount = 600_000;

        try executor.executeFlashLoan(_flashParams(inputAmount, loanAmount, 10_000_000)) {
            assert(false);
        } catch {}
    }

    function testV2FeePpmExtraDataControlsExecutionQuote() public {
        setUp();
        tokenA.mint(address(executor), 400_000);

        uint256 inputAmount = 1_000_000;
        uint256 loanAmount = 600_000;
        executor.executeFlashLoan(_flashParamsWithFee(inputAmount, loanAmount, 1, 2500));

        assert(tokenA.balanceOf(address(executor)) > 400_000);
    }

    function _flashParams(uint256 inputAmount, uint256 loanAmount, uint256 minProfit)
        private
        view
        returns (ArbitrageExecutor.FlashLoanParams memory)
    {
        return _flashParamsWithFee(inputAmount, loanAmount, minProfit, 3000);
    }

    function _flashParamsWithFee(uint256 inputAmount, uint256 loanAmount, uint256 minProfit, uint256 feePpm)
        private
        view
        returns (ArbitrageExecutor.FlashLoanParams memory)
    {
        ArbitrageExecutor.ExecutionParams memory execution;
        execution.inputToken = address(tokenA);
        execution.inputAmount = inputAmount;
        execution.minProfit = minProfit;
        execution.deadline = block.timestamp + 1;
        execution.snapshotId = 1;
        execution.hops = new ArbitrageExecutor.Hop[](2);

        uint256 firstOut = _amountOut(inputAmount, 1_000_000_000, 2_000_000_000, feePpm);
        uint256 secondOut = _amountOut(firstOut, 1_000_000_000, 700_000_000, feePpm);

        execution.hops[0].splits = new ArbitrageExecutor.Split[](1);
        execution.hops[0].splits[0] = ArbitrageExecutor.Split({
            adapterType: ArbitrageExecutor.AdapterType.UniswapV2Like,
            target: address(pairAb),
            tokenIn: address(tokenA),
            tokenOut: address(tokenB),
            amountIn: inputAmount,
            minAmountOut: firstOut,
            extraData: abi.encode(feePpm)
        });

        execution.hops[1].splits = new ArbitrageExecutor.Split[](1);
        execution.hops[1].splits[0] = ArbitrageExecutor.Split({
            adapterType: ArbitrageExecutor.AdapterType.UniswapV2Like,
            target: address(pairBa),
            tokenIn: address(tokenB),
            tokenOut: address(tokenA),
            amountIn: firstOut,
            minAmountOut: secondOut,
            extraData: abi.encode(feePpm)
        });

        return
            ArbitrageExecutor.FlashLoanParams({
                loanAsset: address(tokenA), loanAmount: loanAmount, execution: execution
            });
    }

    function _amountOut(uint256 amountIn, uint256 reserveIn, uint256 reserveOut, uint256 feePpm)
        private
        pure
        returns (uint256)
    {
        uint256 amountInWithFee = amountIn * (1_000_000 - feePpm);
        return amountInWithFee * reserveOut / (reserveIn * 1_000_000 + amountInWithFee);
    }
}
