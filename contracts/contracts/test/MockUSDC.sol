// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;
import {ERC20} from "@openzeppelin/contracts/token/ERC20/ERC20.sol";
contract MockUSDC is ERC20 {
    uint8 private immutable _precision;
    constructor(uint8 precision) ERC20("Mock USDC", "USDC") { _precision = precision; }
    function decimals() public view override returns (uint8) { return _precision; }
    function mint(address to, uint256 amount) external { _mint(to, amount); }
}
