// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;
import {CWZECSwap} from "../CWZECSwap.sol";
import {FHE, externalEuint64, euint64, ebool} from "@fhevm/solidity/lib/FHE.sol";
import {ZamaEthereumConfig} from "@fhevm/solidity/config/ZamaConfig.sol";

contract SwapReceiverMock is ZamaEthereumConfig {
    address public immutable token;
    uint8 public mode;
    address public pool;
    constructor(address t) { token = t; }
    function configure(uint8 m, address p) external { mode=m; pool=p; }
    function onConfidentialTransferReceived(address, address, euint64, bytes calldata) external returns(ebool ok) {
        require(msg.sender == token);
        if (mode == 2) revert("forward failed");
        if (mode == 3) {
            (bool success, bytes memory result) = pool.call(abi.encodeCall(CWZECSwap.swap, (externalEuint64.wrap(bytes32(0)),externalEuint64.wrap(bytes32(0)),hex"",type(uint256).max,CWZECSwap.PriceQuote(1,0,1),hex"")));
            require(!success && bytes4(result) == bytes4(keccak256("ReentrancyGuardReentrantCall()")), "wrong reentrancy rejection");
        }
        ok = FHE.asEbool(mode != 1);
        FHE.allowTransient(ok, token);
    }
}
