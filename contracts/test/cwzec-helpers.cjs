const assert = require('node:assert/strict');
const hre = require('hardhat');
const { same, requireLocalMock } = require('./helpers.cjs');
const { FhevmType } = require('@fhevm/hardhat-plugin');
async function decrypt(token, handle, signer) {
  return hre.fhevm.userDecryptEuint(FhevmType.euint64, handle, await token.getAddress(), signer);
}

async function fixture() {
  requireLocalMock();
  const [admin, minter, service, auditor, user, other, outsider] = await hre.ethers.getSigners();
  const factory = await hre.ethers.getContractFactory('CWZEC', admin);
  const token = await factory.deploy(minter.address, service.address, auditor.address);
  await token.waitForDeployment();
  return { token, factory, admin, minter, service, auditor, user, other, outsider };
}
const tag = (label) => hre.ethers.id(label);
async function input(token, signer, value) {
  return hre.fhevm.createEncryptedInput(await token.getAddress(), signer.address).add64(value).encrypt();
}
async function receipt(tx) { return (await tx).wait(); }
function event(token, result, name) {
  const found = result.logs.map((log) => { try { return token.interface.parseLog(log); } catch { return null; } })
    .find((log) => log?.name === name);
  assert.ok(!!found, `${name} emitted`);
  return found.args;
}
async function mint(f, value, label = 'deposit', to = f.user) {
  const encrypted = await input(f.token, f.minter, value);
  const result = await receipt(f.token.connect(f.minter).confidentialMint(
    to.address, encrypted.handles[0], encrypted.inputProof, tag(label)));
  return event(f.token, result, 'DepositMinted');
}
async function redeem(f, value, user = f.user, dest = '0x1234') {
  const encrypted = await input(f.token, user, value);
  const result = await receipt(f.token.connect(user).requestRedeem(encrypted.handles[0], encrypted.inputProof, dest));
  return event(f.token, result, 'RedeemRequested');
}
async function balance(f, user = f.user) {
  return decrypt(f.token, await f.token.confidentialBalanceOf(user.address), user);
}
async function supply(f) { return decrypt(f.token, await f.token.confidentialTotalSupply(), f.auditor); }
async function actual(f, handle, signer = f.service) { return decrypt(f.token, handle, signer); }
async function rejects(action, classification = '') {
  let caught;
  try { await action(); } catch (error) { caught = error; }
  assert.ok(!!caught, 'operation rejected');
  if (classification) {
    let classified = String(caught.message).includes(classification);
    if (!classified && !classification.includes(' ')) {
      const artifact = await hre.artifacts.readArtifact('CWZEC');
      const selector = new hre.ethers.Interface(artifact.abi).getError(classification).selector;
      classified = String(caught.data ?? '').startsWith(selector) || String(caught.message).includes(selector);
    }
    assert.ok(classified, `rejection classified as ${classification}`);
  }
}
async function denied(f, handle, signer = f.outsider) {
  await rejects(() => actual(f, handle, signer), 'is not authorized to user decrypt handle');
}
module.exports = { assert, hre, fixture, tag, input, receipt, event, mint, redeem, balance, supply, actual,
  rejects, denied, same, requireLocalMock };
