"""Keep the FHE sidecar on loopback and out of the coordinator's key environment."""
import base64
import os
from pathlib import Path
import signal
import socket
import subprocess
import tempfile
import time


def main():
    stopping = False

    def stop(_signal, _frame):
        nonlocal stopping
        stopping = True

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    # Provision/restore PostgreSQL first. Standby never loads keys or starts scans.
    if os.environ.get("BRIDGE_ACTIVATE") != "1":
        print("Bridge standby; restore state before setting BRIDGE_ACTIVATE=1", flush=True)
        while not stopping:
            time.sleep(0.2)
        return 0

    os.umask(0o077)
    with tempfile.TemporaryDirectory(prefix="bridge-keys-") as directory:
        env = os.environ.copy()
        for name in ("VAULT", "BRIDGE_SERVICE", "BRIDGE_AUDITOR", "BRIDGE_DESTINATION"):
            secret = name + "_KEY_BASE64"
            raw = base64.b64decode(env.pop(secret), validate=True)
            os.environ.pop(secret, None)
            if len(raw) != 32:
                raise ValueError("invalid key length")
            path = Path(directory) / name
            path.write_bytes(raw)
            env[name + "_KEY_PATH"] = str(path)
        env["SIDECAR_URL"] = "http://127.0.0.1:8788"
        env["BRIDGE_HTTP_PORT"] = env.get("PORT", "8787")
        sidecar = subprocess.Popen(
            ["/usr/local/bin/node", "/app/sidecar/src/main.ts"],
            env={"SEPOLIA_RPC_URL": env["ZAMA_RPC_URL"], "SIDECAR_PORT": "8788"},
        )
        bridge = None
        try:
            deadline = time.monotonic() + 180
            while not stopping:
                if sidecar.poll() is not None or time.monotonic() >= deadline:
                    raise RuntimeError("sidecar not ready")
                try:
                    with socket.create_connection(("127.0.0.1", 8788), timeout=0.5):
                        break
                except OSError:
                    time.sleep(0.2)
            if stopping:
                return 0
            bridge = subprocess.Popen(["/usr/local/bin/bridge-auto"], env=env)
            while not stopping and bridge.poll() is None and sidecar.poll() is None:
                time.sleep(0.2)
            return 0 if stopping else (bridge.poll() or 1)
        finally:
            # Drain the coordinator while the sidecar is still available.
            if bridge is not None and bridge.poll() is None:
                bridge.terminate()
                bridge.wait()
            if sidecar.poll() is None:
                sidecar.terminate()
            sidecar.wait()


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception:
        # Never print configuration, key bytes or exception payloads.
        raise SystemExit("Bridge container startup failed; check configuration and service logs")
