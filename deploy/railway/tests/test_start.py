import base64
import importlib.util
import os
from pathlib import Path
import signal
import unittest
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location('railway_start', Path(__file__).parents[1] / 'start.py')
start = importlib.util.module_from_spec(spec)
spec.loader.exec_module(start)


class ContainerStartTests(unittest.TestCase):
    def test_standby_never_starts_children_or_reads_keys(self):
        handlers = {}
        with patch.dict(os.environ, {}, clear=True), patch.object(start.signal, 'signal', side_effect=lambda s, f: handlers.update({s: f})), patch.object(start.time, 'sleep', side_effect=lambda _: handlers[signal.SIGTERM](None, None)), patch.object(start.subprocess, 'Popen') as spawn:
            self.assertEqual(start.main(), 0)
            spawn.assert_not_called()

    def test_invalid_key_fails_before_starting_sidecar(self):
        with patch.dict(os.environ, {'BRIDGE_ACTIVATE': '1', 'VAULT_KEY_BASE64': 'invalid'}, clear=True), patch.object(start.signal, 'signal'), patch.object(start.subprocess, 'Popen') as spawn:
            with self.assertRaises(ValueError):
                start.main()
            spawn.assert_not_called()

    def test_keys_are_private_and_not_given_to_sidecar_and_shutdown_drains_bridge_first(self):
        names = ('VAULT', 'BRIDGE_SERVICE', 'BRIDGE_AUDITOR', 'BRIDGE_DESTINATION')
        env = {n + '_KEY_BASE64': base64.b64encode(bytes([i + 1]) * 32).decode() for i, n in enumerate(names)}
        env.update(BRIDGE_ACTIVATE='1', ZAMA_RPC_URL='https://rpc.invalid', ADMIN_API_KEY='test-admin', PORT='9000')
        handlers, events, paths = {}, [], []
        sidecar, bridge = Mock(), Mock()
        sidecar.poll.return_value = bridge.poll.return_value = None
        bridge.terminate.side_effect = lambda: events.append('bridge-stop')
        bridge.wait.side_effect = lambda: events.append('bridge-drained')
        sidecar.terminate.side_effect = lambda: events.append('sidecar-stop')

        def spawn(command, env):
            if 'node' in command[0]:
                self.assertEqual(env, {'SEPOLIA_RPC_URL': 'https://rpc.invalid', 'SIDECAR_PORT': '8788'})
                return sidecar
            self.assertFalse(any(k.endswith('_KEY_BASE64') for k in env))
            self.assertEqual(env['BRIDGE_HTTP_PORT'], '9000')
            for i, name in enumerate(names):
                path = Path(env[name + '_KEY_PATH'])
                paths.append(path)
                self.assertEqual(path.stat().st_mode & 0o777, 0o600)
                self.assertEqual(path.read_bytes(), bytes([i + 1]) * 32)
            return bridge

        connection = Mock()
        connection.__enter__ = Mock()
        connection.__exit__ = Mock(return_value=False)
        with patch.dict(os.environ, env, clear=True), patch.object(start.signal, 'signal', side_effect=lambda s, f: handlers.update({s: f})), patch.object(start.time, 'sleep', side_effect=lambda _: handlers[signal.SIGTERM](None, None)), patch.object(start.socket, 'create_connection', return_value=connection), patch.object(start.subprocess, 'Popen', side_effect=spawn):
            self.assertEqual(start.main(), 0)
        self.assertEqual(events, ['bridge-stop', 'bridge-drained', 'sidecar-stop'])
        self.assertTrue(all(not p.exists() for p in paths))


if __name__ == '__main__':
    unittest.main()
