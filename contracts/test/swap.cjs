const {setup,price,enc,swap,balances,receipt,rejects,hre,assert} = require('./swap-helpers.cjs');
describe('Confidential one-way swap', function() {
  it('exchanges actual input, exact units and full balances', async function() {
    const f=await setup(); await swap(f,100000000n,49500000n);
    await balances(f,0n,49500000n,100000000n,950500000n);
  });
  it('rejects corrupted proofs, direct transfer callbacks and missing operator permission',async function(){
    const f=await setup();
    const e=await enc(f,100000000n);
    await rejects(()=>swap(f,0n,0n,{e:{...e,inputProof:'0x1234'}}));
    const z=await hre.fhevm.createEncryptedInput(await f.token.getAddress(),f.user.address).add64(100000000n).encrypt();
    await rejects(()=>f.token.connect(f.user)['confidentialTransferAndCall(address,bytes32,bytes,bytes)'](f.addr,z.handles[0],z.inputProof,'0x'));
    await receipt(f.token.connect(f.user).setOperator(f.addr,0));
    await rejects(()=>swap(f));
    await balances(f,100000000n,0n,0n,1000000000n);
  });
  for (const reason of ['funds','zero','liquidity','minimum','deadline','proof','callback','forward-refund','forward-revert','reentrancy']) {
    it(reason, async function() {
      const f=await setup(reason.startsWith('forward') || reason==='reentrancy',reason==='liquidity'?1n:1000000000n);
      if(reason.startsWith('forward') || reason==='reentrancy') await receipt(f.balancer.configure(reason==='forward-refund'?1:reason==='forward-revert'?2:3,f.addr));
      if(reason==='deadline') await rejects(()=>swap(f,100000000n,0n,{deadline:0}));
      else if(reason==='proof') await rejects(async()=>swap(f,0n,0n,{e:await enc(f,100000000n,0n,f.other)}));
      else if(reason==='callback') await rejects(()=>f.pool.onConfidentialTransferReceived(f.addr,f.user.address,hre.ethers.ZeroHash,'0x'));
      else if(reason==='forward-revert') await rejects(()=>swap(f));
      else await swap(f,reason==='funds'?100000001n:reason==='zero'?0n:100000000n,reason==='minimum'?49500001n:0n);
      if(reason==='reentrancy') await balances(f,0n,49500000n,100000000n,950500000n);
      else await balances(f,100000000n,0n,0n,reason==='liquidity'?1n:1000000000n);
    });
  }
});
