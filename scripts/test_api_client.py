"""Client contract tests using a loopback HTTP peer; no local credentials are read."""
import contextlib
import io
import json
import os
from pathlib import Path
import runpy
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from unittest.mock import patch
import urllib.error

import api_request
import recovery_request


class ClientTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.received = []
        cls.responses = {}

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_GET(self):
                body = self.rfile.read(int(self.headers.get('content-length', 0)))
                cls.received.append((self.command, self.path, dict(self.headers),
                                     json.loads(body) if body else None))
                code, headers, payload = cls.responses.get(self.path, (200, {}, {'ok': True}))
                self.send_response(code)
                for name, value in headers.items():
                    self.send_header(name, value)
                self.end_headers()
                self.wfile.write(json.dumps(payload).encode())

            do_POST = do_GET

        cls.server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()
        cls.thread.join()

    def setUp(self):
        self.received.clear()
        self.responses.clear()
        self.config = {'BRIDGE_HTTP_PORT': str(self.server.server_port),
                       'ADMIN_API_KEY': 'test-operator-token'}
        self.config_patch = patch.object(api_request, 'local_config', return_value=self.config)
        self.config_patch.start()
        self.addCleanup(self.config_patch.stop)

    def run_main(self, module, args, body=''):
        output = io.StringIO()
        with patch('sys.argv', [module.__file__, *args]), patch('sys.stdin', io.StringIO(body)), \
                contextlib.redirect_stdout(output):
            module.main()
        return json.loads(output.getvalue())

    def test_prepare_uses_public_domain_and_preserves_wallet_transaction(self):
        domain = {'chain_id': '11155111', 'contract': '0x' + '12' * 20}
        wallet = {'operation_id': 'durable-1', 'transaction': {
            'from': '0x' + '34' * 20, 'to': domain['contract'],
            'chainId': '0xaa36a7', 'value': '0x0', 'data': '0xabcdef'},
            'fixed_fee_zat': '10000', 'informational_only': True}
        self.responses['/v1/info'] = (200, {}, domain)
        self.responses['/v1/redeem/prepare'] = (200, {}, wallet)
        terms = {'user': wallet['transaction']['from'], 'amount': '18446744073709551615',
                 'zcash_address': 'public-test-destination'}
        with patch.object(api_request.pathlib.Path, 'read_text', side_effect=AssertionError('key file read')), \
                patch.object(recovery_request.subprocess, 'run', side_effect=AssertionError('signer invoked')):
            result = self.run_main(api_request, ['prepare', 'durable-1'], json.dumps(terms))
        self.assertEqual(result, wallet)
        self.assertEqual([(r[0], r[1]) for r in self.received],
                         [('GET', '/v1/info'), ('POST', '/v1/redeem/prepare')])
        self.assertEqual(self.received[1][3], dict(terms, operation_id='durable-1', **domain))
        self.assertTrue(all('X-Api-Key' not in r[2] for r in self.received))

    def test_prepare_rejects_signing_or_decryption_key_input_before_http(self):
        for key in ('private_key', 'destination_decryption_key'):
            terms = {'user': '0x' + '34' * 20, 'amount': '100000',
                     'zcash_address': 'public-test-destination', key: 'must-not-send'}
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.run_main(api_request, ['prepare', 'durable-1'], json.dumps(terms))
        self.assertEqual(self.received, [])

    def test_auth_is_scoped_to_operator_routes(self):
        public = ['/v1/info', '/v1/memo', '/v1/redeem/prepare', '/v1/redeems/123',
                  '/v1/deposits/abc', '/v1/administer', '/v1/payouts-other/1']
        protected = ['/v1/admin/status', '/v1/payouts/123', '/v1/deposits?recipient=abc']
        for route in public + protected:
            api_request.request(route)
            headers = self.received[-1][2]
            self.assertEqual(headers.get('X-Api-Key'),
                             self.config['ADMIN_API_KEY'] if route in protected else None, route)

    def test_redirect_does_not_send_operator_credential_to_target(self):
        self.responses['/v1/admin/status'] = (302, {
            'Location': f'http://127.0.0.1:{self.server.server_port}/redirect-target'}, {})
        with self.assertRaises(urllib.error.HTTPError) as error:
            api_request.request('/v1/admin/status')
        self.assertEqual(error.exception.code, 302)
        self.assertEqual([r[1] for r in self.received], ['/v1/admin/status'])

    def test_environment_proxy_cannot_intercept_operator_request(self):
        proxy = f'http://127.0.0.1:{self.server.server_port}'
        with patch.dict(os.environ, {'http_proxy': proxy, 'HTTP_PROXY': proxy,
                                     'all_proxy': proxy, 'ALL_PROXY': proxy,
                                     'no_proxy': '', 'NO_PROXY': ''}):
            self.assertEqual(api_request.request('/v1/admin/status'), {'ok': True})
        # A proxy request would use an absolute URI as the HTTP request target.
        self.assertEqual(self.received[0][1], '/v1/admin/status')

    def test_recovery_routes_methods_and_decimal_string_ids(self):
        large_id = 2**255 + 123
        cases = [(['status'], 'GET', '/v1/admin/status', None),
                 (['death'], 'GET', '/v1/admin/recovery', None),
                 (['recredit', hex(large_id), '17'], 'POST', '/v1/admin/recredit',
                  {'redeem_id': str(large_id), 'version': '17'}),
                 (['refund', 'deposit:4', '19', 'participant', 'destination'],
                  'POST', '/v1/admin/refund', {'deposit_id': 'deposit:4', 'version': '19',
                                             'participant': 'participant', 'destination': 'destination'})]
        for args, method, path, body in cases:
            with self.subTest(action=args[0]):
                self.assertEqual(self.run_main(recovery_request, args), {'ok': True})
                actual = self.received[-1]
                self.assertEqual((actual[0], actual[1], actual[3]), (method, path, body))
                self.assertEqual(actual[2]['X-Api-Key'], self.config['ADMIN_API_KEY'])

    def test_cli_http_errors_do_not_print_server_body_or_private_terms(self):
        self.responses['/v1/redeem/prepare'] = (400, {}, {'error': 'untrusted-private-body'})
        self.responses['/v1/admin/status'] = (403, {}, {'error': 'untrusted-private-body'})
        for module, args in [(api_request, ['/v1/redeem/prepare', '-']),
                             (recovery_request, ['status'])]:
            with self.subTest(helper=module.__name__):
                stdout, stderr = io.StringIO(), io.StringIO()
                with patch.dict(os.environ, self.config), \
                        patch.object(Path, 'exists', return_value=False), \
                        patch('sys.argv', [module.__file__, *args]), \
                        patch('sys.stdin', io.StringIO('{"zcash_address":"private-destination"}')), \
                        contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr), \
                        self.assertRaises(SystemExit) as error:
                    runpy.run_path(module.__file__, run_name='__main__')
                output = stdout.getvalue() + stderr.getvalue() + str(error.exception)
                for secret in ('untrusted-private-body', 'private-destination', self.config['ADMIN_API_KEY']):
                    self.assertNotIn(secret, output)
                self.assertNotEqual(error.exception.code, 0)
                self.assertIn('retain the same operation', output)


if __name__ == '__main__':
    unittest.main()
