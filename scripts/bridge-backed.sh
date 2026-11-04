#!/bin/sh
# One isolated testnet domain. Never sources or changes the old runtime.env.
set -eu
cd "$(dirname "$0")/.."
umask 077
mkdir -p .local
if [ ! -f .local/backed.env ]; then
    python3 - <<'PY'
import getpass, pathlib, secrets, shlex
root = pathlib.Path.cwd()
values = {
 'DATABASE_URL': f'postgresql://{getpass.getuser()}@127.0.0.1:5432/cwzec_backed_roundtrip',
 'VAULT_KEY_PATH': str(root/'.local/backed-vault.seed'),
 'ADMIN_API_KEY': secrets.token_hex(32),
 'ZCASH_LIGHTWALLETD_URL': 'https://testnet.zec.rocks:443',
 'CONFIRMATIONS': '3', 'ZAMA_RPC_URL': 'https://ethereum-sepolia-rpc.publicnode.com',
 'ZAMA_CHAIN_ID': '11155111',
 'BRIDGE_SERVICE_KEY_PATH': str(root/'.local/zama-probe.key'),
 'BRIDGE_AUDITOR_KEY_PATH': str(root/'.local/cwzec-auditor.key'),
 'BRIDGE_DESTINATION_KEY_PATH': str(root/'.local/backed-destination.key'),
 'SIDECAR_URL': 'http://127.0.0.1:8788',
}
with (root/'.local/backed.env').open('x') as f:
 for key, value in values.items(): f.write(f'export {key}={shlex.quote(value)}\n')
PY
fi
. ./.local/backed.env
if [ -f .local/backed-deployment.json ]; then
    # Only validated public identities, never arbitrary shell fragments.
    eval "$(python3 - <<'PY'
import json, re
j=json.load(open('.local/backed-deployment.json'))
a=j['contract']; assert re.fullmatch(r'0x[0-9a-fA-F]{40}', a)
print('export ZAMA_CONTRACT='+a)
if j['deployment_block'] is not None: print('export ZAMA_DEPLOYMENT_BLOCK='+str(int(j['deployment_block'])))
PY
)"
fi
command=${1:-run}
if [ "$#" -gt 0 ]; then shift; fi
case "$command" in
 domain) exec ./service/target/debug/bridge-domain "$@" ;;
 run) exec "${BRIDGE_AUTO_BIN:-./service/target/debug/bridge-auto}" "$@" ;;
 api) exec python3 scripts/api_request.py "$@" ;;
 user) exec ./service/target/debug/bridge-user "$@" ;;
 recovery) exec python3 scripts/recovery_request.py "$@" ;;
 acceptance) exec ./service/target/debug/bridge-acceptance "$@" ;;
 *) echo 'Use api /v1/path or prepare ID, domain prepare|submit, run [once], user prepare|submit|adopt|recover, acceptance, or recovery status|recredit|refund|death|finish' >&2; exit 2 ;;
esac
