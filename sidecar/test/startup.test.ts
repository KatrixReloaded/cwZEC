import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';

test('startup forwards only the explicit RPC and port configuration to Node', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'cwzec-startup-'));
  try {
    const fakeNode = join(dir, 'node');
    await writeFile(fakeNode, '#!/bin/sh\n/usr/bin/env\n', { mode: 0o700 });
    const result = await promisify(execFile)('/bin/bash', [fileURLToPath(new URL('../../scripts/sidecar-start.sh', import.meta.url))], {
      env: { ...process.env, NODE_BIN: fakeNode, NODE_OPTIONS: '--require=secret', PRIVATE_KEY: 'test-only', KEY_PATH: '/test-only', DATABASE_URL: 'test-only', SEPOLIA_RPC_URL: 'https://rpc.example', SIDECAR_PORT: '8789' },
    });
    const environment = Object.fromEntries(result.stdout.trim().split('\n').map(line => {
      const index = line.indexOf('='); return [line.slice(0, index), line.slice(index + 1)];
    }));
    assert.equal(environment.SEPOLIA_RPC_URL, 'https://rpc.example');
    assert.equal(environment.SIDECAR_PORT, '8789');
    for (const name of ['NODE_OPTIONS', 'PRIVATE_KEY', 'KEY_PATH', 'DATABASE_URL', 'HOME', 'NODE_BIN']) assert.equal(environment[name], undefined);
    assert.ok(Object.keys(environment).every(key => ['SEPOLIA_RPC_URL', 'SIDECAR_PORT', 'PWD', 'SHLVL', '_'].includes(key)));
  } finally { await rm(dir, { recursive: true, force: true }); }
});

test('startup resolves Node from PATH and defaults to the separate sidecar port', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'cwzec-startup-path-'));
  try {
    await writeFile(join(dir, 'node'), '#!/bin/sh\n/usr/bin/env\n', { mode: 0o700 });
    const result = await promisify(execFile)('/bin/bash', [fileURLToPath(new URL('../../scripts/sidecar-start.sh', import.meta.url))], {
      env: { PATH: `${dir}:/usr/bin:/bin` },
    });
    assert.match(result.stdout, /^SIDECAR_PORT=8788$/m);
    assert.doesNotMatch(result.stdout, /^PATH=/m);
  } finally { await rm(dir, { recursive: true, force: true }); }
});
