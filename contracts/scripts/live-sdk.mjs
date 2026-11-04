// Bounded lifecycle driver: no wallet signer or signing-key input exists here.
import { randomBytes } from 'node:crypto';
import { createRequire } from 'node:module';
import { createInterface } from 'node:readline';
import { pathToFileURL } from 'node:url';

// Preserve the already proven live SDK pin, independently of the mock plugin's peers.
const probeRequire = createRequire(new URL('../../probes/zama/package.json', import.meta.url));
for (const method of ['log', 'info', 'warn', 'error', 'debug', 'trace', 'dir', 'table']) console[method] = () => {};
const { createInstance, SepoliaConfigV2 } = await import(pathToFileURL(probeRequire.resolve('@zama-fhe/relayer-sdk/node')));
const { getAddress, TypedDataEncoder, verifyTypedData } = await import(pathToFileURL(probeRequire.resolve('ethers')));
const initial = BigInt(`0x${randomBytes(7).toString('hex')}`) + 12n;
const values = new Map([
  ['initial', initial], ['partial', initial / 3n], ['remaining', initial - initial / 3n],
  ['over_balance', initial + 1n], ['zero', 0n], ['maximum', (1n << 64n) - 1n], ['overflow', 1n],
  ['maximum_remaining', (1n << 64n) - 1n - initial / 3n],
]);
let instance;
let pending;
const hex = (bytes) => `0x${typeof bytes === 'string' ? bytes.replace(/^0x/, '') : Buffer.from(bytes).toString('hex')}`;

async function request(r) {
  if (r.command === 'init') {
    instance = await createInstance({ ...SepoliaConfigV2, network: r.rpcUrl });
    return { ok: true, aclAddress: SepoliaConfigV2.aclContractAddress };
  }
  if (r.command === 'quit') { pending = undefined; values.clear(); return { ok: true }; }
  if (!instance) return { error: 'not_initialized' };
  if (r.command === 'encrypt') {
    if (!values.has(r.value)) return { error: 'unknown_private_value' };
    const encrypted = await instance.createEncryptedInput(getAddress(r.contract), getAddress(r.user))
      .add64(values.get(r.value)).encrypt();
    return { handle: hex(encrypted.handles[0]), inputProof: hex(encrypted.inputProof) };
  }
  if (r.command === 'permit') {
    const contract = getAddress(r.contract), user = getAddress(r.user);
    if (!Array.isArray(r.checks) || r.checks.length === 0) return { error: 'empty_checks' };
    for (const check of r.checks) {
      if (!/^0x[0-9a-fA-F]{64}$/.test(check.handle) || !values.has(check.expected)) return { error: 'invalid_check' };
    }
    const keypair = instance.generateKeypair(), start = Math.floor(Date.now() / 1000);
    const typedData = JSON.parse(JSON.stringify(instance.createEIP712(keypair.publicKey, [contract], start, 1),
      (_, value) => typeof value === 'bigint' ? value.toString() : value));
    typedData.domain.chainId = String(typedData.domain.chainId);
    typedData.message.startTimestamp = String(typedData.message.startTimestamp);
    typedData.message.durationDays = String(typedData.message.durationDays);
    const { EIP712Domain: _, ...types } = typedData.types;
    pending = { contract, user, checks: r.checks, keypair, start, typedData, types };
    return { publicKey: hex(keypair.publicKey), startTimestamp: String(start), typedData,
      digest: TypedDataEncoder.hash(typedData.domain, types, typedData.message) };
  }
  if (r.command === 'decrypt') {
    const p = pending; pending = undefined;
    if (!p) return { error: 'missing_permit' };
    if (getAddress(verifyTypedData(p.typedData.domain, p.types, p.typedData.message, r.signature)) !== p.user)
      return { error: 'permit_signer_mismatch' };
    let clear;
    try {
      clear = await instance.userDecrypt(p.checks.map(c => ({ handle: c.handle, contractAddress: p.contract })),
        p.keypair.privateKey, p.keypair.publicKey, r.signature, [p.contract], p.user, p.start, 1);
    } catch (error) {
      if (p.checks.some(c => error?.message === `User address ${p.user} is not authorized to user decrypt handle ${c.handle.toLowerCase()}!`))
        return { authorized: false, reason: 'sdk_acl_denied' };
      return { error: 'decryption_failed' };
    }
    const matches = p.checks.every(c => {
      const value = clear[c.handle] ?? clear[c.handle.toLowerCase()];
      return value !== undefined && BigInt(value) === values.get(c.expected);
    });
    return { authorized: true, matchesExpected: matches };
  }
  return { error: 'unknown_command' };
}

const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
for await (const line of lines) {
  let r, result;
  try { r = JSON.parse(line); result = await request(r); }
  catch { result = { error: 'sdk_operation_failed' }; }
  process.stdout.write(`${JSON.stringify(result)}\n`);
  if (r?.command === 'quit') break;
}
lines.close();
