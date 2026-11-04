import { test } from 'node:test';
import assert from 'node:assert/strict';
import { Wallet, verifyTypedData } from 'ethers';
import { createServer } from 'node:http';
import { decimal, midpoint, fetchPrice, createQuotes, quoteServer, domain, TYPES, SOURCE, signQuote, MAX_PRICE } from '../dist/service.js';
const wallet = new Wallet('0x'+'11'.repeat(32)); // synthetic test key only
const swap = '0x1111111111111111111111111111111111111111';
const ticker = (b='50',a='51') => ({error:[],result:{XZECZUSD:{b:[b,'1','1.000'],a:[a,'1','1.000']}}});

test('exact decimals, full precision midpoint and floor rounding',()=>{
  assert.deepEqual(decimal('001.000000001'),{units:1000000001n,scale:1000000000n});
  assert.equal(midpoint(ticker('50','51')),5050000000n);
  assert.equal(midpoint(ticker('1.000000009','1.000000011')),100000001n);
  assert.equal(midpoint(ticker('1','1.00000001')),100000000n);
  assert.equal(midpoint(ticker('1000000','1000000')),MAX_PRICE);
  assert.equal(midpoint(ticker('0.000000001','0.000000019')),1n);
});
test('rejects malformed, crossed, zero, unsupported and Kraken error responses',()=>{
  for(const bad of ['',null,1,'1e2','-1','+1','.5','1.',' 1','1'.repeat(97)]) assert.throws(()=>decimal(bad));
  for(const body of [null,{}, {error:['EGeneral:failure'],result:{}}, {error:[],result:{}},
    {error:[],result:{other:ticker().result.XZECZUSD}},
    {error:[],result:{XZECZUSD:{a:'50',b:['49']}}},
    ticker('52','51'),ticker('0','1'),ticker('1','0'),ticker('1','1000000.000000001'),
    ticker('0.000000001','0.000000001'),ticker('bad','1')]) assert.throws(()=>midpoint(body));
});
test('fetch uses fixed URL, no cache, refuses redirects, HTTP errors, malformed and oversized bodies',async()=>{
  assert.equal(await fetchPrice(async(url,opts)=>{
    assert.equal(url,SOURCE); assert.equal(opts.cache,'no-store'); assert.equal(opts.redirect,'error');
    return Response.json(ticker());
  }),5050000000n);
  for(const response of [new Response('',{status:500}),new Response('{'),Response.json({error:['error']}),new Response(' '.repeat(16385))]) {
    await assert.rejects(fetchPrice(async()=>response));
  }
  // Bound the decoded stream even when no Content-Length is supplied.
  let cancelled=false;
  await assert.rejects(fetchPrice(async()=>new Response(new ReadableStream({pull(c){c.enqueue(new Uint8Array(17000));},cancel(){cancelled=true;}}))));
  assert.equal(cancelled,true);
});
test('timeout bounds headers and a stalled response body',async()=>{
  for(const body of [false,true]) {
    const server=createServer((_req,res)=>{if(body){res.writeHead(200);res.write('{');}});
    await new Promise(r=>server.listen(0,'127.0.0.1',r));
    try { await assert.rejects(fetchPrice((_url,opts)=>fetch(`http://127.0.0.1:${server.address().port}`,opts),40)); }
    finally { server.closeAllConnections(); await new Promise(r=>server.close(r)); }
  }
});
test('startup, successful refresh, failed refresh, exact expiry and recovery without freshness extension',async()=>{
  let now=100000, fail=false, calls=0;
  const quotes=createQuotes(wallet,swap,async()=>{calls++;if(fail)throw Error();return 5000000000n;},()=>now);
  assert.equal(quotes.current(),undefined);
  assert.equal(await quotes.refresh(),true);
  const first=quotes.current(); assert.equal(first.quote.observedAt,'100'); assert.equal(first.quote.expiresAt,'160');
  fail=true; now=120000; assert.equal(await quotes.refresh(),false); assert.deepEqual(quotes.current(),first);
  now=159999; assert.deepEqual(quotes.current(),first);
  now=160000; assert.equal(quotes.current(),undefined);
  assert.equal(await quotes.refresh(),false); assert.equal(quotes.current(),undefined);
  fail=false; now=170000; assert.equal(await quotes.refresh(),true);
  assert.equal(quotes.current().quote.observedAt,'170'); assert.equal(calls,4);
});
test('non-overlapping refresh, timestamp after successful observation only',async()=>{
  let resolve, calls=0, now=100000;
  const quotes=createQuotes(wallet,swap,()=>{calls++;return new Promise(r=>resolve=r);},()=>now);
  const pending=quotes.refresh(); assert.equal(await quotes.refresh(),false); assert.equal(calls,1);
  now=105000; resolve(5000000000n); assert.equal(await pending,true);
  assert.equal(quotes.current().quote.observedAt,'105');
});
test('backward clock invalidates cache and refuses fresh signatures until clock recovers',async()=>{
  let now=100000;
  const quotes=createQuotes(wallet,swap,async()=>5000000000n,()=>now);
  await quotes.refresh(); now=99999;
  assert.equal(quotes.current(),undefined); assert.equal(await quotes.refresh(),false);
  now=100001; assert.equal(quotes.current(),undefined);
  assert.equal(await quotes.refresh(),true);
  let finish;
  const during=createQuotes(wallet,swap,()=>new Promise(r=>finish=r),()=>now);
  const pending=during.refresh(); now=100000; finish(5000000000n);
  assert.equal(await pending,false); assert.equal(during.current(),undefined);
});
test('signature recovery and exact domain/field encoding',async()=>{
  const q={priceE8:'5000000000',observedAt:'100',expiresAt:'160'};
  const signed=await signQuote(wallet,swap,q);
  assert.equal(verifyTypedData(domain(swap),TYPES,q,signed.signature),wallet.address);
  assert.equal(signed.chainId,'11155111'); assert.equal(signed.swapAddress,swap);
  assert.deepEqual(domain(swap),{name:'CWZECSwap',version:'1',chainId:'11155111',verifyingContract:swap});
  assert.deepEqual(TYPES.PriceQuote.map(x=>x.type),['uint64','uint64','uint64']);
  for(const field of Object.values(signed.quote)) assert.equal(typeof field,'string');
});
test('HTTP 503/200/expiry/recovery, no-store, exact optional origin and no credentials',async()=>{
  let now=100000;
  const quotes=createQuotes(wallet,swap,async()=>5000000000n,()=>now);
  const server=quoteServer(quotes,'https://frontend.example');
  await new Promise(r=>server.listen(0,'127.0.0.1',r));
  const url=`http://127.0.0.1:${server.address().port}/quote`;
  try {
    let r=await fetch(url); assert.equal(r.status,503); assert.equal(r.headers.get('cache-control'),'no-store');
    await quotes.refresh(); r=await fetch(url,{headers:{Origin:'https://frontend.example'}});
    assert.equal(r.status,200); assert.deepEqual(await r.json(),quotes.current());
    assert.equal(r.headers.get('access-control-allow-origin'),'https://frontend.example');
    assert.equal(r.headers.get('access-control-allow-credentials'),null);
    r=await fetch(url,{headers:{Origin:'https://other.example'}}); assert.equal(r.headers.get('access-control-allow-origin'),null);
    now=160000; assert.equal((await fetch(url)).status,503);
    await quotes.refresh(); assert.equal((await fetch(url)).status,200);
    assert.equal((await fetch(url,{method:'POST'})).status,404);
    assert.equal((await fetch(url+'/other')).status,404);
  } finally { await new Promise(r=>server.close(r)); }
});
