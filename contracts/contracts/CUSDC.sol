// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {IERC20} from "@openzeppelin/contracts/interfaces/IERC20.sol";
import {ERC7984} from "@openzeppelin/confidential-contracts/token/ERC7984/ERC7984.sol";
import {ERC7984ERC20Wrapper} from "@openzeppelin/confidential-contracts/token/ERC7984/extensions/ERC7984ERC20Wrapper.sol";
import {ZamaEthereumConfig} from "@fhevm/solidity/config/ZamaConfig.sol";

/// @notice Fully backed, non-upgradeable confidential USDC wrapper.
contract CUSDC is ERC7984ERC20Wrapper, ZamaEthereumConfig {
    constructor(IERC20 usdc) ERC7984("Confidential USD Coin", "cUSDC", "") ERC7984ERC20Wrapper(usdc) {}
}
