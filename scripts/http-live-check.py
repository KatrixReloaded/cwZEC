#!/usr/bin/env python3
"""Read-only HTTP comparison with existing live evidence. Never submits a request body."""
import json
import os
import pathlib
import sys
from api_request import request


def main():
    root = pathlib.Path(__file__).resolve().parents[1]
    previous = json.loads((root / '.local/backed-roundtrip-evidence.json').read_text())
    recovery = json.loads((root / '.local/bridge-recovery-evidence.json').read_text())
    info = request('/v1/info')
    deposits = request('/v1/deposits/' + previous['deposit_transaction'])
    redeem = request('/v1/redeems/' + previous['redeem_id'])
    recredit = request('/v1/redeems/' + recovery['redeem_id'])
    status = request('/v1/admin/status')
    accounting = request('/v1/admin/reconcile')
    blocked = request('/v1/admin/recovery')
    deposit = next(d for d in deposits['deposits'] if d['deposit_id'] == previous['deposit_id'])
    refund = next(f for f in status['refunds'] if f['deposit_id'] == recovery['deposit_id'])
    public = json.dumps([info, deposits, redeem, recredit])
    checks = {
        'active_saved_domain': info['chain_id'] == str(previous['chain_id']) and info['contract'].lower() == previous['contract'].lower(),
        'confirmation_policy': info['zcash_confirmations'] == 3 and info['evm_confirmations'] == 2,
        'saved_exact_mint': deposit['status'] == 'minted' and deposit['mint_transaction'] == previous['mint_transaction'] and deposit['evm_confirmations'] >= 2,
        'saved_paid_and_fulfilled': redeem['status'] == 'fulfilled' and redeem['payment']['status'] == 'paid' and redeem['payment']['txid'] == previous['payout_transaction'] and redeem['fulfillment']['settled'] and redeem['fulfillment']['transaction'] == previous['fulfillment_transaction'],
        'saved_recredit_without_payment': recredit['status'] == 're_credited' and recredit['payment'] is None and recredit['recredit']['settled'] and recredit['recredit']['transaction'] == recovery['recredit_transaction'],
        'saved_refund_paid': refund['state'] == 'Paid' and refund['payment']['status'] == 'paid' and refund['payment']['txid'] == recovery['refund_transaction'] and refund['void_transaction'] == recovery['void_transaction'],
        'accounting_checkpoint_visible': int(accounting['report']['checkpoint_id']) >= recovery['accounting_checkpoint'] and accounting['report']['coverage_complete'] and accounting['report']['valid'] and not accounting['persisted']['halted'],
        'exact_accounting_strings': all(isinstance(accounting['report'][k], str) and accounting['report'][k].isdigit() for k in ('supply_zat','assets_zat','deposit_liability_zat','unpaid_redemptions_zat','fee_reserve_zat')),
        'recovery_disabled': blocked == {'enabled': False, 'reason': 'blocked_historical_absence_unproven'},
        'public_fields_private': not any('"' + k + '"' in public for k in ('actual','amount','destination','recipient','memo','rho','rseed','signed_raw','permit','assets_zat','supply_zat')),
        'prior_evidence_preserved': all(previous['checks'].values()) and all(recovery['checks'].values()),
    }
    if not all(checks.values()):
        raise ValueError('comparison failed')
    result = {'kind': 'live_read_only_http', 'checks': checks,
              'accounting_checkpoint': accounting['report']['checkpoint_id'],
              'accounting_current': accounting['persisted']['report_current'],
              'prior_roundtrip_checkpoint': previous['accounting_checkpoint'],
              'prior_recovery_checkpoint': recovery['accounting_checkpoint']}
    path = root / '.local/http-readonly-evidence.json'
    if path.exists():
        if json.loads(path.read_text())['checks'] != checks:
            raise ValueError('existing evidence differs')
    else:
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, 'w') as file:
            json.dump(result, file, indent=2)
            file.flush()
            os.fsync(file.fileno())
    print(json.dumps(result, indent=2))


if __name__ == '__main__':
    try:
        main()
    except (ValueError, KeyError, OSError, StopIteration):
        sys.exit('Read-only HTTP comparison failed/unavailable; prior evidence and journals unchanged.')
