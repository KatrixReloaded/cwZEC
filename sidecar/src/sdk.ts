import { MemoryStorage, ZamaSDK, createConfig, type EIP712TypedData } from '@zama-fhe/sdk';
import { EthersProvider } from '@zama-fhe/sdk/ethers';
import { node } from '@zama-fhe/sdk/node';
import { sepolia } from '@zama-fhe/sdk/chains';
import { FetchRequest, JsonRpcProvider, TypedDataEncoder, ZeroAddress, getAddress, verifyTypedData } from 'ethers';

const RPC_TIMEOUT_MS = 15_000;
const RELAYER_TIMEOUT_MS = 120_000;
const MAX_UINT64 = (1n << 64n) - 1n;

type Keypair = Awaited<ReturnType<ZamaSDK['relayer']['generateTransportKeyPair']>>;
export type PreparedPermit = {
  keypair: Keypair;
  eip712: EIP712TypedData;
  publicKey: string;
  startTimestamp: string;
  digest: string;
};
export type DecryptionInput = {
  contract: string;
  user: string;
  handle: string;
  keypair: Keypair;
  eip712: EIP712TypedData;
};

export class SdkError extends Error {
  readonly code: string;
  constructor(code: string) {
    super(code);
    this.name = 'SdkError';
    this.code = code;
  }
}

function address(value: string): `0x${string}` {
  try {
    const result = getAddress(value);
    if (result === ZeroAddress) throw new Error();
    return result as `0x${string}`;
  } catch {
    throw new SdkError('invalid_address');
  }
}

function handle(value: string): `0x${string}` {
  if (!/^0x[0-9a-fA-F]{64}$/.test(value) || /^0x0{64}$/.test(value)) {
    throw new SdkError('invalid_handle');
  }
  return value.toLowerCase() as `0x${string}`;
}

function uint64(value: unknown): asserts value is bigint {
  if (typeof value !== 'bigint' || value < 0n || value > MAX_UINT64) {
    throw new SdkError('invalid_uint64');
  }
}

/** Only the pinned backend's local *user* ACL denial is an authorization result. */
export function classifyDecryptionError(error: unknown, user: string, ciphertext: string): 'sdk_acl_denied' | 'decrypt_failed' {
  // @fhevm/sdk 0.13.2 overwrites the subtype's `name` in FhevmErrorBase.
  // Contract denial uses the same class; matching its exact user message matters.
  if (error instanceof Error && error.constructor.name === 'AclUserDecryptionError' &&
      error.name === 'FhevmErrorBase' && 'contractName' in error && error.contractName === 'ACL' &&
      error.message === `User ${user} is not authorized to decrypt handle ${ciphertext}!\nVersion: @fhevm/sdk@0.13.2`) {
    return 'sdk_acl_denied';
  }
  return 'decrypt_failed';
}

function signingParts(eip712: EIP712TypedData) {
  const { EIP712Domain: _domain, ...types } = eip712.types;
  return {
    domain: eip712.domain,
    types: Object.fromEntries(Object.entries(types).map(([name, fields]) => [name, [...fields]])),
    message: eip712.message,
  };
}

/** A read-only public Sepolia client. No signer or persistent credential storage. */
export function createSdk(rpcUrl: string) {
  let url: URL;
  try {
    url = new URL(rpcUrl);
    if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password) throw new Error();
  } catch {
    throw new SdkError('invalid_rpc_url');
  }
  const request = new FetchRequest(url.href);
  request.timeout = RPC_TIMEOUT_MS;
  const provider = new JsonRpcProvider(request, undefined, { batchMaxCount: 1 });
  // Both SDK-level reads and backend ACL/version reads use the bounded transport.
  const network = {
    // This fixed-chain read-only provider has no wallet/account event source.
    on: () => {},
    removeListener: () => {},
    request: ({ method, params }: { method: string; params?: unknown }) => {
      if (params !== undefined && !Array.isArray(params)) return Promise.reject(new SdkError('invalid_rpc_params'));
      return provider.send(method, params === undefined ? [] : [...params as readonly unknown[]]);
    },
  };
  let sdk: ZamaSDK;
  try {
    sdk = new ZamaSDK(createConfig({
      chains: [{ ...sepolia, network }],
      provider: new EthersProvider({ provider }),
      storage: new MemoryStorage(),
      relayers: { [sepolia.id]: node({ timeout: RELAYER_TIMEOUT_MS, debug: false }) },
      telemetry: false,
    }));
  } catch {
    provider.destroy();
    throw new SdkError('sdk_init_failed');
  }
  const shutdown = new AbortController();
  let disposed = false;
  const ready = () => { if (disposed) throw new SdkError('sdk_disposed'); };
  const options = () => ({
    timeout: RELAYER_TIMEOUT_MS,
    signal: AbortSignal.any([shutdown.signal, AbortSignal.timeout(RELAYER_TIMEOUT_MS)]),
    fetchRetries: 0,
  });

  return {
    // Warm the shared public-key cache before accepting encryption work. The
    // SDK's encrypt timeout starts after this lazy initialization step.
    async prepareEncryption() {
      ready();
      let timer: ReturnType<typeof setTimeout> | undefined;
      const abort = new AbortController();
      try {
        await Promise.race([
          sdk.relayer.fetchFheEncryptionKeyBytes({ options: {
            signal: AbortSignal.any([shutdown.signal, abort.signal]), fetchRetries: 0,
          } }),
          new Promise<never>((_, reject) => {
            timer = setTimeout(() => {
              abort.abort();
              reject(new SdkError('encryption_key_timeout'));
            }, RELAYER_TIMEOUT_MS);
          }),
        ]);
      } catch (error) {
        if (error instanceof SdkError && error.code === 'encryption_key_timeout') throw error;
        throw new SdkError('encryption_key_failed');
      } finally {
        clearTimeout(timer);
      }
    },
    async encrypt(contract: string, user: string, value: bigint) {
      ready();
      const contractAddress = address(contract);
      const userAddress = address(user);
      uint64(value);
      try {
        const result = await sdk.encrypt({ contractAddress, userAddress, values: [{ type: 'euint64', value }] }, options());
        if (result.encryptedValues.length !== 1 || !/^0x(?:[0-9a-fA-F]{2})+$/.test(result.inputProof)) {
          throw new Error();
        }
        return { handle: handle(result.encryptedValues[0]), inputProof: result.inputProof };
      } catch {
        throw new SdkError('encrypt_failed');
      }
    },
    async prepare(contract: string, user: string): Promise<PreparedPermit> {
      ready();
      const contractAddress = address(contract);
      if (address(user) === contractAddress) throw new SdkError('invalid_user');
      try {
        const keypair = await sdk.relayer.generateTransportKeyPair();
        const startTimestamp = Math.floor(Date.now() / 1000);
        const raw = await sdk.relayer.createUnsignedLegacyDecryptionPermitEip712({
          transportKeyPair: keypair, contractAddresses: [contractAddress], startTimestamp, durationSeconds: 86_400,
        });
        // Decimal strings preserve uint256 values for Rust's independent digest check.
        const eip712: EIP712TypedData = JSON.parse(JSON.stringify(raw, (_key, value) => typeof value === 'bigint' ? value.toString() : value));
        eip712.domain.chainId = String(eip712.domain.chainId);
        eip712.message.startTimestamp = String(eip712.message.startTimestamp);
        eip712.message.durationDays = String(eip712.message.durationDays);
        const parts = signingParts(eip712);
        return { keypair, eip712, publicKey: keypair.publicKey, startTimestamp: String(startTimestamp),
          digest: TypedDataEncoder.hash(parts.domain, parts.types, parts.message) };
      } catch {
        throw new SdkError('permit_failed');
      }
    },
    async decrypt(input: DecryptionInput, signature: string): Promise<bigint> {
      ready();
      const contractAddress = address(input.contract);
      const userAddress = address(input.user);
      const ciphertext = handle(input.handle);
      if (contractAddress === userAddress) throw new SdkError('invalid_user');
      if (!/^0x[0-9a-fA-F]{130}$/.test(signature)) throw new SdkError('invalid_signature');
      let recovered: string;
      try {
        const parts = signingParts(input.eip712);
        recovered = verifyTypedData(parts.domain, parts.types, parts.message, signature);
      } catch {
        throw new SdkError('invalid_signature');
      }
      if (recovered !== userAddress) throw new SdkError('signature_user_mismatch');
      try {
        const signedPermit = await sdk.relayer.parseSignedDecryptionPermit({
          transportKeyPair: input.keypair,
          serializedPermit: { version: 1, eip712: input.eip712, signature, signerAddress: userAddress },
        });
        const values = await sdk.relayer.decryptValuesFromPairs({
          pairs: [{ encryptedValue: ciphertext, contractAddress }],
          transportKeyPair: input.keypair, signedPermit, options: options(),
        });
        if (values.length !== 1 || values[0].type !== 'uint64') throw new Error();
        uint64(values[0].value);
        return values[0].value;
      } catch (error) {
        throw new SdkError(classifyDecryptionError(error, userAddress, ciphertext));
      }
    },
    dispose() {
      if (disposed) return;
      disposed = true;
      shutdown.abort();
      try { sdk.terminate(); } finally { provider.destroy(); }
    },
  };
}

export type SdkAdapter = ReturnType<typeof createSdk>;
