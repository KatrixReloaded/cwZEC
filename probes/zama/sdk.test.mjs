import test from 'node:test';
import assert from 'node:assert/strict';
import { createProbe } from './sdk.mjs';

const contract = `0x${'11'.repeat(20)}`;
const user = `0x${'22'.repeat(20)}`;
const other = `0x${'33'.repeat(20)}`;
const handle = `0x${'ab'.repeat(32)}`;
const signature = `0x${'44'.repeat(65)}`;

function fixture({ failure, recovered = user } = {}) {
  let expected;
  let calls = 0;
  const instance = {
    createEncryptedInput() {
      return { add64(value) { expected = value + 1n; return this; }, async encrypt() {
        return { handles: [handle], inputProof: new Uint8Array([1, 2]) };
      } };
    },
    generateKeypair: () => ({ publicKey: 'aabb', privateKey: 'ccdd' }),
    createEIP712: (publicKey, contracts, start, duration) => ({
      domain: { chainId: 11155111n },
      types: { EIP712Domain: [], UserDecryptRequestVerification: [] },
      message: { publicKey, contractAddresses: contracts, startTimestamp: start, durationDays: duration },
    }),
    async userDecrypt() {
      calls++;
      if (failure) throw new Error(failure);
      return { [handle]: expected };
    },
  };
  const probe = createProbe({
    createInstance: async () => instance,
    SepoliaConfigV2: {},
    ethers: {
      getAddress(value) { if (!/^0x[0-9a-f]{40}$/.test(value)) throw new Error(); return value; },
      ZeroAddress: `0x${'00'.repeat(20)}`,
      TypedDataEncoder: { hash: () => `0x${'55'.repeat(32)}` },
      verifyTypedData: () => recovered,
    },
  });
  return { probe, calls: () => calls };
}

async function prepare(probe, permitUser = user) {
  assert.equal((await probe({ command: 'init', rpcUrl: 'https://rpc.example' })).ok, true);
  const encrypted = await probe({ command: 'encrypt', contract, user });
  assert.deepEqual(Object.keys(encrypted), ['handle', 'inputProof']);
  const permit = await probe({ command: 'permit', contract, user: permitUser, handle });
  assert.equal(permit.typedData.domain.chainId, '11155111');
  assert.equal(typeof permit.typedData.message.startTimestamp, 'string');
  assert.equal(permit.typedData.message.durationDays, '1');
  assert.equal(permit.publicKey, '0xaabb');
}

test('returns only comparison and consumes the ephemeral permit', async () => {
  const { probe } = fixture();
  await prepare(probe);
  assert.deepEqual(await probe({ command: 'decrypt', signature }), { authorized: true, matchesExpected: true });
  assert.deepEqual(await probe({ command: 'decrypt', signature }), { error: 'missing_permit' });
});

test('only the exact SDK user ACL preflight failure is an unauthorized result', async () => {
  const denial = `User address ${other} is not authorized to user decrypt handle ${handle}!`;
  for (const failure of [denial, 'network unavailable', denial + ' extra',
    `dapp contract ${contract} is not authorized to user decrypt handle ${handle}!`]) {
    const { probe } = fixture({ failure, recovered: other });
    await prepare(probe, other);
    assert.deepEqual(await probe({ command: 'decrypt', signature }), failure === denial
      ? { authorized: false, reason: 'sdk_acl_denied' }
      : { error: 'decrypt_failed' });
  }
});

test('wrong signer never invokes SDK decryption; rejects malformed handle', async () => {
  const { probe, calls } = fixture({ recovered: other });
  await prepare(probe);
  assert.deepEqual(await probe({ command: 'decrypt', signature }), { error: 'signature_user_mismatch' });
  assert.equal(calls(), 0);
  assert.deepEqual(await probe({ command: 'permit', contract, user, handle: 'garbage' }), { error: 'invalid_handle' });
});
