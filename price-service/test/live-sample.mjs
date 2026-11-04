// Explicit read-only external request; never run as part of offline unit tests.
import { Wallet, verifyTypedData } from 'ethers';
import { fetchPrice, signQuote, domain, TYPES } from '../dist/service.js';
const wallet=new Wallet('0x'+'11'.repeat(32)); // synthetic; never configure a deployed pool with this key
const swap='0x1111111111111111111111111111111111111111';
const price=await fetchPrice();
const now=BigInt(Math.floor(Date.now()/1000));
const signed=await signQuote(wallet,swap,{priceE8:String(price),observedAt:String(now),expiresAt:String(now+60n)});
if(verifyTypedData(domain(swap),TYPES,signed.quote,signed.signature)!==wallet.address)throw Error('Recovery failed');
console.log(JSON.stringify({syntheticKey:true,localVerification:true,...signed}));
