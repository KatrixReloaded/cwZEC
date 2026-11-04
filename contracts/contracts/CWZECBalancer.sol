// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {FHE, externalEuint64, euint64, ebool} from "@fhevm/solidity/lib/FHE.sol";
import {ZamaEthereumConfig} from "@fhevm/solidity/config/ZamaConfig.sol";
import {IERC7984Receiver} from "@openzeppelin/confidential-contracts/interfaces/IERC7984Receiver.sol";
import {ERC165} from "@openzeppelin/contracts/utils/introspection/ERC165.sol";

interface ICWZECRedemption {
    function requestRedeem(externalEuint64 amount, bytes calldata proof, bytes calldata destination) external returns (uint256);
}

/// @notice Holds recycling inventory; the service can only redeem to the pinned envelope.
/// @dev Setup must authenticate the envelope plaintext and its chain/token/balancer binding.
contract CWZECBalancer is IERC7984Receiver, ERC165, ZamaEthereumConfig {
    address public immutable token;
    address public immutable service;
    bytes32 public immutable destinationHash;
    struct Operation { bytes32 terms; uint256 redeemId; }
    mapping(bytes32 => Operation) public operations;
    error Unauthorized();
    error InvalidConfiguration();
    error DestinationMismatch();
    error OperationConflict();
    event RedemptionRequested(bytes32 indexed operationId, uint256 indexed redeemId);

    constructor(address token_, address service_, bytes32 destinationHash_) {
        require(token_.code.length != 0 && service_ != address(0) && destinationHash_ != bytes32(0), InvalidConfiguration());
        token = token_;
        service = service_;
        destinationHash = destinationHash_;
    }

    /// @dev Input proof must bind (token, address(this)), not (token, service).
    /// The returned ID identifies a request; only the bridge's actual encrypted burn is authoritative.
    function redeem(bytes32 operationId, externalEuint64 amount, bytes calldata proof, bytes calldata destination)
        external returns (uint256)
    {
        require(msg.sender == service, Unauthorized());
        require(keccak256(destination) == destinationHash, DestinationMismatch());
        bytes32 terms = keccak256(abi.encode(amount, proof, destination));
        Operation storage operation = operations[operationId];
        if (operation.redeemId != 0) {
            require(operation.terms == terms, OperationConflict());
            return operation.redeemId;
        }
        operation.terms = terms;
        uint256 id = ICWZECRedemption(token).requestRedeem(amount, proof, destination);
        operation.redeemId = id;
        emit RedemptionRequested(operationId, id);
        return id;
    }

    function onConfidentialTransferReceived(address, address, euint64 amount, bytes calldata)
        external returns (ebool accepted)
    {
        require(msg.sender == token, Unauthorized());
        require(FHE.isAllowed(amount, address(this)), Unauthorized());
        accepted = FHE.asEbool(true);
        FHE.allowTransient(accepted, token);
    }

    function supportsInterface(bytes4 id) public view override returns (bool) {
        return id == type(IERC7984Receiver).interfaceId || super.supportsInterface(id);
    }
}
