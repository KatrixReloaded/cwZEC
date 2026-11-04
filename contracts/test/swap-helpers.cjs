const h = require('./cwzec-helpers.cjs');
const { FhevmType } = require('@fhevm/hardhat-plugin');
async function setup(receiver = false, liquidity = 1000000000n, funds = 100000000n, fee = 100) {
  const f = await h.fixture();
  const deploy = async (name, ...args) => (await h.hre.ethers.getContractFactory(name)).deploy(...args);
  f.usdc = await deploy('MockUSDC', 6);
  f.cusdc = await deploy('CUSDC', await f.usdc.getAddress());
  f.balancer = receiver ? await deploy('SwapReceiverMock', await f.token.getAddress()) : await deploy('CWZECBalancer', await f.token.getAddress(), f.service.address, h.tag('destination'));
  f.quoteWallet = new h.hre.ethers.Wallet('0x'+'11'.repeat(32));
  f.pool = await deploy('CWZECSwap', await f.token.getAddress(), await f.cusdc.getAddress(), await f.balancer.getAddress(), f.quoteWallet.address, fee);
  f.addr = await f.pool.getAddress();
  await h.receipt(f.usdc.mint(f.admin.address, liquidity));
  await h.receipt(f.usdc.approve(await f.cusdc.getAddress(), liquidity));
  await h.receipt(f.cusdc.wrap(f.addr, liquidity));
  await h.mint(f, funds);
  await h.receipt(f.token.connect(f.user).setOperator(f.addr, 2n**48n-1n));
  await price(f);
  return f;
}
async function price(f, p = 5000000000n, time, expiry) {
  const {domain,TYPES} = await import('../../price-service/dist/service.js');
  time ??= (await h.hre.ethers.provider.getBlock('latest')).timestamp;
  const quote = {priceE8:String(p), observedAt:String(time), expiresAt:String(expiry ?? BigInt(time)+60n)};
  // The pinned FHE mock only supports 31337. Exact production signing is tested separately.
  const localDomain = {...domain(f.addr), chainId:(await h.hre.ethers.provider.getNetwork()).chainId};
  f.signed = {quote, signature:await f.quoteWallet.signTypedData(localDomain,TYPES,quote)};
}
async function enc(f, n, min = 0n, signer=f.user) { return h.hre.fhevm.createEncryptedInput(f.addr, signer.address).add64(n).add64(min).encrypt(); }
async function swap(f, n=100000000n, min=0n, options={}) {
  const e = options.e || await enc(f,n,min);
  const deadline = options.deadline ?? (await h.hre.ethers.provider.getBlock('latest')).timestamp+50;
  return h.receipt(f.pool.connect(f.user).swap(e.handles[0],e.handles[1],e.inputProof,deadline, options.quote || f.signed.quote, options.signature ?? f.signed.signature,{value:options.value ?? 0}));
}
async function balances(f, zSeller, uSeller, zBalancer, uPool) {
  const read = async(t,a,s) => { const v=await t.confidentialBalanceOf(a); return v===h.hre.ethers.ZeroHash ? 0n : h.hre.fhevm.userDecryptEuint(FhevmType.euint64,v,await t.getAddress(),s); };
  h.same(await read(f.token,f.user.address,f.user), zSeller, 'seller input');
  h.same(await read(f.cusdc,f.user.address,f.user), uSeller, 'seller payment');
  // Contract holders cannot sign user decryption: mock-only debug decryption for assertions.
  const debug = async(t,a) => { const v=await t.confidentialBalanceOf(a); return v===h.hre.ethers.ZeroHash ? 0n : h.hre.fhevm.debugger.decryptEuint(FhevmType.euint64,v); };
  h.same(await debug(f.token,await f.balancer.getAddress()),zBalancer,'balancer input');
  h.same(await debug(f.token,f.addr),0n,'no pool input residue');
  h.same(await debug(f.cusdc,f.addr),uPool,'pool output reserve');
  h.same(await debug(f.cusdc,await f.balancer.getAddress()),0n,'no balancer output leakage');
}
module.exports={...h,setup,price,enc,swap,balances};
