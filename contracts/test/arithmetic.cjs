const hre = require('hardhat');
const { deployHarness, encryptedCall, state, same, requireLocalMock } = require('./helpers.cjs');

describe('upstream ERC7984 encrypted arithmetic', function () {
  before(requireLocalMock);

  it('normal and exact burns return the actual burn and update balance and supply', async function () {
    const { token, caller } = await deployHarness();
    await encryptedCall(token, caller, 'mint', 19n, [caller.address]);
    let observed = await state(token, caller, caller);
    same(observed.balance, 19n, 'mint balance matches request');
    same(observed.supply, 19n, 'mint supply matches request');
    same(observed.actual, 19n, 'mint actual matches request');

    await encryptedCall(token, caller, 'burn', 7n);
    observed = await state(token, caller, caller);
    same(observed.balance, 12n, 'normal burn balance decreases by actual');
    same(observed.supply, 12n, 'normal burn supply decreases by actual');
    same(observed.actual, 7n, 'normal burn actual matches request');

    await encryptedCall(token, caller, 'burn', 12n);
    observed = await state(token, caller, caller);
    same(observed.balance, 0n, 'exact burn empties balance');
    same(observed.supply, 0n, 'exact burn empties supply');
    same(observed.actual, 12n, 'exact burn actual matches request');
  });

  it('overburn succeeds with zero actual and preserves balance and supply', async function () {
    const { token, caller } = await deployHarness();
    await encryptedCall(token, caller, 'mint', 11n, [caller.address]);
    const before = await state(token, caller, caller);
    await encryptedCall(token, caller, 'burn', 12n);
    const after = await state(token, caller, caller);
    same(after.balance, before.balance, 'overburn balance unchanged');
    same(after.supply, before.supply, 'overburn supply unchanged');
    same(after.actual, 0n, 'overburn actual zero');
  });

  it('burning an uninitialized account succeeds with zero actual and preserves other funds', async function () {
    const { token, caller, receiver } = await deployHarness();
    await encryptedCall(token, caller, 'mint', 13n, [caller.address]);
    same(await token.confidentialBalanceOf(receiver.address), hre.ethers.ZeroHash,
      'account starts uninitialized');
    await encryptedCall(token, receiver, 'burn', 1n);
    const observed = await state(token, receiver, receiver);
    same(observed.balance, 0n, 'uninitialized burn balance zero');
    same(observed.supply, 13n, 'uninitialized burn supply unchanged');
    same(observed.actual, 0n, 'uninitialized burn actual zero');
    const funded = await state(token, caller, receiver);
    same(funded.balance, 13n, 'uninitialized burn preserves funded account balance');
  });

  it('burning an initialized zero balance succeeds with zero actual', async function () {
    const { token, caller } = await deployHarness();
    await encryptedCall(token, caller, 'mint', 0n, [caller.address]);
    const before = await state(token, caller, caller);
    same((await token.confidentialBalanceOf(caller.address)) !== hre.ethers.ZeroHash, true,
      'zero balance has an initialized handle');
    await encryptedCall(token, caller, 'burn', 0n);
    const zeroBurn = await state(token, caller, caller);
    same(zeroBurn.balance, before.balance, 'zero requested burn balance unchanged');
    same(zeroBurn.supply, before.supply, 'zero requested burn supply unchanged');
    same(zeroBurn.actual, 0n, 'zero requested burn actual zero');
    await encryptedCall(token, caller, 'burn', 1n);
    const after = await state(token, caller, caller);
    same(after.balance, before.balance, 'initialized zero balance unchanged');
    same(after.supply, before.supply, 'initialized zero supply unchanged');
    same(after.actual, 0n, 'initialized zero burn actual zero');
  });

  it('overflow mint succeeds with zero actual and preserves receiver balance and supply', async function () {
    const { token, caller, receiver } = await deployHarness();
    const maximum = (1n << 64n) - 1n;
    await encryptedCall(token, caller, 'mint', maximum, [receiver.address]);
    const before = await state(token, receiver, caller);
    same(before.balance, maximum, 'maximum mint receiver balance matches request');
    same(before.supply, maximum, 'maximum mint supply matches request');
    same(before.actual, maximum, 'maximum mint actual matches request');
    await encryptedCall(token, caller, 'mint', 1n, [receiver.address]);
    const after = await state(token, receiver, caller);
    same(after.balance, before.balance, 'overflow receiver balance unchanged');
    same(after.supply, before.supply, 'overflow supply unchanged');
    same(after.actual, 0n, 'overflow mint actual zero');
  });

  it('reusing freed supply capacity prevents minting the recorded burn amount back', async function () {
    const { token, caller, receiver } = await deployHarness();
    const maximum = (1n << 64n) - 1n;
    await encryptedCall(token, caller, 'mint', maximum, [caller.address]);
    await encryptedCall(token, caller, 'burn', 9n);
    const burned = await state(token, caller, caller);
    same(burned.actual, 9n, 'capacity test burn actual matches request');
    await encryptedCall(token, receiver, 'mint', burned.actual, [receiver.address]);
    const replacement = await state(token, receiver, receiver);
    same(replacement.balance, burned.actual, 'another receiver consumes freed capacity');
    same(replacement.supply, maximum, 'replacement mint restores maximum supply');

    // Re-encrypt the observed burn amount: this probes arithmetic capacity only.
    // The production lifecycle must separately exercise its stored-handle path.
    await encryptedCall(token, caller, 'mint', burned.actual, [caller.address]);
    const after = await state(token, caller, caller);
    same(after.balance, burned.balance, 'failed recredit amount leaves burner balance unchanged');
    same(after.supply, maximum, 'failed recredit amount leaves supply unchanged');
    same(after.actual, 0n, 'minting recorded burn amount returns zero after capacity reuse');
  });
});
