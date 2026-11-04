// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {ERC7984} from "@openzeppelin/confidential-contracts/token/ERC7984/ERC7984.sol";
import {FHE, euint64, externalEuint64} from "@fhevm/solidity/lib/FHE.sol";
import {ZamaEthereumConfig} from "@fhevm/solidity/config/ZamaConfig.sol";

/// @dev Local arithmetic probe only. Deliberately unrestricted; never deploy as a token.
contract ERC7984Harness is ERC7984, ZamaEthereumConfig {
    euint64 public lastActual;

    constructor() ERC7984("Arithmetic probe", "PROBE", "") {}

    function mint(address to, externalEuint64 input, bytes calldata proof) external {
        _record(_mint(to, FHE.fromExternal(input, proof)));
    }

    function burn(externalEuint64 input, bytes calldata proof) external {
        _record(_burn(msg.sender, FHE.fromExternal(input, proof)));
    }

    function _record(euint64 actual) private {
        lastActual = actual;
        FHE.allowThis(actual);
        FHE.allow(actual, msg.sender);
        FHE.allow(confidentialTotalSupply(), msg.sender);
    }
}
