// Test-only stand-in for the SDK worker pool that otherwise outlives disposal.
import { Worker } from 'node:worker_threads';
new Worker('setInterval(() => {}, 1000)', { eval: true });
