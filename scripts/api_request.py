#!/usr/bin/env python3
"""Local HTTP client. Keys stay in their owners; operator auth is sent only locally."""
import json
import os
import pathlib
import shlex
import sys
import urllib.error
import urllib.parse
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parents[1]


def local_config():
    config = {}
    path = ROOT / '.local/backed.env'
    if path.exists():
        for line in path.read_text().splitlines():
            words = shlex.split(line)
            if len(words) == 2 and words[0] == 'export':
                key, value = words[1].split('=', 1)
                config[key] = value
    for key in ('ADMIN_API_KEY', 'BRIDGE_HTTP_PORT'):
        if key in os.environ:
            config[key] = os.environ[key]
    return config


def request(path, body=None):
    if not path.startswith('/v1/') or any(c in path for c in ('\r', '\n', '#')):
        raise ValueError('invalid API path')
    config = local_config()
    port = int(config.get('BRIDGE_HTTP_PORT', '8787'))
    if not 0 < port < 65536:
        raise ValueError('invalid local port')
    headers = {'content-type': 'application/json'}
    route = urllib.parse.urlsplit(path).path
    if route.startswith(('/v1/admin/', '/v1/payouts/')) or route == '/v1/deposits':
        headers['x-api-key'] = config['ADMIN_API_KEY']
    data = None if body is None else json.dumps(body).encode()
    if data is not None and len(data) > 8192:
        raise ValueError('request too large')
    req = urllib.request.Request(f'http://127.0.0.1:{port}' + path, data=data, headers=headers)
    # Never redirect an operator credential or use an environment proxy.
    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, *args, **kwargs):
            return None
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    with opener.open(req, timeout=35) as response:
        return json.load(response)


def main():
    args = sys.argv[1:]
    if len(args) == 2 and args[0] == 'prepare':
        body = json.loads(sys.stdin.read(8193))
        if set(body) != {'user', 'amount', 'zcash_address'}:
            raise ValueError('expected user, amount and zcash_address')
        info = request('/v1/info')
        body.update(operation_id=args[1], chain_id=info['chain_id'], contract=info['contract'])
        result = request('/v1/redeem/prepare', body)
    elif len(args) in (1, 2) and args[0].startswith('/v1/'):
        body = None
        if len(args) == 2:
            body = json.loads(sys.stdin.read(8193) if args[1] == '-' else args[1])
        result = request(args[0], body)
    else:
        raise ValueError('usage: api_request.py /v1/path [JSON|-] or prepare OPERATION_ID < request.json')
    print(json.dumps(result, indent=2))


if __name__ == '__main__':
    try:
        main()
    except urllib.error.HTTPError as error:
        # Do not print untrusted error bodies or private request terms.
        sys.exit(f'HTTP {error.code}: request rejected; retain the same operation ID')
    except (ValueError, KeyError, OSError, urllib.error.URLError):
        sys.exit('HTTP request/configuration failed; retain the same operation ID and signed bytes')
