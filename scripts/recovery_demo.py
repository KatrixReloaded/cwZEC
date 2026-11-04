#!/usr/bin/env python3
"""Drive the real HTTP service through two process exits and one public payout.

Run after a confirmed deposit, with the ordinary service stopped. Amounts and
credentials are handled in memory and never printed. No wallet or SQL logic lives here.
"""
import http.client
import json
import os
import pathlib
import re
import signal
import shlex
import subprocess
import sys
import time
import urllib.error
import urllib.request

root = pathlib.Path(__file__).resolve().parents[1]
if len(sys.argv) != 3:
    sys.exit('usage: scripts/recovery_demo.py PUBLIC_TESTNET_PAYOUT_ADDRESS DEPOSIT_ID')
destination, deposit_id = sys.argv[1:]
config = {}
for line in (root / '.local/runtime.env').read_text().splitlines():
    words = shlex.split(line)
    if len(words) == 2 and words[0] == 'export':
        key, value = words[1].split('=', 1)
        config[key] = value


def api(path, body=None, timeout=120):
    request = urllib.request.Request('http://127.0.0.1:8787' + path,
        data=None if body is None else json.dumps(body).encode(),
        headers={'x-api-key': config['ADMIN_API_KEY'], 'content-type': 'application/json'})
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return json.load(response)


def start(stage):
    env = os.environ.copy()
    env.pop('CWZEC_INTERRUPT_AFTER', None)
    if stage in ('journal', 'broadcast'):
        env['CWZEC_INTERRUPT_AFTER'] = stage
    log_path = root / f'.local/recovery-{stage}.log'
    log = log_path.open('wb')
    os.chmod(log_path, 0o600)
    process = subprocess.Popen([str(root / 'scripts/run-local.sh'), 'legacy-wallet'], cwd=root,
        env=env, stdout=log, stderr=log)
    log.close()
    return process, log_path


def ready(process):
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError('service exited before becoming ready; inspect its private local log')
        try:
            if api('/v1/info', timeout=2)['scanner_ready']:
                return
        except (OSError, urllib.error.URLError):
            pass
        time.sleep(1)
    raise RuntimeError('service readiness timed out')


def stop(process):
    if process.poll() is None:
        process.terminate()
        process.wait(timeout=30)


def interrupted(process, log, stage):
    if process.wait(timeout=180) != 75:
        raise RuntimeError(f'expected interruption after {stage}; inspect private local log')
    match = re.search(rf'^CWZEC_INTERRUPTED {stage} ([0-9a-f]{{64}})$', log.read_text(), re.M)
    if match is None:
        raise RuntimeError(f'missing {stage} fingerprint')
    return match.group(1)


listeners = subprocess.run(['lsof', '-nP', '-iTCP:8787', '-sTCP:LISTEN', '-t'],
    capture_output=True, text=True)
if listeners.returncode not in (0, 1):
    sys.exit('Cannot identify the listener on port 8787; no process was stopped.')
pids = sorted(set(int(pid) for pid in listeners.stdout.split()))
allowed = {str(root / f'service/target/{profile}/app') for profile in ('debug', 'release')}
for pid in pids:
    files = subprocess.run(['lsof', '-a', '-p', str(pid), '-d', 'txt', '-Fn'],
        capture_output=True, text=True, check=True).stdout.splitlines()
    if not any(line.startswith('n') and line[1:] in allowed for line in files):
        sys.exit('Port 8787 belongs to another application; no process was stopped.')
for pid in pids:
    os.kill(pid, signal.SIGTERM)
if pids:
    print('Stopped the existing cWZEC process for the restart check; PostgreSQL is unchanged.', flush=True)
    time.sleep(2)

process = None
try:
    process, log = start('journal')
    ready(process)
    info = api('/v1/info')
    matching = [d for d in api('/v1/deposits')['deposits'] if d['deposit_id'] == deposit_id]
    if len(matching) != 1:
        raise RuntimeError('the requested deposit was not recovered after restart')
    deposit = matching[0]
    if not deposit['confirmed'] or not deposit['memo_valid'] or deposit['kind'] != 'external' or deposit['spent']:
        raise RuntimeError('deposit must be confirmed, external, unspent and have a valid memo')
    gross = deposit['value_zat'] // 2
    if gross <= info['fixed_fee_zat']:
        raise RuntimeError('deposit is too small for the payout and positive change')
    operation_id = 'milestone-' + deposit_id
    request = {'operation_id': operation_id, 'deposit_id': deposit_id,
        'destination': destination, 'gross_amount_zat': gross}
    unauthorized = urllib.request.Request('http://127.0.0.1:8787/v1/admin/payout',
        data=json.dumps(request).encode(), headers={'content-type': 'application/json'})
    try:
        urllib.request.urlopen(unauthorized, timeout=10)
    except urllib.error.HTTPError as error:
        if error.code != 401:
            raise RuntimeError('unexpected authentication rejection') from None
    else:
        raise RuntimeError('unauthenticated payout request was accepted')
    print('Deposit and memo recovered after service restart. Preparing one payout.', flush=True)
    try:
        api('/v1/admin/payout', request, timeout=180)
    except urllib.error.HTTPError:
        raise RuntimeError('payout request rejected; no retry with a different operation ID') from None
    except (OSError, http.client.HTTPException):
        pass  # Expected when the service exits immediately after the durable journal commit.
    journal_digest = interrupted(process, log, 'journal')
    print('Journal committed; process exited before broadcast.', flush=True)

    process, log = start('broadcast')
    broadcast_digest = interrupted(process, log, 'broadcast')
    if journal_digest != broadcast_digest:
        raise RuntimeError('recovery changed the signed transaction')
    print('Stored transaction submitted; process exited before recording local success.', flush=True)

    process, log = start('recovered')
    ready(process)
    deadline = time.monotonic() + 1800
    while time.monotonic() < deadline:
        status = api('/v1/payouts/' + operation_id)
        if status['signed_bytes_digest'] != journal_digest:
            raise RuntimeError('journal fingerprint changed')
        if status['status'] == 'confirmed':
            if not status['inputs_spent'] or not status['internal_change_observed']:
                raise RuntimeError('confirmation did not include input spends and internal change')
            api('/v1/admin/payout', request)
            conflicting = dict(request, gross_amount_zat=gross + 1)
            try:
                api('/v1/admin/payout', conflicting)
            except urllib.error.HTTPError as error:
                if error.code != 409:
                    raise RuntimeError('unexpected conflicting-operation response') from None
            else:
                raise RuntimeError('conflicting operation terms were accepted')
            replay = api('/v1/payouts/' + operation_id)
            if replay['signed_bytes_digest'] != journal_digest or replay['txid'] != status['txid']:
                raise RuntimeError('idempotent replay changed the payment')
            evidence = {'network': 'testnet', 'pool': 'ironwood', 'deposit_txid': deposit['txid'],
                'deposit_id': deposit_id, 'operation_id': operation_id, 'payout_txid': status['txid'],
                'signed_bytes_digest': journal_digest, 'confirmations': status['confirmations'],
                'restart_received_note': True, 'journal_interruption': True,
                'ambiguous_submission_interruption': True, 'same_signed_bytes': True,
                'unauthenticated_payout_rejected': True, 'operation_replay_unchanged': True,
                'conflicting_operation_rejected': True,
                'inputs_spent': True, 'internal_change_observed': True}
            path = root / '.local/recovery-evidence.json'
            path.write_text(json.dumps(evidence, indent=2) + '\n')
            os.chmod(path, 0o600)
            print('Payout confirmed; spent input and internal change observed; recovery reused identical bytes.')
            print('Public payout txid: ' + status['txid'])
            break
        if status['status'] == 'blocked_expiry_unresolved':
            raise RuntimeError('attempt expiry unresolved; inputs remain reserved, no replacement made')
        time.sleep(10)
    else:
        raise RuntimeError('confirmation wait timed out; journal remains durable, do not create another payment')
except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
    sys.exit(str(error))
finally:
    if process is not None:
        stop(process)
