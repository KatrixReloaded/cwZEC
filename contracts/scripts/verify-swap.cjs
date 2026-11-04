// Read-only Sepolia verification. No keystore or private configuration is read.
const { ethers } = require('ethers');
const fs = require('node:fs');
const path = require('node:path');
(async () => {
  const provider = new ethers.JsonRpcProvider(process.env.SEPOLIA_RPC_URL || 'https://ethereum-sepolia-rpc.publicnode.com');
  const network = await provider.getNetwork();
  if (network.chainId !== 11155111n) throw Error('Not Sepolia');
  for (const [address, getters] of [
    ['0x91cac58cea711bd9487721824ea38e2b5742253f', ['decimals()(uint8)', 'service()(address)', 'auditor()(address)']],
    ['0xE239634d946663EFD2D5fd9F980310E71DFa8856', ['decimals()(uint8)', 'underlying()(address)', 'rate()(uint256)']],
    ['0x89f2D06A3cD63b6c0a2eC14c341b3F8b02cE963c', ['token()(address)', 'service()(address)', 'destinationHash()(bytes32)']],
    ['0xDd24F84d36BF92C65F92307595335bdFab5Bbd21', []],
  ]) {
    const code = await provider.getCode(address);
    if (code === '0x') throw Error(`No code: ${address}`);
    const values = {};
    for (const getter of getters) {
      const fragment = ethers.FunctionFragment.from(`function ${getter.replace(")(", ") returns (")}`);
      const contract = new ethers.Contract(address, [fragment], provider);
      values[fragment.name] = String(await contract[fragment.name].staticCall());
    }
    const names = { '0x91cac58cea711bd9487721824ea38e2b5742253f':'CWZEC', '0xe239634d946663efd2d5fd9f980310e71dfa8856':'CUSDC', '0x89f2d06a3cd63b6c0a2ec14c341b3f8b02ce963c':'CWZECBalancer' };
    const name = names[address.toLowerCase()];
    if (name) {
      const dir = path.join(__dirname, '../artifacts/build-info');
      for (const file of fs.readdirSync(dir)) {
        const build = JSON.parse(fs.readFileSync(path.join(dir,file)));
        const output = build.output.contracts[`contracts/${name}.sol`]?.[name];
        if (!output) continue;
        const compiled = output.evm.deployedBytecode;
        const trim = x => x.slice(0, x.length - 4 - parseInt(x.slice(-4),16)*2);
        const mask = x => {
          x = trim(x.replace(/^0x/,''));
          for (const ranges of Object.values(compiled.immutableReferences)) for (const {start,length} of ranges)
            x=x.slice(0,start*2)+'0'.repeat(length*2)+x.slice((start+length)*2);
          return x;
        };
        values.runtimeMatchesPinnedSource = mask(code) === mask(compiled.object);
        break;
      }
    }
    console.log(JSON.stringify({ chain: String(network.chainId), address, codeHash: ethers.keccak256(code), ...values }));
  }
})().catch(e => { console.error(e.message); process.exitCode = 1; });
