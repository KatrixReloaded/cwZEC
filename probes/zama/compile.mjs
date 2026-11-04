import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { readFileSync } from 'node:fs';
import { dirname, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import solc from 'solc';

const here = dirname(fileURLToPath(import.meta.url));
const modules = resolve(here, 'node_modules');
const input = {
  language: 'Solidity',
  sources: {
    'FheProbe.sol': {
      content: await readFile(resolve(here, '../../contracts/contracts/FheProbe.sol'), 'utf8'),
    },
  },
  settings: {
    optimizer: { enabled: true, runs: 200 },
    evmVersion: 'cancun',
    outputSelection: { '*': { '*': ['abi', 'evm.bytecode.object', 'evm.deployedBytecode.object'] } },
  },
};
const output = JSON.parse(solc.compile(JSON.stringify(input), {
  import(path) {
    const target = resolve(modules, path);
    if (!target.startsWith(modules + sep)) return { error: 'import_outside_node_modules' };
    try {
      return { contents: readFileSync(target, 'utf8') };
    } catch {
      return { error: 'import_not_found' };
    }
  },
}));
const errors = (output.errors ?? []).filter((e) => e.severity === 'error');
if (errors.length) {
  for (const error of errors) process.stderr.write(`${error.formattedMessage}\n`);
  process.exitCode = 1;
} else {
  const contract = output.contracts['FheProbe.sol'].FheProbe;
  const artifact = {
    abi: contract.abi,
    bytecode: `0x${contract.evm.bytecode.object}`,
    deployedBytecode: `0x${contract.evm.deployedBytecode.object}`,
    compiler: solc.version(),
  };
  await mkdir(resolve(here, 'artifacts'), { recursive: true });
  await writeFile(resolve(here, 'artifacts/FheProbe.json'), `${JSON.stringify(artifact, null, 2)}\n`);
  process.stdout.write('compile_ok\n');
}
