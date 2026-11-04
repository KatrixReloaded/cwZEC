#!/usr/bin/env python3
"""Local operator credentials go through stdin, never argv or printed output."""
import json
import os
import subprocess
import sys
import urllib.error
from api_request import request


def main():
    args = sys.argv[1:]
    if args in (["status"], ["death"]):
        action = {"action": args[0]}
    elif len(args) == 3 and args[0] == "finish":
        action = {"action": "finish", "redeem_id": hex(int(args[1], 0)), "deposit_id": args[2]}
    elif len(args) == 3 and args[0] == "recredit":
        action = {"action": "recredit", "redeem_id": hex(int(args[1], 0)), "version": int(args[2])}
    elif len(args) == 5 and args[0] == "refund":
        action = {"action": "refund", "deposit_id": args[1], "version": int(args[2]),
                  "participant": args[3], "destination": args[4]}
    else:
        raise ValueError("Use status, recredit ID VERSION, or refund DEPOSIT_ID VERSION PARTICIPANT DESTINATION")
    if action["action"] == "finish":
        # Local evidence export retains its existing exact checkpoint validation.
        payload = {"admin_key": os.environ["ADMIN_API_KEY"], "request": action}
        return subprocess.run(
            [os.environ.get("BRIDGE_RECOVERY_BIN", "./service/target/debug/bridge-recovery")],
            input=json.dumps(payload), text=True, check=False,
        ).returncode
    action_name = action.pop("action")
    if action_name in ("status", "death"):
        result = request('/v1/admin/' + ('recovery' if action_name == 'death' else 'status'))
    else:
        action['version'] = str(action['version'])
        if action_name == 'recredit':
            action['redeem_id'] = str(int(action['redeem_id'], 16))
        result = request('/v1/admin/' + action_name, action)
    print(json.dumps(result, indent=2))
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ValueError, KeyError, OSError, urllib.error.URLError):
        print("Recovery HTTP request failed; retain the same operation and terms.", file=sys.stderr)
        sys.exit(2)
