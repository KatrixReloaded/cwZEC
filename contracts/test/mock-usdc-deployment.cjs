const assert = require('node:assert/strict');
const hre = require('hardhat');

describe('mock USDC deployment allocation', function () {
  it('starts empty, permits arbitrary minting, and leaves a new wrapper empty', async function () {
    const [deployer, other, recipient] = await hre.ethers.getSigners();
    const mock = await (await hre.ethers.getContractFactory('MockUSDC', deployer)).deploy(6);
    assert.equal(await mock.decimals(), 6n);
    assert.equal(await mock.totalSupply(), 0n);
    await (await mock.connect(other).mint(recipient.address, 10_000_000_000n)).wait();
    assert.equal(await mock.balanceOf(recipient.address), 10_000_000_000n);
    assert.equal(await mock.totalSupply(), 10_000_000_000n);
    const wrapper = await (await hre.ethers.getContractFactory('CUSDC')).deploy(await mock.getAddress());
    assert.equal(await wrapper.underlying(), await mock.getAddress());
    assert.equal(await wrapper.decimals(), 6n);
    assert.equal(await wrapper.rate(), 1n);
    assert.equal(await mock.balanceOf(await wrapper.getAddress()), 0n);
    assert.equal(await wrapper.inferredTotalSupply(), 0n);
    assert.equal(await wrapper.confidentialTotalSupply(), hre.ethers.ZeroHash);
    assert.equal(await mock.balanceOf(recipient.address), 10_000_000_000n);
  });
});
