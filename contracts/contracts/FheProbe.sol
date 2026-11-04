// SPDX-License-Identifier: BSD-3-Clause-Clear
pragma solidity 0.8.28;

import {FHE, euint64, externalEuint64} from "@fhevm/solidity/lib/FHE.sol";
import {ZamaEthereumConfig} from "@fhevm/solidity/config/ZamaConfig.sol";

/// @notice Feasibility only: the owner submits one encrypted integer and adds one.
contract FheProbe is ZamaEthereumConfig {
    address public immutable owner = msg.sender;
    euint64 public result;

    error OwnerOnly();

    function submit(externalEuint64 input, bytes calldata proof) external {
        if (msg.sender != owner) revert OwnerOnly();
        result = FHE.add(FHE.fromExternal(input, proof), uint64(1));
        FHE.allowThis(result);
        FHE.allow(result, msg.sender);
    }
}
