import { Wallet, getAddress, ZeroAddress } from 'ethers';
import { createServer } from 'node:http';
import { pathToFileURL } from 'node:url';

export const SOURCE = 'https://api.kraken.com/0/public/Ticker?pair=ZECUSD';
export const MAX_PRICE = 100000000000000n;
export const CHAIN_ID = '11155111';
export const TYPES = { PriceQuote: [
  { name: 'priceE8', type: 'uint64' }, { name: 'observedAt', type: 'uint64' },
  { name: 'expiresAt', type: 'uint64' },
] };
export type Quote = { priceE8: string; observedAt: string; expiresAt: string };
export const domain = (swap: string) => ({ name: 'CWZECSwap', version: '1', chainId: CHAIN_ID, verifyingContract: getAddress(swap) });
export async function signQuote(wallet: Wallet, swap: string, quote: Quote) {
  return { quote, signature: await wallet.signTypedData(domain(swap), TYPES, quote), chainId: CHAIN_ID, swapAddress: getAddress(swap) };
}

// Preserve all source precision until the final midpoint division.
export function decimal(value: unknown): { units: bigint; scale: bigint } {
  if (typeof value !== 'string' || value.length > 96 || !/^\d+(\.\d+)?$/.test(value)) throw Error('Invalid decimal');
  const [whole, fraction = ''] = value.split('.');
  return { units: BigInt(whole + fraction), scale: 10n ** BigInt(fraction.length) };
}
export function midpoint(body: any): bigint {
  if (!Array.isArray(body?.error) || body.error.length !== 0 ||
      !body.result || Object.keys(body.result).length !== 1 || !body.result.XZECZUSD) throw Error('Invalid Kraken response');
  const ticker = body.result.XZECZUSD;
  if (!Array.isArray(ticker.b) || !Array.isArray(ticker.a)) throw Error('Missing bid/ask');
  const b = decimal(ticker.b[0]), a = decimal(ticker.a[0]);
  if (b.units <= 0n || a.units <= 0n || b.units * a.scale > a.units * b.scale ||
      b.units * 100000000n > MAX_PRICE * b.scale || a.units * 100000000n > MAX_PRICE * a.scale) throw Error('Invalid bid/ask');
  const price = (b.units * a.scale + a.units * b.scale) * 100000000n / (2n * b.scale * a.scale);
  if (price <= 0n || price > MAX_PRICE) throw Error('Unsupported price');
  return price;
}
export async function fetchPrice(fetcher: typeof fetch = fetch, timeoutMs = 5000): Promise<bigint> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const response = await fetcher(SOURCE, { signal: controller.signal, cache: 'no-store', redirect: 'error' });
    if (!response.ok || !response.body) throw Error('Kraken HTTP error');
    const reader = response.body.getReader();
    let size = 0;
    const chunks: Uint8Array[] = [];
    try {
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        size += value.byteLength;
        if (size > 16384) throw Error('Kraken response too large');
        chunks.push(value);
      }
    } finally { await reader.cancel(); }
    return midpoint(JSON.parse(Buffer.concat(chunks).toString('utf8')));
  } finally { clearTimeout(timer); }
}
export function createQuotes(wallet: Wallet, swap: string, source = fetchPrice, now = () => Date.now()) {
  let cached: Awaited<ReturnType<typeof signQuote>> | undefined;
  let highWater = -Infinity;
  let busy = false;
  function clock() {
    const ms = now();
    if (!Number.isSafeInteger(ms) || ms < 0 || ms < highWater) {
      cached = undefined;
      throw Error('Clock moved backward or is invalid');
    }
    highWater = ms;
    return BigInt(Math.floor(ms / 1000));
  }
  return {
    async refresh() {
      if (busy) return false;
      busy = true;
      try {
        clock();
        const price = await source();
        const observed = clock();
        if (price <= 0n || price > MAX_PRICE) throw Error('Unsupported price');
        const signed = await signQuote(wallet, swap, { priceE8: String(price), observedAt: String(observed), expiresAt: String(observed + 60n) });
        clock();
        cached = signed;
        return true;
      } catch { return false; } finally { busy = false; }
    },
    current() {
      try { return cached && clock() < BigInt(cached.quote.expiresAt) ? cached : undefined; }
      catch { return undefined; }
    },
  };
}
export function quoteServer(quotes: Pick<ReturnType<typeof createQuotes>, 'current'>, origin?: string) {
  return createServer((req, res) => {
    res.setHeader('Cache-Control', 'no-store');
    if (origin) res.setHeader('Vary', 'Origin');
    if (origin && req.headers.origin === origin) {
      res.setHeader('Access-Control-Allow-Origin', origin);
    }
    if (req.method !== 'GET' || req.url !== '/quote') { res.writeHead(404).end(); return; }
    const quote = quotes.current();
    res.writeHead(quote ? 200 : 503, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify(quote ?? { error: 'Price unavailable' }));
  });
}
function main() {
  // Only the dedicated price configuration is read; no wallet files or other service env files.
  const key = process.env.PRICE_SIGNING_KEY;
  const swap = getAddress(process.env.SWAP_ADDRESS ?? '');
  if (!key || !/^0x[0-9a-fA-F]{64}$/.test(key) || swap === ZeroAddress) throw Error('Invalid configuration');
  const wallet = new Wallet(key);
  const port = process.env.PORT ?? '8787';
  if (!/^\d+$/.test(port) || +port < 1 || +port > 65535) throw Error('Invalid port');
  const origin = process.env.FRONTEND_ORIGIN;
  if (origin && (!['https:', 'http:'].includes(new URL(origin).protocol) || new URL(origin).origin !== origin)) throw Error('Invalid origin');
  const quotes = createQuotes(wallet, swap);
  const server = quoteServer(quotes, origin);
  let stopped = false;
  let timer: NodeJS.Timeout;
  async function poll() {
    if (!await quotes.refresh()) console.error('Price refresh failed');
    if (!stopped) timer = setTimeout(poll, 15000);
  }
  server.listen(+port, process.env.BIND_HOST ?? '127.0.0.1');
  server.on('error', () => { console.error('Price HTTP server failed'); process.exit(1); });
  void poll();
  for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => {
    stopped = true; clearTimeout(timer); server.close();
  });
}
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try { main(); } catch { console.error('Invalid price-service configuration'); process.exitCode = 1; }
}
