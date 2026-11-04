const assert = require('node:assert/strict');
const hre = require('hardhat');
const { FhevmType } = require('@fhevm/hardhat-plugin');

function requireLocalMock() {
  assert.ok(hre.network.name === 'hardhat' && hre.fhevm.isMock, 'local Hardhat FHEVM mock required');
}

async function deployHarness() {
  requireLocalMock();
  const [caller, receiver] = await hre.ethers.getSigners();
  const token = await (await hre.ethers.getContractFactory('ERC7984Harness', caller)).deploy();
  await token.waitForDeployment();
  return { token, caller, receiver };
}

async function encryptedCall(token, signer, method, amount, args = []) {
  const input = await hre.fhevm.createEncryptedInput(await token.getAddress(), signer.address)
    .add64(amount).encrypt();
  const tx = await token.connect(signer)[method](...args, input.handles[0], input.inputProof);
  const receipt = await tx.wait();
  assert.ok(receipt.status === 1, 'encrypted operation succeeded');
}

async function decrypt(token, handle, signer) {
  return hre.fhevm.userDecryptEuint(FhevmType.euint64, handle, await token.getAddress(), signer);
}

async function state(token, holder, caller) {
  return {
    balance: await decrypt(token, await token.confidentialBalanceOf(holder.address), holder),
    supply: await decrypt(token, await token.confidentialTotalSupply(), caller),
    actual: await decrypt(token, await token.lastActual(), caller),
  };
}

// Compare booleans so assertion diagnostics never interpolate decrypted amounts.
function same(actual, expected, relation) {
  assert.ok(actual === expected, relation);
}

module.exports = { deployHarness, encryptedCall, state, same, requireLocalMock };
