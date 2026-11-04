#!/usr/bin/env python3
"""Source-only, nonfunded same-machine acceptance. Never opens existing .local state."""
import getpass
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tempfile
import time
from urllib.parse import quote
import urllib.error
import urllib.request


def main():
    os.umask(0o077)
    repository = Path(__file__).resolve().parent.parent
    root = Path(tempfile.mkdtemp(prefix="cwzec-acceptance-"))
    source = root / "source"
    source.mkdir()
    for name in ("logs", "runtime", "cargo-home", "npm-cache"):
        (root / name).mkdir()
    for name in ("npm-user.conf", "npm-global.conf"):
        (root / name).touch()
    print(f"Isolated same-machine acceptance: {root}", flush=True)
    excluded = {".local", ".git", ".agents", ".codex", ".aws", "node_modules",
                "target", "artifacts", "cache", "fhevmTemp", "__pycache__", ".DS_Store"}
    files = []
    for name in ("service", "sidecar", "contracts", "probes", "scripts", "docs"):
        for directory, dirs, names in os.walk(repository / name):
            dirs[:] = [d for d in dirs if d not in excluded and not (Path(directory) / d).is_symlink()]
            files.extend(Path(directory) / n for n in names)
    files.extend(repository.glob("*.md"))
    files.extend(repository / n for n in (".gitignore", ".nvmrc"))
    manifest = {}
    for path in sorted(files):
        relative = path.relative_to(repository)
        if (any(p in excluded for p in relative.parts) or path.is_symlink()
                or not path.is_file() or path.name.startswith(".env")
                or path.suffix in {".key", ".seed", ".pyc", ".log"}):
            continue
        target = source / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, target)
        manifest[str(relative)] = hashlib.sha256(path.read_bytes()).hexdigest()
    (root / "source-manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    node = shutil.which(os.environ.get("NODE_BIN", "node"))
    if node is None:
        raise RuntimeError("Install Node 24.22.1 or set NODE_BIN")
    node = str(Path(node).absolute())
    # Installed toolchains are reused; package/build caches and runtime state are not.
    env = {k: os.environ[k] for k in ("HOME", "PATH", "RUSTUP_HOME", "TMPDIR") if k in os.environ}
    env.update(PATH=str(Path(node).parent) + os.pathsep + env.get("PATH", ""),
               NODE_BIN=node, CARGO_HOME=str(root / "cargo-home"),
               CARGO_TARGET_DIR=str(root / "target"),
               TMPDIR=str(root / "runtime"),
               npm_config_cache=str(root / "npm-cache"),
               npm_config_userconfig=str(root / "npm-user.conf"),
               npm_config_globalconfig=str(root / "npm-global.conf"))
    results = {"environment": {"os": platform.platform(), "machine": platform.machine(),
                               "python": platform.python_version(), "node_binary": node},
               "steps": [], "final_backend_acceptance": "not established by this script"}

    def run(name, command, cwd=source):
        print(name, flush=True)
        with (root / "logs" / f"{name}.log").open("w") as log:
            result = subprocess.run(command, cwd=cwd, env=env, stdout=log, stderr=subprocess.STDOUT)
        results["steps"].append({"name": name, "command": command, "exit": result.returncode})
        (root / "results.json").write_text(json.dumps(results, indent=2) + "\n")
        if result.returncode:
            raise RuntimeError(f"{name} failed; inspect {root / 'logs' / (name + '.log')}")

    run("node-version", [node, "--version"])
    if (root / "logs/node-version.log").read_text().strip() != "v24.22.1":
        raise RuntimeError("Use pinned Node 24.22.1; no dependency installation attempted")
    compiler = subprocess.check_output(["rustup", "which", "--toolchain", "1.92.0", "rustc"], env=env, text=True).strip()
    env["RUSTC"] = compiler
    env["PATH"] = str(Path(compiler).parent) + os.pathsep + env["PATH"]
    results["environment"]["rustc_binary"] = compiler
    cargo = ["rustup", "run", "1.92.0", "cargo"]
    run("rust-version", ["rustup", "run", "1.92.0", "rustc", "--version"])
    run("npm-version", ["npm", "--version"])
    run("postgres-version", ["psql", "--version"])
    run("cargo-fetch", cargo + ["fetch", "--locked"], source / "service")
    for project in ("sidecar", "contracts"):
        run(project + "-install", ["npm", "ci", "--ignore-scripts", "--no-audit", "--no-fund"], source / project)
    run("format", cargo + ["fmt", "--all", "--", "--check"], source / "service")
    run("check", cargo + ["check", "--workspace", "--all-targets", "--locked", "--offline"], source / "service")
    run("build", cargo + ["build", "-p", "app", "--bins", "--locked", "--offline"], source / "service")
    run("release", cargo + ["build", "-p", "app", "--bin", "bridge-auto", "--release", "--locked", "--offline"], source / "service")
    # Create a new database only. Never drop/recreate a caller-supplied database.
    database = "cwzec_accept_" + os.urandom(6).hex()
    env.update(PGHOST=os.environ.get("PGHOST", "127.0.0.1"),
               PGPORT=os.environ.get("PGPORT", "5432"),
               PGUSER=os.environ.get("PGUSER", getpass.getuser()))
    run("create-database", ["createdb", database])
    results["disposable_database"] = database
    env["DATABASE_URL"] = f"postgresql://{quote(env['PGUSER'], safe='')}@{env['PGHOST']}:{env['PGPORT']}/{database}"
    # Each Rust integration test also owns a disposable schema inside this database.
    run("recovery-checks", ["sh", "scripts/recovery-checks.sh"])
    run("sidecar-check", ["npm", "run", "check"], source / "sidecar")
    run("sidecar-tests", ["npm", "test"], source / "sidecar")
    run("contract-tests", ["npm", "test"], source / "contracts")
    # Start only the keyless sidecar. Warmup is read-only public SDK/RPC traffic.
    # Port 0 lets the OS select a distinct listener without racing another service.
    env["SIDECAR_PORT"] = "0"
    for attempt in range(2):
        log_path = root / "logs" / f"sidecar-start-{attempt}.log"
        with log_path.open("w") as log:
            process = subprocess.Popen(["bash", "scripts/sidecar-start.sh"], cwd=source, env=env,
                                       stdout=log, stderr=subprocess.STDOUT)
            try:
                deadline = time.monotonic() + 180
                while True:
                    output = log_path.read_text()
                    if "Sidecar ready on 127.0.0.1:" in output:
                        port = int(output.strip().split(":")[-1])
                        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
                        try:
                            opener.open(f"http://127.0.0.1:{port}/", timeout=5)
                        except urllib.error.HTTPError as error:
                            assert error.code == 405
                        else:
                            raise RuntimeError("Unexpected sidecar GET response")
                        break
                    if process.poll() is not None or time.monotonic() > deadline:
                        raise RuntimeError(f"Sidecar startup failed; inspect {log_path}")
                    time.sleep(0.25)
            finally:
                if process.poll() is None:
                    process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
                    raise RuntimeError("Sidecar graceful shutdown timed out")
            if process.returncode != 0:
                raise RuntimeError("Sidecar did not shut down successfully")
        results["steps"].append({"name": f"sidecar-start-stop-{attempt}", "exit": 0})
    results["controlled_installation_checks"] = "passed"
    (root / "results.json").write_text(json.dumps(results, indent=2) + "\n")
    print(f"Controlled checks passed; evidence: {root / 'results.json'}", flush=True)
    print(f"Disposable database retained. Optional cleanup: dropdb {database}")
    print("No funded coordinator, deployment or transfer started; separate-machine reproduction remains unverified.")


if __name__ == "__main__":
    main()
