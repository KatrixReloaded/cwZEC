const {ethers}=require('hardhat');
const assert=require('node:assert/strict');
describe('Service → production Solidity verifier, exact Sepolia domain',function(){
  it('accepts real service signature; rejects wrong chain/pool/signer and tampering',async function(){
    const {signQuote,domain,TYPES}=await import('../../price-service/dist/service.js');
    const wallet=new ethers.Wallet('0x'+'11'.repeat(32)); // synthetic only
    const deploy=async(name,...args)=>(await ethers.getContractFactory(name)).deploy(...args);
    // Plain constructor compatibility stubs; no FHE operations or funded exchange here.
    const z=await deploy('MockUSDC',8),u=await deploy('MockUSDC',6);
    const b=await deploy('SwapReceiverMock',z.target);
    const pool=await deploy('CWZECSwap',z.target,u.target,b.target,wallet.address,100);
    const now=(await ethers.provider.getBlock('latest')).timestamp;
    const signed=await signQuote(wallet,pool.target,{priceE8:'5000000000',observedAt:String(now),expiresAt:String(now+60)});
    const d=await pool.eip712Domain();
    assert.equal(d.chainId,11155111n); assert.equal(d.verifyingContract,pool.target);
    assert.equal(d.name,'CWZECSwap'); assert.equal(d.version,'1');
    assert.equal(await pool.PRICE_QUOTE_TYPEHASH(),ethers.id('PriceQuote(uint64 priceE8,uint64 observedAt,uint64 expiresAt)'));
    const [n,den]=await pool.rate(signed.quote,signed.signature);
    assert.equal(n,49500000000000n); assert.equal(den,100000000000000n);
    for(const changed of [{chainId:31337},{verifyingContract:z.target},{name:'Other'},{version:'2'}]) {
      const signature=await wallet.signTypedData({...domain(pool.target),...changed},TYPES,signed.quote);
      await assert.rejects(pool.rate(signed.quote,signature),/InvalidPrice/);
    }
    const wrong=await signQuote(new ethers.Wallet('0x'+'22'.repeat(32)),pool.target,signed.quote);
    await assert.rejects(pool.rate(signed.quote,wrong.signature),/InvalidPrice/);
    await assert.rejects(pool.rate({...signed.quote,priceE8:'5000000001'},signed.signature),/InvalidPrice/);
  });
});
