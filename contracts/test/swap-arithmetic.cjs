const {setup,price,swap,balances,assert} = require('./swap-helpers.cjs');
const MAX=(1n<<64n)-1n;
describe('Swap bounded arithmetic',function(){
  for(const [name,input,p,paid] of [
    ['round down',100000001n,5000000001n,49500000n],
    ['dust rejects',1n,1n,0n],
    ['128-bit product before narrowing',MAX,100000000000000n,0n],
    ['largest input small price',MAX,1n,MAX*9900n/100000000000000n],
    ['uint64 output boundary',MAX,10000000000n,MAX*9900n/10000n],
  ]) it(name,async function(){
    const f=await setup(false,MAX,input); await price(f,p); await swap(f,input);
    await balances(f,paid?0n:input,paid,paid?input:0n,MAX-paid);
  });
  it('accepts exactly uint64 max output and rejects max plus one before narrowing',async function(){
    let f=await setup(false,MAX,MAX,0); await price(f,10000000000n); await swap(f,MAX);
    await balances(f,0n,MAX,MAX,0n);
    f=await setup(false,MAX,1n<<63n,0); await price(f,20000000000n); await swap(f,1n<<63n);
    await balances(f,1n<<63n,0n,0n,MAX);
  });
  it('fixed E8 formula equals the previous exponent conversion for representable prices',function(){
    for(const [p,e] of [[5000000000n,-8],[12345678900n,-10],[123456n,-4],[1n,0]]) {
      const scaled=p*100000000n/(10n**BigInt(-e));
      for(const input of [1n,100000001n,MAX]) {
        const previous=input*p*9900n/(100n*10000n*10n**BigInt(-e));
        assert.equal(input*scaled*9900n/100000000000000n,previous);
      }
    }
  });
  it('rational rate includes decimals and fee without early rounding',async function(){
    const f=await setup(); const [n,d]=await f.pool.rate(f.signed.quote,f.signed.signature);
    assert.equal(n,5000000000n*9900n); assert.equal(d,100000000000000n);
  });
});
