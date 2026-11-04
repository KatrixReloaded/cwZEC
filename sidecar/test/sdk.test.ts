import assert from 'node:assert/strict';
import { test } from 'node:test';
import { ZamaSDK, type EIP712TypedData } from '@zama-fhe/sdk';
import { Wallet, TypedDataEncoder } from 'ethers';
import { classifyDecryptionError, createSdk, type DecryptionInput } from '../src/sdk.ts';

const contract = '0x1111111111111111111111111111111111111111';
const user = '0x2222222222222222222222222222222222222222';
const ciphertext = `0x${'ab'.repeat(32)}`;
const message = `User ${user} is not authorized to decrypt handle ${ciphertext}!`;

// Reproduce 0.13.2's concrete runtime error shape, including its base-class name bug.
class AclUserDecryptionError extends Error {
  contractName = 'ACL';
  constructor(text = message) {
    super(`${text}\nVersion: @fhevm/sdk@0.13.2`);
    this.name = 'FhevmErrorBase';
  }
}

test('only the pinned SDK local user denial becomes sdk_acl_denied', () => {
  assert.equal(classifyDecryptionError(new AclUserDecryptionError(), user, ciphertext), 'sdk_acl_denied');
  for (const error of [
    new Error(message),
    new Error('fetch failed: private payload'),
    new Error('HTTP 403 forbidden'),
    Object.assign(new Error(`${message}\nVersion: @fhevm/sdk@0.13.2`), { name: 'FhevmErrorBase', contractName: 'ACL' }),
    Object.assign(new Error(message), { name: 'NotEntitledError', code: 'NOT_ENTITLED' }),
    new AclUserDecryptionError(`Dapp contract ${contract} is not authorized to user decrypt handle ${ciphertext}!`),
    new AclUserDecryptionError('userAddress must not equal contractAddress'),
    new Error('outer', { cause: new AclUserDecryptionError() }),
    { name: 'FhevmErrorBase', message },
  ]) {
    assert.equal(classifyDecryptionError(error, user, ciphertext), 'decrypt_failed');
  }
  assert.equal(classifyDecryptionError(new AclUserDecryptionError(), contract, ciphertext), 'decrypt_failed');
});

test('rejects unsafe RPC URLs without initializing the SDK', () => {
  for (const rpc of ['not-a-url', 'file:///tmp/rpc', 'https://user:secret@example.test']) {
    assert.throws(() => createSdk(rpc), { message: 'invalid_rpc_url' });
  }
});

test('read-only adapter boundaries and exact SDK method wiring without network', async (t) => {
  const wallet = Wallet.createRandom();
  const raw: EIP712TypedData = {
    domain: { name: 'Decryption', version: '1', chainId: 11155111, verifyingContract: contract },
    primaryType: 'UserDecryptRequestVerification',
    types: {
      EIP712Domain: [
        { name: 'name', type: 'string' }, { name: 'version', type: 'string' },
        { name: 'chainId', type: 'uint256' }, { name: 'verifyingContract', type: 'address' },
      ],
      UserDecryptRequestVerification: [
        { name: 'publicKey', type: 'bytes' }, { name: 'contractAddresses', type: 'address[]' },
        { name: 'startTimestamp', type: 'uint256' }, { name: 'durationDays', type: 'uint256' },
        { name: 'extraData', type: 'bytes' },
      ],
    },
    message: { publicKey: '0x1234', contractAddresses: [contract], startTimestamp: 123n, durationDays: 1n, extraData: '0x00' },
  };
  const keypair = { publicKey: '0x1234', tkmsVersion: 'test' };
  let result: unknown = [{ type: 'uint64', value: 42n }];
  let failure: unknown;
  let decryptCalls = 0;
  let parseCalls = 0;
  let keyLoads = 0;
  let keyFailure = false;
  let keyStall = false;
  const backend = {
    fetchFheEncryptionKeyBytes: async (params: { options: { signal: AbortSignal; fetchRetries: number } }) => {
      keyLoads++;
      if (keyFailure) throw new Error('sensitive upstream payload');
      if (keyStall) return new Promise(() => {});
      assert.equal(params.options.signal.aborted, false);
      assert.equal(params.options.fetchRetries, 0);
      return {};
    },
    generateTransportKeyPair: async () => keypair,
    createUnsignedLegacyDecryptionPermitEip712: async (params: Record<string, unknown>) => {
      assert.equal(params.transportKeyPair, keypair);
      assert.deepEqual(params.contractAddresses, [contract]);
      assert.equal(params.durationSeconds, 86400);
      raw.message.startTimestamp = params.startTimestamp;
      return raw;
    },
    parseSignedDecryptionPermit: async (params: { transportKeyPair: unknown; serializedPermit: Record<string, unknown> }) => {
      parseCalls++;
      assert.equal(params.transportKeyPair, keypair);
      assert.equal(params.serializedPermit.version, 1);
      assert.equal(params.serializedPermit.signerAddress, wallet.address);
      return 'signed-permit';
    },
    decryptValuesFromPairs: async (params: Record<string, unknown>) => {
      decryptCalls++;
      assert.deepEqual(params.pairs, [{ encryptedValue: ciphertext, contractAddress: contract }]);
      assert.equal(params.transportKeyPair, keypair);
      assert.equal(params.signedPermit, 'signed-permit');
      const options = params.options as { timeout: number; signal: AbortSignal };
      assert.equal(options.timeout, 120000);
      assert.equal(options.signal.aborted, false);
      if (failure) throw failure;
      return result;
    },
  };
  const relayerDescriptor = Object.getOwnPropertyDescriptor(ZamaSDK.prototype, 'relayer')!;
  Object.defineProperty(ZamaSDK.prototype, 'relayer', { configurable: true, get: () => backend });
  t.after(() => Object.defineProperty(ZamaSDK.prototype, 'relayer', relayerDescriptor));
  t.mock.method(ZamaSDK.prototype, 'encrypt', async (...[params, options]: Parameters<ZamaSDK['encrypt']>) => {
    assert.deepEqual(params, { contractAddress: contract, userAddress: wallet.address, values: [{ type: 'euint64', value: 42n }] });
    assert.equal(options?.timeout, 120000);
    assert.ok(options?.signal instanceof AbortSignal);
    return { encryptedValues: [ciphertext as `0x${string}`], inputProof: '0x1234' as const };
  });
  t.mock.method(ZamaSDK.prototype, 'terminate', () => {});
  const sdk = createSdk('http://127.0.0.1:1');
  t.after(() => sdk.dispose());
  await sdk.prepareEncryption();
  assert.equal(keyLoads, 1);
  assert.deepEqual(await sdk.encrypt(contract, wallet.address, 42n), { handle: ciphertext, inputProof: '0x1234' });
  for (const value of [-1n, 1n << 64n, 42 as unknown as bigint]) {
    await assert.rejects(sdk.encrypt(contract, wallet.address, value), { message: 'invalid_uint64' });
  }
  const prepared = await sdk.prepare(contract, wallet.address);
  assert.equal(prepared.keypair, keypair);
  assert.equal(prepared.publicKey, '0x1234');
  assert.equal(prepared.eip712.domain.chainId, '11155111');
  assert.equal(typeof prepared.eip712.message.startTimestamp, 'string');
  assert.equal(prepared.eip712.message.durationDays, '1');
  assert.equal(prepared.startTimestamp, prepared.eip712.message.startTimestamp);
  const { EIP712Domain: _domain, ...types } = prepared.eip712.types;
  const mutableTypes = Object.fromEntries(Object.entries(types).map(([name, fields]) => [name, [...fields]]));
  assert.equal(prepared.digest, TypedDataEncoder.hash(prepared.eip712.domain, mutableTypes, prepared.eip712.message));
  const signature = await wallet.signTypedData(prepared.eip712.domain, mutableTypes, prepared.eip712.message);
  const input: DecryptionInput = { contract, user: wallet.address, handle: ciphertext, keypair: prepared.keypair, eip712: prepared.eip712 };
  await assert.rejects(sdk.decrypt(input, 'bad'), { message: 'invalid_signature' });
  await assert.rejects(sdk.decrypt({ ...input, user }, signature), { message: 'signature_user_mismatch' });
  assert.equal(parseCalls, 0);
  assert.equal(decryptCalls, 0);
  assert.equal(await sdk.decrypt(input, signature), 42n);
  for (const bad of [[], [{ type: 'uint64', value: 1n }, { type: 'uint64', value: 2n }],
    [{ type: 'uint32', value: 1n }], [{ type: 'uint64', value: 1 }],
    [{ type: 'uint64', value: -1n }], [{ type: 'uint64', value: 1n << 64n }]]) {
    result = bad;
    await assert.rejects(sdk.decrypt(input, signature), { message: 'decrypt_failed' });
  }
  failure = new Error('secret infrastructure details');
  await assert.rejects(sdk.decrypt(input, signature), (error: unknown) => {
    assert.ok(error instanceof Error);
    assert.equal(error.message, 'decrypt_failed');
    assert.equal(error.cause, undefined);
    return true;
  });
  failure = new AclUserDecryptionError(`User ${wallet.address} is not authorized to decrypt handle ${ciphertext}!`);
  await assert.rejects(sdk.decrypt(input, signature), { message: 'sdk_acl_denied' });
  keyFailure = true;
  await assert.rejects(sdk.prepareEncryption(), { message: 'encryption_key_failed' });
  keyFailure = false;
  keyStall = true;
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const warming = sdk.prepareEncryption();
  t.mock.timers.tick(120000);
  await assert.rejects(warming, { message: 'encryption_key_timeout' });
  t.mock.timers.reset();
  sdk.dispose();
  sdk.dispose();
  await assert.rejects(sdk.prepare(contract, wallet.address), { message: 'sdk_disposed' });
});
