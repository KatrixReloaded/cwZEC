// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import {FHE, externalEuint64, euint64, ebool} from "@fhevm/solidity/lib/FHE.sol";
import {ZamaEthereumConfig} from "@fhevm/solidity/config/ZamaConfig.sol";
import {ERC7984} from "@openzeppelin/confidential-contracts/token/ERC7984/ERC7984.sol";
import {FHESafeMath} from "@openzeppelin/confidential-contracts/utils/FHESafeMath.sol";
import {AccessControl} from "@openzeppelin/contracts/access/AccessControl.sol";

/// @notice Confidential bridge token. The trusted minter authenticates Zcash deposits and payouts.
contract CWZEC is ERC7984, AccessControl, ZamaEthereumConfig {
    bytes32 public constant MINTER_ROLE = keccak256("MINTER_ROLE");
    address public immutable service;
    address public immutable auditor;

    enum DepositStatus { None, Minted, Refunded }
    enum RedemptionStatus { None, Pending, Fulfilled, Recredited }
    struct Redemption {
        address user;
        euint64 amount;
        RedemptionStatus status;
        bytes32 zcashTxid;
    }

    mapping(bytes32 => DepositStatus) public consumed;
    mapping(bytes32 => euint64) public mintedAmounts;
    mapping(uint256 => Redemption) private _redemptions;
    uint256 public nextRedeemId = 1;
    // Circulating supply plus actual burns whose redemption is still Pending.
    // Keeping their capacity reserved makes a later recorded-amount recredit exact.
    euint64 private _committedSupply;

    error ZeroRoleAddress();
    error DepositConsumed(bytes32 depositId);
    error UnknownRedemption(uint256 redeemId);
    error RedemptionNotPending(uint256 redeemId);
    error ZeroZcashTxid();

    event DepositMinted(bytes32 indexed depositId, address indexed to, euint64 amount);
    event DepositVoided(bytes32 indexed depositId);
    event RedeemRequested(uint256 indexed redeemId, address indexed user, euint64 amount, bytes destCt);
    event RedeemFulfilled(uint256 indexed redeemId, bytes32 zcashTxid);
    event RedeemRecredited(uint256 indexed redeemId, address indexed user, euint64 amount);

    constructor(address minter_, address service_, address auditor_)
        ERC7984("Confidential Wrapped ZEC", "cWZEC", "")
    {
        require(minter_ != address(0) && service_ != address(0) && auditor_ != address(0), ZeroRoleAddress());
        service = service_;
        auditor = auditor_;
        _grantRole(DEFAULT_ADMIN_ROLE, msg.sender);
        _grantRole(MINTER_ROLE, minter_);
    }

    function decimals() public pure override returns (uint8) { return 8; }

    function supportsInterface(bytes4 id) public view override(ERC7984, AccessControl) returns (bool) {
        return super.supportsInterface(id);
    }

    /// @notice Consumes a deposit even if uint64 capacity makes its actual mint zero.
    /// @dev The operator must reconcile the actual result before discharging the deposit liability.
    function confidentialMint(address to, externalEuint64 amount, bytes calldata proof, bytes32 depositId)
        external onlyRole(MINTER_ROLE) returns (euint64 actual)
    {
        require(consumed[depositId] == DepositStatus.None, DepositConsumed(depositId));
        consumed[depositId] = DepositStatus.Minted;
        euint64 requested = FHE.fromExternal(amount, proof);
        (ebool success, euint64 committed) = FHESafeMath.tryIncrease(_committedSupply, requested);
        _committedSupply = committed;
        FHE.allowThis(committed);
        actual = _mint(to, FHE.select(success, requested, FHE.asEuint64(0)));
        mintedAmounts[depositId] = actual;
        FHE.allow(actual, service);
        emit DepositMinted(depositId, to, actual);
    }

    /// @notice Disables minting for this deposit; does not certify a Zcash refund.
    function voidDeposit(bytes32 depositId) external onlyRole(MINTER_ROLE) {
        require(consumed[depositId] == DepositStatus.None, DepositConsumed(depositId));
        consumed[depositId] = DepositStatus.Refunded;
        emit DepositVoided(depositId);
    }

    function requestRedeem(externalEuint64 amount, bytes calldata proof, bytes calldata destCt)
        external returns (uint256 redeemId)
    {
        euint64 actual = _burn(msg.sender, FHE.fromExternal(amount, proof));
        FHE.allow(actual, service);
        redeemId = nextRedeemId++;
        _redemptions[redeemId] = Redemption(msg.sender, actual, RedemptionStatus.Pending, bytes32(0));
        emit RedeemRequested(redeemId, msg.sender, actual, destCt);
    }

    function redemptions(uint256 redeemId) public view
        returns (address user, euint64 amount, RedemptionStatus status, bytes32 zcashTxid)
    {
        Redemption storage record = _record(redeemId);
        return (record.user, record.amount, record.status, record.zcashTxid);
    }

    /// @notice Records the trusted operator's confirmed payout decision.
    function markRedeemFulfilled(uint256 redeemId, bytes32 zcashTxid) external onlyRole(MINTER_ROLE) {
        Redemption storage record = _pending(redeemId);
        require(zcashTxid != bytes32(0), ZeroZcashTxid());
        record.status = RedemptionStatus.Fulfilled;
        record.zcashTxid = zcashTxid;
        _committedSupply = FHE.sub(_committedSupply, record.amount);
        FHE.allowThis(_committedSupply);
        emit RedeemFulfilled(redeemId, zcashTxid);
    }

    /// @notice Restores the recorded actual burn to its original user exactly once.
    /// @dev The operator must authorize this only if no payout was issued, or every issued
    /// payout is conclusively dead. This contract cannot prove Zcash payout death.
    function reCreditRedeem(uint256 redeemId) external onlyRole(MINTER_ROLE) {
        Redemption storage record = _pending(redeemId);
        record.status = RedemptionStatus.Recredited;
        euint64 actual = _mint(record.user, record.amount);
        emit RedeemRecredited(redeemId, record.user, actual);
    }

    function _record(uint256 redeemId) private view returns (Redemption storage record) {
        record = _redemptions[redeemId];
        require(record.status != RedemptionStatus.None, UnknownRedemption(redeemId));
    }

    function _pending(uint256 redeemId) private view returns (Redemption storage record) {
        record = _record(redeemId);
        require(record.status == RedemptionStatus.Pending, RedemptionNotPending(redeemId));
    }

    function _update(address from, address to, euint64 amount) internal override returns (euint64 actual) {
        actual = super._update(from, to, amount);
        if (from == address(0) || to == address(0)) FHE.allow(confidentialTotalSupply(), auditor);
    }
}
