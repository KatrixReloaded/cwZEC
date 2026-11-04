//! The bounded CWZEC MVP ABI. All encrypted uint64 values are opaque bytes32 handles.
use alloy::sol;
sol! {
    function confidentialMint(address to, bytes32 amount, bytes proof, bytes32 depositId) external returns (bytes32 actual);
    function voidDeposit(bytes32 depositId) external;
    function requestRedeem(bytes32 amount, bytes proof, bytes destCt) external returns (uint256 redeemId);
    function markRedeemFulfilled(uint256 redeemId, bytes32 zcashTxid) external;
    function reCreditRedeem(uint256 redeemId) external;
    function consumed(bytes32 depositId) external view returns (uint8);
    function mintedAmounts(bytes32 depositId) external view returns (bytes32);
    function redemptions(uint256 redeemId) external view returns (address user, bytes32 amount, uint8 status, bytes32 zcashTxid);
    function confidentialBalanceOf(address account) external view returns (bytes32);
    function confidentialTotalSupply() external view returns (bytes32);
    function MINTER_ROLE() external view returns (bytes32);
    function DEFAULT_ADMIN_ROLE() external view returns (bytes32);
    function hasRole(bytes32 role, address account) external view returns (bool);
    function getRoleAdmin(bytes32 role) external view returns (bytes32);
    function service() external view returns (address);
    function auditor() external view returns (address);
    function nextRedeemId() external view returns (uint256);
    function decimals() external view returns (uint8);
    event DepositMinted(bytes32 indexed depositId, address indexed to, bytes32 amount);
    event DepositVoided(bytes32 indexed depositId);
    event RedeemRequested(uint256 indexed redeemId, address indexed user, bytes32 amount, bytes destCt);
    event RedeemFulfilled(uint256 indexed redeemId, bytes32 zcashTxid);
    event RedeemRecredited(uint256 indexed redeemId, address indexed user, bytes32 amount);
    event ConfidentialTransfer(address indexed from, address indexed to, bytes32 indexed amount);
}
