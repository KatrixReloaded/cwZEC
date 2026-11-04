// Suppress dependency diagnostics before importing the SDK. Only fixed startup
// status is written below; requests and SDK payloads never enter logs.
for (const name of ['log', 'info', 'warn', 'error', 'debug', 'trace', 'dir', 'table'] as const) console[name] = () => {};
const { createSdk } = await import('./sdk.ts');
const { createServer } = await import('./http.ts');

try {
  const rawPort = process.env.SIDECAR_PORT ?? '8788';
  if (!/^(0|[1-9][0-9]{0,4})$/.test(rawPort) || Number(rawPort) > 65535) throw new Error();
  const sdk = createSdk(process.env.SEPOLIA_RPC_URL ?? 'https://ethereum-sepolia-rpc.publicnode.com');
  // Initialization completes before the listener can accept encryption requests.
  await sdk.prepareEncryption();
  const server = createServer(sdk);
  const dispose = () => { try { sdk.dispose(); } catch { /* fixed exit status only */ } };
  const stop = (code = 0) => {
    server.closeAllConnections();
    server.close();
    dispose();
    // The native TFHE worker pool survives SDK disposal.
    process.exit(code);
  };
  process.once('SIGINT', () => stop());
  process.once('SIGTERM', () => stop());
  server.once('sdkTimeout', () => {
    // Bound retention even if the SDK leaves a promise and its keys alive.
    setTimeout(() => stop(1), 1000).unref();
  });
  server.once('error', () => {
    dispose();
    process.stderr.write('Sidecar listener failed\n', () => process.exit(1));
  });
  server.listen(Number(rawPort), '127.0.0.1', () => {
    const bound = server.address();
    if (!bound || typeof bound === 'string') throw new Error();
    process.stdout.write(`Sidecar ready on 127.0.0.1:${bound.port}\n`);
  });
} catch {
  process.stderr.write('Sidecar initialization failed\n', () => process.exit(1));
}
