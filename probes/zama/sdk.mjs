import { randomBytes } from 'node:crypto';
import { createInterface } from 'node:readline';
import { pathToFileURL } from 'node:url';

const EXECUTOR = '0x92C920834Ec8941d2C77D188936E1f7A6f49c127';
const hex = (value) => {
  if (typeof value === 'string') return `0x${value.replace(/^0x/, '')}`;
  return `0x${Buffer.from(value).toString('hex')}`;
};
const fail = (code) => { throw new Error(code); };

// Dependency injection keeps the ACL failure classification independently testable.
export function createProbe({ createInstance, SepoliaConfigV2, ethers }) {
  let instance;
  let expected;
  let pending;
  const address = (value) => {
    try {
      if (typeof value !== 'string') fail('invalid_address');
      const result = ethers.getAddress(value);
      if (result === ethers.ZeroAddress) fail('invalid_address');
      return result;
    } catch { fail('invalid_address'); }
  };
  const handle = (value) => {
    if (typeof value !== 'string' || !/^0x[0-9a-fA-F]{64}$/.test(value) || /^0x0{64}$/.test(value)) {
      fail('invalid_handle');
    }
    return value.toLowerCase();
  };
  return async (request) => {
    if (!request || typeof request !== 'object' || Array.isArray(request)) return { error: 'invalid_request' };
    const command = request.command;
    try {
      if (command === 'quit') {
        pending = undefined;
        expected = undefined;
        instance = undefined;
        return { ok: true };
      }
      if (command === 'init') {
        if (instance) return { error: 'already_initialized' };
        let rpc;
        try { rpc = new URL(request.rpcUrl); } catch { return { error: 'invalid_rpc_url' }; }
        if (!['http:', 'https:'].includes(rpc.protocol) || rpc.username || rpc.password) return { error: 'invalid_rpc_url' };
        instance = await createInstance({ ...SepoliaConfigV2, network: rpc.href });
        return {
          ok: true,
          aclAddress: SepoliaConfigV2.aclContractAddress,
          kmsAddress: SepoliaConfigV2.kmsContractAddress,
          inputVerifierAddress: SepoliaConfigV2.inputVerifierContractAddress,
          executorAddress: EXECUTOR,
        };
      }
      if (!instance) return { error: 'not_initialized' };
      if (command === 'encrypt') {
        const contract = address(request.contract);
        const user = address(request.user);
        pending = undefined;
        expected = undefined;
        const value = BigInt(`0x${randomBytes(7).toString('hex')}`);
        const encrypted = await instance.createEncryptedInput(contract, user).add64(value).encrypt();
        const encryptedHandle = handle(hex(encrypted.handles[0]));
        expected = { contract, user, value: value + 1n };
        return { handle: encryptedHandle, inputProof: hex(encrypted.inputProof) };
      }
      if (command === 'permit') {
        const contract = address(request.contract);
        const user = address(request.user);
        const ciphertext = handle(request.handle);
        if (!expected || expected.contract !== contract) return { error: 'missing_expected_result' };
        if (contract === user) return { error: 'invalid_user' };
        pending = undefined;
        const keypair = instance.generateKeypair();
        const start = Math.floor(Date.now() / 1000);
        const raw = instance.createEIP712(keypair.publicKey, [contract], start, 1);
        const typedData = JSON.parse(JSON.stringify(raw, (_, value) => typeof value === 'bigint' ? value.toString() : value));
        typedData.domain.chainId = String(typedData.domain.chainId);
        typedData.message.startTimestamp = String(typedData.message.startTimestamp);
        typedData.message.durationDays = String(typedData.message.durationDays);
        const { EIP712Domain: _, ...types } = typedData.types;
        const digest = ethers.TypedDataEncoder.hash(typedData.domain, types, typedData.message);
        pending = { contract, user, handle: ciphertext, keypair, start, typedData, types };
        return { publicKey: hex(keypair.publicKey), startTimestamp: String(start), typedData, digest };
      }
      if (command === 'decrypt') {
        const current = pending;
        pending = undefined; // A permit/keypair is consumed even on failure.
        if (!current) return { error: 'missing_permit' };
        if (typeof request.signature !== 'string' || !/^0x[0-9a-fA-F]{130}$/.test(request.signature)) return { error: 'invalid_signature' };
        let recovered;
        try {
          recovered = ethers.verifyTypedData(current.typedData.domain, current.types, current.typedData.message, request.signature);
        } catch { return { error: 'invalid_signature' }; }
        if (recovered.toLowerCase() !== current.user.toLowerCase()) return { error: 'signature_user_mismatch' };
        let result;
        try {
          result = await instance.userDecrypt(
            [{ handle: current.handle, contractAddress: current.contract }],
            current.keypair.privateKey, current.keypair.publicKey, request.signature,
            [current.contract], current.user, current.start, 1,
          );
        } catch (error) {
          // SDK 0.4.5 stable userDecrypt throws this exact string in its local
          // ACL preflight, before contacting the relayer. This is not a remote denial.
          const denial = `User address ${current.user} is not authorized to user decrypt handle ${current.handle}!`;
          if (error instanceof Error && error.message === denial) return { authorized: false, reason: 'sdk_acl_denied' };
          return { error: 'decrypt_failed' };
        }
        const clear = result[current.handle] ?? result[current.handle.slice(2)];
        if (clear === undefined) return { error: 'decrypt_result_missing' };
        return { authorized: true, matchesExpected: expected?.user === current.user && BigInt(clear) === expected.value };
      }
      return { error: 'unknown_command' };
    } catch (error) {
      const safe = ['invalid_address', 'invalid_handle'];
      return { error: safe.includes(error?.message) ? error.message : `${['init', 'encrypt', 'permit', 'decrypt'].includes(command) ? command : 'request'}_failed` };
    }
  };
}

async function main() {
  // SDK diagnostics must never leak plaintext or corrupt the JSON-lines channel.
  for (const method of ['log', 'info', 'warn', 'error', 'debug', 'trace', 'dir', 'table']) console[method] = () => {};
  let probe;
  try {
    const [sdk, ethers] = await Promise.all([import('@zama-fhe/relayer-sdk/node'), import('ethers')]);
    probe = createProbe({ ...sdk, ethers });
  } catch {
    process.stderr.write('sdk_load_failed\n');
    process.exitCode = 1;
    return;
  }
  const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
  for await (const line of lines) {
    let request;
    try { request = JSON.parse(line); } catch {
      process.stdout.write('{"error":"invalid_json"}\n');
      continue;
    }
    const response = await probe(request);
    process.stdout.write(`${JSON.stringify(response)}\n`);
    if (request?.command === 'quit') break;
  }
  lines.close();
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch(() => { process.stderr.write('probe_failed\n'); process.exitCode = 1; });
}
