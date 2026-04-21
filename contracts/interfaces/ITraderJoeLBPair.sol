// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

interface ITraderJoeLBPair {
    function getTokenX() external view returns (address);
    function getTokenY() external view returns (address);
    function getBinStep() external view returns (uint16);
    function getReserves() external view returns (uint128 reserveX, uint128 reserveY);
    function getSwapOut(uint128 amountIn, bool swapForY)
        external
        view
        returns (uint128 amountInLeft, uint128 amountOut, uint128 fee);
    function swap(bool swapForY, address to) external returns (bytes32 amountsOut);
}
