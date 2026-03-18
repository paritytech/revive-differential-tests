// SPDX-License-Identifier: MIT

pragma solidity >=0.8.0;

import "./lib.sol";

contract Main {
    function test(uint a, uint b) public pure returns (uint) {
        return MathLib.add(a, b);
    }
}
