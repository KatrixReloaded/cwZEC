const {setup,price,swap,balances,rejects,hre,assert} = require('./swap-helpers.cjs');
describe('Swap signed price policy',function(){
  for(const kind of ['wrong-signer','tampered-price','tampered-observation','tampered-expiry','wrong-chain','wrong-pool','future','expired','overlong','reversed','equal','zero','range','malformed','eth']) it(kind,async function(){
    const f=await setup();
    const {domain,TYPES}=await import('../../price-service/dist/service.js');
    const now=(await hre.ethers.provider.getBlock('latest')).timestamp;
    const cases={future:[5000000000n,now+100],expired:[5000000000n,now-60,now],overlong:[5000000000n,now,now+61],reversed:[5000000000n,now,now-1],equal:[5000000000n,now,now],zero:[0n],range:[100000000000001n]};
    if(cases[kind]) await price(f,...cases[kind]);
    const options={};
    if(kind==='wrong-signer') options.signature=await new hre.ethers.Wallet('0x'+'22'.repeat(32)).signTypedData({...domain(f.addr),chainId:31337},TYPES,f.signed.quote);
    if(kind.startsWith('tampered')) {
      const field={'tampered-price':'priceE8','tampered-observation':'observedAt','tampered-expiry':'expiresAt'}[kind];
      options.quote={...f.signed.quote,[field]:String(BigInt(f.signed.quote[field])-1n)};
    }
    if(kind==='wrong-chain' || kind==='wrong-pool') options.signature=await f.quoteWallet.signTypedData({...domain(f.addr),chainId:31337,...(kind==='wrong-chain'?{chainId:1}:{verifyingContract:f.user.address})},TYPES,f.signed.quote);
    if(kind==='malformed') options.signature='0x1234';
    if(kind==='eth') options.value=1;
    await rejects(()=>swap(f,100000000n,0n,options));
    await balances(f,100000000n,0n,0n,1000000000n);
  });
  it('reuses exactly the same quote for two swaps',async function(){
    const f=await setup(false,1000000000n,200000000n);
    await swap(f); await swap(f);
    await balances(f,0n,99000000n,200000000n,901000000n);
    assert.equal(await hre.ethers.provider.getBalance(f.addr),0n);
  });
  it('matches service domain, uint64 fields and Solidity typed data digest',async function(){
    const f=await setup(); const {domain,TYPES}=await import('../../price-service/dist/service.js');
    const d=await f.pool.eip712Domain();
    assert.equal(d.name,'CWZECSwap'); assert.equal(d.version,'1');
    assert.equal(d.chainId,31337n); assert.equal(d.verifyingContract,f.addr);
    assert.equal(await f.pool.PRICE_QUOTE_TYPEHASH(),hre.ethers.id('PriceQuote(uint64 priceE8,uint64 observedAt,uint64 expiresAt)'));
    assert.equal(hre.ethers.verifyTypedData({...domain(f.addr),chainId:31337},TYPES,f.signed.quote,f.signed.signature),f.quoteWallet.address);
    const [n,dn]=await f.pool.rate(f.signed.quote,f.signed.signature);
    assert.equal(n,5000000000n*9900n); assert.equal(dn,100000000000000n);
  });
  it('accepts a full 60-second lifetime and rejects at the exact expiry second',async function(){
    const f=await setup();
    await f.pool.rate(f.signed.quote,f.signed.signature);
    await hre.network.provider.send('evm_setNextBlockTimestamp',[Number(f.signed.quote.expiresAt)]);
    await hre.network.provider.send('evm_mine');
    await rejects(()=>f.pool.rate(f.signed.quote,f.signed.signature));
  });
  it('rejects a zero signer at construction',async function(){
    const f=await setup(); const factory=await hre.ethers.getContractFactory('CWZECSwap');
    await rejects(()=>factory.deploy(f.token.target,f.cusdc.target,f.balancer.target,hre.ethers.ZeroAddress,100));
  });
});
