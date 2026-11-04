// Read-only SDK selection gate; driven by Rust over private stdio.
import { createInterface } from 'node:readline';
for (const name of ['log', 'info', 'warn', 'error', 'debug', 'trace', 'dir', 'table'] as const) console[name] = () => {};
const { createSdk } = await import('../src/sdk.ts');
let sdk: ReturnType<typeof createSdk> | undefined;
let pending: any;
for await (const line of createInterface({ input: process.stdin })) {
  let result: unknown;
  try {
    const req = JSON.parse(line);
    if (req.command === 'init') {
      sdk = createSdk(req.rpcUrl);
      result = { ok: true };
    } else if (req.command === 'permit' && sdk) {
      const prepared = await sdk.prepare(req.contract, req.user);
      pending = { ...prepared, contract: req.contract, user: req.user, handle: req.handle };
      result = { publicKey: prepared.publicKey, startTimestamp: prepared.startTimestamp,
        typedData: prepared.eip712, digest: prepared.digest };
    } else if (req.command === 'decrypt' && sdk && pending) {
      const current = pending;
      pending = undefined;
      const value = await sdk.decrypt(current, req.signature);
      result = { matchesExpected: value === 0n };
    } else if (req.command === 'encryption-key' && sdk) {
      await sdk.prepareEncryption();
      result = { ok: true };
    } else if (req.command === 'encrypt' && sdk) {
      const resultInput = await sdk.encrypt(req.contract, req.user, 18446744073709551615n);
      result = { valid: /^0x[0-9a-fA-F]{64}$/.test(resultInput.handle) && resultInput.inputProof.length > 2 };
    } else if (req.command === 'quit') {
      sdk?.dispose();
      // The SDK's native TFHE pool outlives dispose(). This dedicated child has
      // finished all requests; flush its acknowledgement, then terminate it.
      process.stdout.write('{"ok":true}\n', () => process.exit(0));
      break;
    } else { result = { error: 'invalid_command' }; }
  } catch { result = { error: 'sdk_compatibility_failed' }; }
  process.stdout.write(`${JSON.stringify(result)}\n`);
}
