const { assert, hre, fixture, mint, input, receipt, event, actual, rejects, same, tag } = require('./cwzec-helpers.cjs');
describe('Restricted balancer', function () {
  it('binds contract-caller proofs, receipt callbacks, operations and recredit', async function () {
    const f = await fixture();
    const dest = '0x123456';
    const b = await (await hre.ethers.getContractFactory('CWZECBalancer')).deploy(await f.token.getAddress(), f.service.address, hre.ethers.keccak256(dest));
    const addr = await b.getAddress();
    await mint(f, 100n);
    assert.equal(await b.supportsInterface('0x01ffc9a7'), true);
    assert.equal(await b.supportsInterface('0xffffffff'), false);
    let e = await input(f.token, f.user, 100n);
    await receipt(f.token.connect(f.user)['confidentialTransferAndCall(address,bytes32,bytes,bytes)'](addr, e.handles[0], e.inputProof, '0x'));
    // Bare confidential transfers are supported too (no receiver callback).
    await mint(f, 5n, 'bare');
    e = await input(f.token, f.user, 5n);
    await receipt(f.token.connect(f.user)['confidentialTransfer(address,bytes32,bytes)'](addr, e.handles[0], e.inputProof));
    const bound = async (n) => hre.fhevm.createEncryptedInput(await f.token.getAddress(), addr).add64(n).encrypt();
    e = await bound(40n);
    const call = (signer, id, enc = e, destination = dest) => b.connect(signer).redeem(tag(id), enc.handles[0], enc.inputProof, destination);
    await rejects(() => call(f.outsider, 'one'));
    await rejects(() => call(f.service, 'one', e, '0xab'));
    const wrong = await input(f.token, f.service, 40n);
    await rejects(() => call(f.service, 'wrong', wrong));
    await rejects(() => b.connect(f.outsider).onConfidentialTransferReceived(f.user.address, f.user.address, e.handles[0], '0x'));
    const burned = event(f.token, await receipt(call(f.service, 'one')), 'RedeemRequested');
    assert.equal(burned.user, addr);
    same(await actual(f, burned.amount), 40n, 'actual contract burn');
    await receipt(call(f.service, 'one'));
    assert.equal(await f.token.nextRedeemId(), 2n);
    await rejects(() => call(f.service, 'one', wrong));
    await receipt(f.token.connect(f.minter).reCreditRedeem(1n));
    e = await bound(105n);
    same(await actual(f, event(f.token, await receipt(call(f.service, 'two')), 'RedeemRequested').amount), 105n, 'recredit redeemable by original burner');
    for (const n of [0n, 1n]) {
      e = await bound(n);
      same(await actual(f, event(f.token, await receipt(call(f.service, `empty-${n}`)), 'RedeemRequested').amount), 0n, 'zero/overburn actual zero');
    }
  });
});
