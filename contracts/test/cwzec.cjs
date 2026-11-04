const { assert, hre, fixture, tag, input, receipt, event, mint, redeem, balance, supply, actual,
  rejects, denied, same, requireLocalMock } = require('./cwzec-helpers.cjs');
const MAX = (1n << 64n) - 1n;

describe('CWZEC bounded bridge lifecycle (local mock)', function () {
  before(requireLocalMock);

  it('sets metadata, distinct roles, immutable readers and initial ID', async function () {
    const f = await fixture();
    same(await f.token.name(), 'Confidential Wrapped ZEC', 'name');
    same(await f.token.symbol(), 'cWZEC', 'symbol');
    same(await f.token.decimals(), 8n, 'zatoshi decimals');
    same(await f.token.contractURI(), '', 'empty URI');
    same(await f.token.service(), f.service.address, 'service');
    same(await f.token.auditor(), f.auditor.address, 'auditor');
    same(await f.token.nextRedeemId(), 1n, 'first ID');
    assert.ok(await f.token.hasRole(await f.token.DEFAULT_ADMIN_ROLE(), f.admin.address), 'deployer admin');
    assert.ok(await f.token.hasRole(await f.token.MINTER_ROLE(), f.minter.address), 'minter role');
    assert.ok(await f.token.supportsInterface('0x01ffc9a7'), 'ERC165 supported');
    assert.ok(await f.token.supportsInterface('0x7965db0b'), 'AccessControl supported');
  });

  it('rejects every zero constructor role', async function () {
    const f = await fixture();
    for (let i = 0; i < 3; i++) {
      const addresses = [f.minter.address, f.service.address, f.auditor.address];
      addresses[i] = hre.ethers.ZeroAddress;
      await rejects(() => f.factory.deploy(...addresses), 'ZeroRoleAddress');
    }
  });

  it('requires minter for all operator actions, including deployer admin', async function () {
    const f = await fixture();
    const encrypted = await input(f.token, f.admin, 1n);
    for (const signer of [f.admin, f.user, f.outsider]) {
      const token = f.token.connect(signer);
      for (const action of [
        () => token.confidentialMint(f.user.address, encrypted.handles[0], encrypted.inputProof, tag('forbidden')),
        () => token.voidDeposit(tag('forbidden')),
        () => token.markRedeemFulfilled(1, tag('tx')),
        () => token.reCreditRedeem(1),
      ]) await rejects(action, 'AccessControlUnauthorizedAccount');
    }
  });

  it('admin grants and revokes mint authority while nonadmin cannot', async function () {
    const f = await fixture();
    const role = await f.token.MINTER_ROLE();
    await rejects(() => f.token.connect(f.minter).grantRole(role, f.user.address), 'AccessControlUnauthorizedAccount');
    await receipt(f.token.grantRole(role, f.user.address));
    await receipt(f.token.connect(f.user).voidDeposit(tag('new-minter')));
    await receipt(f.token.revokeRole(role, f.user.address));
    await rejects(() => f.token.connect(f.user).voidDeposit(tag('revoked')), 'AccessControlUnauthorizedAccount');
  });

  it('mints actual amount with holder/service ACL and denies outsider balance, amount and supply', async function () {
    const f = await fixture();
    const minted = await mint(f, 19n);
    same(minted.depositId, tag('deposit'), 'deposit event ID');
    same(minted.to, f.user.address, 'deposit recipient');
    same(await f.token.consumed(tag('deposit')), 1n, 'minted status');
    same(await f.token.mintedAmounts(tag('deposit')), minted.amount, 'recorded actual handle');
    same(await actual(f, minted.amount), 19n, 'service reads mint actual');
    same(await balance(f), 19n, 'holder reads balance');
    same(await supply(f), 19n, 'auditor reads supply');
    await denied(f, minted.amount);
    await denied(f, await f.token.confidentialBalanceOf(f.user.address));
    await denied(f, await f.token.confidentialTotalSupply());
    await denied(f, await f.token.confidentialBalanceOf(f.user.address), f.service);
  });

  it('blocks duplicate mint and both mint/void consumption orderings', async function () {
    const f = await fixture();
    const first = await mint(f, 8n);
    await rejects(() => mint(f, 8n), 'DepositConsumed');
    await rejects(() => f.token.connect(f.minter).voidDeposit(tag('deposit')), 'DepositConsumed');
    const result = await receipt(f.token.connect(f.minter).voidDeposit(tag('refund')));
    same(event(f.token, result, 'DepositVoided').depositId, tag('refund'), 'void event');
    same(await f.token.consumed(tag('refund')), 2n, 'refunded status');
    await rejects(() => mint(f, 9n, 'refund'), 'DepositConsumed');
    await rejects(() => f.token.connect(f.minter).voidDeposit(tag('refund')), 'DepositConsumed');
    same(await f.token.mintedAmounts(tag('deposit')), first.amount, 'mint record immutable');
  });

  it('invalid proof rolls back deposit consumption and committed capacity', async function () {
    const f = await fixture();
    const encrypted = await input(f.token, f.minter, MAX);
    await rejects(() => f.token.connect(f.minter).confidentialMint(f.user.address, encrypted.handles[0], '0x', tag('retry')));
    same(await f.token.consumed(tag('retry')), 0n, 'invalid proof leaves unused tag');
    same(await f.token.mintedAmounts(tag('retry')), hre.ethers.ZeroHash, 'invalid proof leaves no result');
    await mint(f, MAX, 'retry');
    same(await supply(f), MAX, 'full capacity remains available');
  });

  it('zero recipient rolls back deposit consumption and committed capacity', async function () {
    const f = await fixture();
    await rejects(() => mint(f, MAX, 'retry', { address: hre.ethers.ZeroAddress }), 'ERC7984InvalidReceiver');
    same(await f.token.consumed(tag('retry')), 0n, 'zero recipient leaves unused tag');
    await mint(f, MAX, 'retry');
    same(await supply(f), MAX, 'capacity rollback permits full mint');
  });

  it('malformed redeem proof rolls back funds and ID allocation', async function () {
    const f = await fixture();
    await mint(f, 9n);
    const encrypted = await input(f.token, f.user, 4n);
    await rejects(() => f.token.connect(f.user).requestRedeem(encrypted.handles[0], '0x', '0x'));
    same(await balance(f), 9n, 'invalid redeem proof preserves holder balance');
    same(await supply(f), 9n, 'invalid redeem proof preserves supply');
    same(await f.token.nextRedeemId(), 1n, 'invalid redeem proof does not allocate ID');
    await rejects(() => f.token.redemptions(1), 'UnknownRedemption');
  });

  it('rejects encrypted input bound to the wrong caller or contract', async function () {
    const f = await fixture();
    const other = await fixture();
    for (const encrypted of [await input(f.token, f.user, 7n), await input(other.token, f.minter, 7n)]) {
      await rejects(() => f.token.connect(f.minter).confidentialMint(f.user.address, encrypted.handles[0], encrypted.inputProof, tag('wrong')));
      same(await f.token.consumed(tag('wrong')), 0n, 'binding failure leaves unused tag');
    }
    const wrong = await input(f.token, f.other, 3n);
    await rejects(() => f.token.connect(f.user).requestRedeem(wrong.handles[0], wrong.inputProof, '0x'));
    const wrongContract = await input(other.token, f.user, 3n);
    await rejects(() => f.token.connect(f.user).requestRedeem(wrongContract.handles[0], wrongContract.inputProof, '0x'));
    same(await f.token.nextRedeemId(), 1n, 'binding failure does not allocate ID');
  });

  it('records normal and exact actual burns, user/service ACL and encrypted destination', async function () {
    const f = await fixture();
    await mint(f, 19n);
    for (const [value, remaining] of [[7n, 12n], [12n, 0n]]) {
      const requested = await redeem(f, value);
      const record = await f.token.redemptions(requested.redeemId);
      same(record.user, f.user.address, 'original burner stored');
      same(record.amount, requested.amount, 'actual handle stored');
      same(record.status, 1n, 'pending status');
      same(record.zcashTxid, hre.ethers.ZeroHash, 'no payout yet');
      same(requested.destCt, '0x1234', 'opaque destination passed through');
      same(await actual(f, record.amount), value, 'service reads actual burn');
      same(await actual(f, record.amount, f.user), value, 'burner retains actual ACL');
      await denied(f, record.amount);
      same(await balance(f), remaining, 'balance reduced by actual');
      same(await supply(f), remaining, 'auditor reads updated supply');
    }
    same(await f.token.nextRedeemId(), 3n, 'monotonic redemption IDs');
  });

  it('overburn records zero, preserves funds and recredits zero to original user', async function () {
    const f = await fixture();
    await mint(f, 11n);
    const requested = await redeem(f, 12n);
    same(await actual(f, requested.amount), 0n, 'overburn actual zero');
    same(await balance(f), 11n, 'overburn preserves balance');
    same(await supply(f), 11n, 'overburn preserves supply');
    const result = await receipt(f.token.connect(f.minter).reCreditRedeem(requested.redeemId));
    const restored = event(f.token, result, 'RedeemRecredited');
    same(restored.user, f.user.address, 'recredit target original user');
    same(await actual(f, restored.amount, f.user), 0n, 'zero actual recredited');
    same(await balance(f), 11n, 'zero recredit preserves balance');
  });

  it('zero and uninitialized-account burns succeed with zero actual', async function () {
    const f = await fixture();
    for (const value of [3n, 0n]) {
      const requested = await redeem(f, value, f.other);
      same(await actual(f, requested.amount), 0n, 'uninitialized burn actual zero');
      same(await supply(f), 0n, 'auditor decrypts supply after empty burn');
      await receipt(f.token.connect(f.minter).reCreditRedeem(requested.redeemId));
      same(await balance(f, f.other), 0n, 'zero recredit original empty account');
    }
    await mint(f, MAX);
    same(await supply(f), MAX, 'zero burns and recredits consume no capacity');
  });

  it('zero mint and funded zero burn preserve capacity and balances', async function () {
    const f = await fixture();
    const zero = await mint(f, 0n, 'zero');
    same(await actual(f, zero.amount), 0n, 'zero mint actual');
    await mint(f, MAX);
    const zeroBurn = await redeem(f, 0n);
    same(await actual(f, zeroBurn.amount), 0n, 'funded zero burn actual');
    same(await balance(f), MAX, 'funded zero burn unchanged');
    await receipt(f.token.connect(f.minter).markRedeemFulfilled(zeroBurn.redeemId, tag('zero-outcome')));
    same(await supply(f), MAX, 'zero fulfillment releases no capacity');
  });

  it('overflow mint consumes tag as Minted but records honest actual zero', async function () {
    const f = await fixture();
    await mint(f, MAX);
    const overflow = await mint(f, 1n, 'overflow', f.other);
    same(await f.token.consumed(tag('overflow')), 1n, 'overflow consumes tag');
    same(await actual(f, overflow.amount), 0n, 'overflow actual zero');
    same(await f.token.mintedAmounts(tag('overflow')), overflow.amount, 'zero actual recorded');
    same(await balance(f), MAX, 'original balance unchanged');
    same(await balance(f, f.other), 0n, 'overflow recipient receives zero');
    same(await supply(f), MAX, 'supply does not wrap');
    await rejects(() => mint(f, 1n, 'overflow'), 'DepositConsumed');
  });

  it('reserves pending burn capacity against intervening mint and recredits recorded actual exactly', async function () {
    const f = await fixture();
    await mint(f, MAX);
    const requested = await redeem(f, 9n);
    const intervening = await mint(f, 9n, 'intervening', f.other);
    same(await actual(f, intervening.amount), 0n, 'pending reserve blocks capacity reuse');
    same(await supply(f), MAX - 9n, 'circulating supply reflects burn');
    const result = await receipt(f.token.connect(f.minter).reCreditRedeem(requested.redeemId));
    const restored = event(f.token, result, 'RedeemRecredited');
    same(await actual(f, restored.amount, f.user), 9n, 'exact recorded burn restored');
    same(await balance(f), MAX, 'original user restored');
    same(await balance(f, f.other), 0n, 'intervening recipient unchanged');
    same(await supply(f), MAX, 'auditor reads recredit supply');
    same((await f.token.redemptions(requested.redeemId)).amount, requested.amount, 'recorded burn immutable');
  });

  it('fulfillment releases only its actual burn capacity', async function () {
    const f = await fixture();
    await mint(f, MAX);
    const requested = await redeem(f, 7n);
    const txid = tag('zcash-payout');
    const result = await receipt(f.token.connect(f.minter).markRedeemFulfilled(requested.redeemId, txid));
    same(event(f.token, result, 'RedeemFulfilled').zcashTxid, txid, 'fulfillment txid event');
    const record = await f.token.redemptions(requested.redeemId);
    same(record.status, 2n, 'fulfilled status');
    same(record.zcashTxid, txid, 'payout txid recorded');
    const replacement = await mint(f, 7n, 'replacement', f.other);
    same(await actual(f, replacement.amount), 7n, 'fulfilled capacity reusable');
    const overflow = await mint(f, 1n, 'too-much');
    same(await actual(f, overflow.amount), 0n, 'only exact capacity released');
    same(await supply(f), MAX, 'supply reaches bound safely');
  });

  it('allows unreserved capacity while preserving pending actual burns', async function () {
    const f = await fixture();
    await mint(f, MAX - 10n);
    const pending = await redeem(f, 4n);
    const accepted = await mint(f, 10n, 'remaining-capacity', f.other);
    same(await actual(f, accepted.amount), 10n, 'unreserved capacity remains usable');
    const refused = await mint(f, 1n, 'reserved-capacity', f.other);
    same(await actual(f, refused.amount), 0n, 'pending capacity remains reserved');
    await receipt(f.token.connect(f.minter).reCreditRedeem(pending.redeemId));
    same(await balance(f), MAX - 10n, 'pending original user restored exactly');
    same(await balance(f, f.other), 10n, 'new issuance remains credited');
    same(await supply(f), MAX, 'combined issuance reaches bound safely');
  });

  for (const recreditFirst of [true, false]) {
    it(`preserves multiple pending reserves with ${recreditFirst ? 'recredit' : 'fulfillment'} first`, async function () {
      const f = await fixture();
      await mint(f, MAX);
      const a = await redeem(f, 5n);
      const b = await redeem(f, 7n);
      const credit = () => receipt(f.token.connect(f.minter).reCreditRedeem(a.redeemId));
      const fulfill = () => receipt(f.token.connect(f.minter).markRedeemFulfilled(b.redeemId, tag('paid')));
      if (recreditFirst) await credit(); else await fulfill();
      const first = await mint(f, 7n, 'middle', f.other);
      same(await actual(f, first.amount), recreditFirst ? 0n : 7n, 'only fulfillment frees capacity');
      if (recreditFirst) await fulfill(); else await credit();
      if (recreditFirst) await mint(f, 7n, 'released', f.other);
      same(await balance(f), MAX - 7n, 'original user restored only first burn');
      same(await balance(f, f.other), 7n, 'only fulfilled reserve replaced');
      same(await supply(f), MAX, 'all outcomes conserve bound');
    });
  }

  it('rejects nonexistent IDs and zero payout txid without changing Pending', async function () {
    const f = await fixture();
    for (const id of [0n, 1n, 999n]) {
      await rejects(() => f.token.redemptions(id), 'UnknownRedemption');
      await rejects(() => f.token.connect(f.minter).reCreditRedeem(id), 'UnknownRedemption');
      await rejects(() => f.token.connect(f.minter).markRedeemFulfilled(id, tag('tx')), 'UnknownRedemption');
    }
    const requested = await redeem(f, 0n);
    await rejects(() => f.token.connect(f.minter).markRedeemFulfilled(requested.redeemId, hre.ethers.ZeroHash), 'ZeroZcashTxid');
    same((await f.token.redemptions(requested.redeemId)).status, 1n, 'failed fulfillment remains pending');
  });

  for (const terminal of ['recredit', 'fulfill']) {
    it(`${terminal} is one-time and mutually exclusive with the other outcome`, async function () {
      const f = await fixture();
      await mint(f, 9n);
      const requested = await redeem(f, 4n);
      const credit = () => f.token.connect(f.minter).reCreditRedeem(requested.redeemId);
      const fulfill = () => f.token.connect(f.minter).markRedeemFulfilled(requested.redeemId, tag('tx'));
      for (const signer of [f.admin, f.user, f.service]) {
        await rejects(() => f.token.connect(signer).reCreditRedeem(requested.redeemId), 'AccessControlUnauthorizedAccount');
        await rejects(() => f.token.connect(signer).markRedeemFulfilled(requested.redeemId, tag('tx')), 'AccessControlUnauthorizedAccount');
      }
      await receipt(terminal === 'recredit' ? credit() : fulfill());
      await rejects(credit, 'RedemptionNotPending');
      await rejects(fulfill, 'RedemptionNotPending');
      same(await balance(f), terminal === 'recredit' ? 9n : 5n, 'terminal outcome balance');
      same((await f.token.redemptions(requested.redeemId)).status, terminal === 'recredit' ? 3n : 2n, 'terminal status retained');
    });
  }

  it('refreshes auditor ACL on new supply handles across mint, burn and recredit', async function () {
    const f = await fixture();
    await mint(f, 13n);
    const first = await f.token.confidentialTotalSupply();
    same(await supply(f), 13n, 'auditor mint supply');
    const requested = await redeem(f, 4n);
    const second = await f.token.confidentialTotalSupply();
    assert.ok(first !== second, 'burn creates new supply handle');
    same(await supply(f), 9n, 'auditor burn supply');
    await receipt(f.token.connect(f.minter).reCreditRedeem(requested.redeemId));
    const third = await f.token.confidentialTotalSupply();
    assert.ok(second !== third && first !== third, 'recredit creates new supply handle');
    same(await supply(f), 13n, 'auditor recredit supply');
    await denied(f, third);
  });

  it('ordinary transfers preserve supply, holder ACL and pending recredit capacity', async function () {
    const f = await fixture();
    await mint(f, MAX);
    const requested = await redeem(f, 3n);
    const before = await f.token.confidentialTotalSupply();
    const encrypted = await input(f.token, f.user, 5n);
    await receipt(f.token.connect(f.user)['confidentialTransfer(address,bytes32,bytes)'](
      f.other.address, encrypted.handles[0], encrypted.inputProof));
    same(await f.token.confidentialTotalSupply(), before, 'transfer retains supply handle');
    same(await balance(f, f.other), 5n, 'recipient can decrypt transferred balance');
    await receipt(f.token.connect(f.minter).reCreditRedeem(requested.redeemId));
    same(await balance(f), MAX - 5n, 'recorded burn restored after transfer');
    same(await supply(f), MAX, 'transfer preserves recredit capacity');
  });
});
