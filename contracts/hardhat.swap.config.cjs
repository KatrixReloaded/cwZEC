// Signature verification only, on an in-process Sepolia-ID chain. No FHE plugin,
// RPC, funded exchange, or external network. Compile first with the normal config.
require('@nomicfoundation/hardhat-ethers');
module.exports = { solidity: '0.8.28', networks: { hardhat: { chainId: 11155111 } } };
