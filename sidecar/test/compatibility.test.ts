import assert from 'node:assert/strict';
import { test } from 'node:test';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';

test('compatibility child flushes quit acknowledgement and exits despite live workers', async () => {
  const child = spawn(process.execPath, [
    '--import', fileURLToPath(new URL('./fixtures/keepalive.ts', import.meta.url)),
    fileURLToPath(new URL('../scripts/compatibility.ts', import.meta.url)),
  ], { env: {}, stdio: ['pipe', 'pipe', 'pipe'] });
  let output = '';
  child.stdout.setEncoding('utf8').on('data', (chunk) => { output += chunk; });
  const timer = setTimeout(() => child.kill('SIGKILL'), 4000);
  try {
    const exited = new Promise<number | null>((resolve, reject) => {
      child.once('error', reject);
      child.once('exit', (code) => resolve(code));
    });
    child.stdin.end('{"command":"quit"}\n');
    assert.equal(await exited, 0);
    assert.deepEqual(JSON.parse(output), { ok: true });
  } finally {
    clearTimeout(timer);
    child.kill();
  }
});
