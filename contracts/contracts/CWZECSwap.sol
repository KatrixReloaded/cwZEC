// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {FHE, externalEuint64, euint64, euint128, ebool} from "@fhevm/solidity/lib/FHE.sol";
import {ZamaEthereumConfig} from "@fhevm/solidity/config/ZamaConfig.sol";
import {IERC7984} from "@openzeppelin/confidential-contracts/interfaces/IERC7984.sol";
import {IERC7984Receiver} from "@openzeppelin/confidential-contracts/interfaces/IERC7984Receiver.sol";
import {EIP712} from "@openzeppelin/contracts/utils/cryptography/EIP712.sol";
import {ECDSA} from "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";
import {ReentrancyGuard} from "@openzeppelin/contracts/utils/ReentrancyGuard.sol";

interface ISwapBalancer { function token() external view returns (address); }

/// @notice One-way inventory swap. Testnet assumption: one USDC is one USD.
/// @dev Requires the audited, unmodified pinned ERC7984 tokens and passive balancer.
contract CWZECSwap is IERC7984Receiver, ZamaEthereumConfig, ReentrancyGuard, EIP712 {
    struct PriceQuote { uint64 priceE8; uint64 observedAt; uint64 expiresAt; }
    bytes32 public constant PRICE_QUOTE_TYPEHASH = keccak256("PriceQuote(uint64 priceE8,uint64 observedAt,uint64 expiresAt)");
    uint64 public constant MAX_PRICE_E8 = 1e14;
    address public immutable quoteSigner;
    IERC7984 public immutable cwzec;
    IERC7984 public immutable cusdc;
    address public immutable balancer;
    uint16 public immutable feeBps;
    address private seller;
    euint64 private minimum;
    uint128 private numerator;
    uint128 private denominator;
    bool private awaitingCallback;
    error InvalidConfiguration();
    error InvalidPrice();
    error Expired();
    error UnauthorizedCallback();

    constructor(address z, address u, address b, address signer, uint16 fee) EIP712("CWZECSwap", "1") {
        require(z != u && z.code.length != 0 && u.code.length != 0 && b.code.length != 0 && signer != address(0), InvalidConfiguration());
        require(IERC7984(z).decimals() == 8 && IERC7984(u).decimals() == 6 && ISwapBalancer(b).token() == z && fee <= 1000, InvalidConfiguration());
        cwzec = IERC7984(z); cusdc = IERC7984(u); balancer = b;
        quoteSigner = signer; feeBps = fee;
    }

    /// @dev Both encrypted inputs bind (this contract, msg.sender). Requires cwZEC operator permission.
    function swap(externalEuint64 amount, externalEuint64 minOut, bytes calldata proof, uint256 deadline, PriceQuote calldata quote, bytes calldata signature)
        external nonReentrant
    {
        require(block.timestamp <= deadline, Expired());
        (numerator, denominator) = rate(quote, signature);
        seller = msg.sender;
        minimum = FHE.fromExternal(minOut, proof);
        euint64 requested = FHE.fromExternal(amount, proof);
        FHE.allowTransient(requested, address(cwzec));
        awaitingCallback = true;
        cwzec.confidentialTransferFromAndCall(msg.sender, address(this), requested, "");
        require(!awaitingCallback, UnauthorizedCallback());
        seller = address(0);
        minimum = euint64.wrap(bytes32(0));
    }

    /// @notice Verify reusable public market data and return the after-fee base-unit rate.
    function rate(PriceQuote calldata q, bytes calldata signature) public view returns (uint128 n, uint128 d) {
        require(q.priceE8 > 0 && q.priceE8 <= MAX_PRICE_E8, InvalidPrice());
        require(q.observedAt <= block.timestamp && q.expiresAt > q.observedAt &&
            q.expiresAt - q.observedAt <= 60 && block.timestamp < q.expiresAt, InvalidPrice());
        bytes32 digest = _hashTypedDataV4(keccak256(abi.encode(PRICE_QUOTE_TYPEHASH, q.priceE8, q.observedAt, q.expiresAt)));
        require(ECDSA.recover(digest, signature) == quoteSigner, InvalidPrice());
        n = uint128(q.priceE8) * (10000 - feeBps);
        d = 1e14;
    }

    function onConfidentialTransferReceived(address operator, address from, euint64 received, bytes calldata)
        external returns (ebool accepted)
    {
        require(msg.sender == address(cwzec) && awaitingCallback && operator == address(this) && from == seller, UnauthorizedCallback());
        require(FHE.isAllowed(received, address(this)), UnauthorizedCallback());
        awaitingCallback = false;
        // (2^64-1)*1e18 < 2^124, hence no 128-bit wrap, even for rejected inputs.
        euint128 wide = FHE.div(FHE.mul(FHE.asEuint128(received), numerator), denominator);
        ebool fits = FHE.le(wide, uint128(type(uint64).max));
        euint64 output = FHE.asEuint64(FHE.select(fits, wide, FHE.asEuint128(0)));
        accepted = FHE.and(fits, FHE.and(FHE.gt(received, 0), FHE.gt(output, 0)));
        accepted = FHE.and(accepted, FHE.ge(output, minimum));
        accepted = FHE.and(accepted, FHE.le(output, cusdc.confidentialBalanceOf(address(this))));
        euint64 forward = FHE.select(accepted, received, FHE.asEuint64(0));
        FHE.allowTransient(forward, address(cwzec));
        euint64 moved = cwzec.confidentialTransferAndCall(balancer, forward, "");
        accepted = FHE.and(accepted, FHE.eq(moved, received));
        // Pinned cUSDC has no transfer hooks; its balance cannot change between check and payment.
        // Its supply ceiling also proves recipient addition and all refunds fit uint64.
        euint64 payment = FHE.select(accepted, output, FHE.asEuint64(0));
        FHE.allowTransient(payment, address(cusdc));
        cusdc.confidentialTransfer(seller, payment);
        FHE.allowTransient(accepted, address(cwzec));
    }
}
