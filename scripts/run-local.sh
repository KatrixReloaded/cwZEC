#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
umask 077
if [ "${1:-}" != legacy-wallet ]; then
    if [ ! -s .local/backed.env ] || [ ! -s .local/backed-deployment.json ]; then
        echo 'Verified backed configuration and deployment required. See README clean setup; an empty database cannot authorize bridge payments.' >&2
        exit 1
    fi
    export BRIDGE_AUTO_BIN="${BRIDGE_AUTO_BIN:-./service/target/release/bridge-auto}"
    exec ./scripts/bridge-backed.sh run "$@"
fi
shift
# Historical standalone wallet demonstration only; never use for a backed domain.
mkdir -p .local
if [ ! -f .local/runtime.env ]; then
    python3 - <<'PY'
import getpass, os, pathlib, secrets, shlex
root = pathlib.Path.cwd()
values = {
    'DATABASE_URL': os.environ.get('DATABASE_URL', f'postgresql://{getpass.getuser()}@127.0.0.1:5432/cwzec_milestone'),
    'VAULT_KEY_PATH': str(root / '.local/vault.seed'),
    'ADMIN_API_KEY': secrets.token_hex(32),
    'ZCASH_LIGHTWALLETD_URL': 'https://testnet.zec.rocks:443',
    'CONFIRMATIONS': '3',
}
with (root / '.local/runtime.env').open('x') as f:
    for key, value in values.items():
        f.write(f'export {key}={shlex.quote(value)}\n')
PY
fi
. ./.local/runtime.env
exec ./service/target/release/app
