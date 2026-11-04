#!/bin/sh
# One user-authorized rejected burn in the existing backed acceptance domain.
set -eu
cd "$(dirname "$0")/.."
umask 077
. ./.local/backed.env
request=.local/recovery-wallet-transaction.json
signed=.local/recovery-user-signed.txt
if [ ! -e "$signed" ]; then
    ./scripts/bridge-backed.sh acceptance prepare-rejected > "$request"
    jq -e '.requestId == "recovery/rejected"' "$request" >/dev/null
    if jq -e '.alreadySubmitted == true' "$request" >/dev/null; then
        exec ./scripts/bridge-backed.sh user recover recovery/rejected
    fi
    expected=$(jq -er '.contract' .local/backed-deployment.json)
    jq -e --arg contract "$expected" '.transaction.chainId == "0xaa36a7" and .transaction.to == $contract and .transaction.value == "0x0"' "$request" >/dev/null
    sender=$(jq -er '.transaction.from' "$request")
    target=$(jq -er '.transaction.to' "$request")
    data=$(jq -er '.transaction.data' "$request")
    estimate=$(cast estimate "$target" "$data" --from "$sender" --rpc-url "$ZAMA_RPC_URL")
    price=$(cast gas-price --rpc-url "$ZAMA_RPC_URL")
    case "$estimate" in ''|*[!0-9]*) exit 1 ;; esac
    case "$price" in ''|*[!0-9]*) exit 1 ;; esac
    [ "$estimate" -gt 0 ]
    [ "$estimate" -le 12000000 ]
    [ "$price" -gt 0 ]
    [ "$price" -le 9999500000 ]
    gas_limit=$((estimate + estimate / 3 + 100000))
    max_fee=$((price * 2 + 1000000))
    echo 'Sign the one-zatoshi recovery burn with your existing user keystore.'
    # Noclobber also prevents concurrent invocations from signing over saved bytes.
    (
        set -C
        cast mktx "$target" "$data" --account "${1:-personal}" \
            --from "$sender" --chain 11155111 --rpc-url "$ZAMA_RPC_URL" \
            --gas-limit "$gas_limit" --gas-price "$max_fee" \
            --priority-gas-price 1000000 > "$signed"
    )
fi
# An empty/interrupted file requires inspection, never automatic signing again.
[ -s "$signed" ]
exec ./scripts/bridge-backed.sh user submit recovery/rejected "$signed"
