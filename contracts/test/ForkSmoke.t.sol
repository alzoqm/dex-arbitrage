// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

interface Vm {
    function createSelectFork(string calldata urlOrAlias) external returns (uint256 forkId);
    function envOr(string calldata name, string calldata defaultValue) external returns (string memory value);
    function envOr(string calldata name, address defaultValue) external returns (address value);
}

contract ArbitrageExecutorForkSmokeTest {
    Vm private constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

    function testBaseConfiguredContractsHaveCodeWhenForkEnvSet() public {
        string memory forkUrl = vm.envOr("BASE_FORK_RPC_URL", string(""));
        if (bytes(forkUrl).length == 0) {
            return;
        }

        vm.createSelectFork(forkUrl);
        assertCodeIfConfigured(vm.envOr("BASE_EXECUTOR_ADDRESS", address(0)), "BASE_EXECUTOR_ADDRESS");
        assertCodeIfConfigured(vm.envOr("BASE_AAVE_POOL", address(0)), "BASE_AAVE_POOL");
        assertCodeIfConfigured(vm.envOr("BASE_USDC", address(0)), "BASE_USDC");
        assertCodeIfConfigured(vm.envOr("BASE_WETH", address(0)), "BASE_WETH");
    }

    function testPolygonConfiguredContractsHaveCodeWhenForkEnvSet() public {
        string memory forkUrl = vm.envOr("POLYGON_FORK_RPC_URL", string(""));
        if (bytes(forkUrl).length == 0) {
            return;
        }

        vm.createSelectFork(forkUrl);
        assertCodeIfConfigured(vm.envOr("POLYGON_EXECUTOR_ADDRESS", address(0)), "POLYGON_EXECUTOR_ADDRESS");
        assertCodeIfConfigured(vm.envOr("POLYGON_AAVE_POOL", address(0)), "POLYGON_AAVE_POOL");
        assertCodeIfConfigured(vm.envOr("POLYGON_USDC", address(0)), "POLYGON_USDC");
        assertCodeIfConfigured(vm.envOr("POLYGON_WMATIC", address(0)), "POLYGON_WMATIC");
    }

    function assertCodeIfConfigured(address target, string memory name) private view {
        if (target == address(0)) {
            return;
        }
        require(target.code.length > 0, name);
    }
}
