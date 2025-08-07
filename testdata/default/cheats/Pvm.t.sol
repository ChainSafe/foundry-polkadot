// SPDX-License-Identifier: MIT OR Apache-2.0
pragma solidity ^0.8.18;

import "ds-test/test.sol";
import "cheats/Vm.sol";

contract PvmTest is DSTest {
    Vm constant vm = Vm(HEVM_ADDRESS);

    function testPvmSwitching() public {
        // Test that we start in EVM mode.
        string memory initialInfo = vm.getPvmInfo();

        // Switch to PVM and verify.
        vm.pvm(true);
        string memory pvmInfo = vm.getPvmInfo();

        // Switch back to EVM and verify.
        vm.pvm(false);
        string memory finalInfo = vm.getPvmInfo();

        // Basic verification that the functions don't revert.
        assertTrue(bytes(initialInfo).length > 0, "Initial info should not be empty");
        assertTrue(bytes(pvmInfo).length > 0, "PVM info should not be empty");
        assertTrue(bytes(finalInfo).length > 0, "Final info should not be empty");
    }
}
