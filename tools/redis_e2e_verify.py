"""End-to-end verification runner for the Arcnix Redis app.

1. Starts a local loopback SSH/SFTP test server (`tools/sftp_fixture.py`) with
   live stateful RESP2 Redis instances (host + password-protected Docker).
2. Runs all 12 core SSH integration test cases (`arcnix-core --test redis_integration`).
3. Runs the live full-stack Arcnix UI + SSH worker E2E test (`redis_ui::tests::live_ssh_ui_e2e_flow`).
4. Builds and launches the real compiled `arcnix` desktop binary connected to the
   local test server, verifies startup stability and real SSH traffic, and checks
   that no Redis password ever appeared in SSH command lines.
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def eventually(probe, timeout=15):
    end = time.monotonic() + timeout
    last = None
    while time.monotonic() < end:
        try:
            val = probe()
            if val:
                return val
        except Exception as exc:
            last = exc
        time.sleep(0.1)
    raise AssertionError(f"Timed out waiting for fixture condition: {last}")


def write_test_config(run_dir: Path, info: dict) -> Path:
    host, port_str = info["address"].rsplit(":", 1)
    local_dir = run_dir / "local"
    local_dir.mkdir(exist_ok=True)
    app_config = {
        "version": 1,
        "config": {
            "servers": [
                {
                    "id": "redis-test-server",
                    "name": "Redis Test Server",
                    "host": host,
                    "port": int(port_str),
                    "username": "arcnix",
                    "auth": "PrivateKey",
                    "private_key": info["client_key_ed25519"],
                    "initial_remote_path": "/",
                }
            ],
            "groups": [],
            "trusted_hosts": [
                {
                    "host": host,
                    "port": int(port_str),
                    "fingerprint": info["host_key_rsa_fingerprint"],
                }
            ],
            "local_directory": str(local_dir),
            "settings": {
                "language": "English",
                "theme": "Light",
                "show_hidden_files": True,
                "resource_interval_seconds": 3,
            },
        },
    }
    config_path = run_dir / "config.json"
    config_path.write_text(json.dumps(app_config), encoding="utf-8")
    return config_path


def find_binary() -> Path:
    meta = json.loads(
        subprocess.check_output(["cargo", "metadata", "--no-deps", "--format-version", "1"], cwd=ROOT)
    )
    return Path(meta["target_directory"]) / "debug" / ("arcnix.exe" if os.name == "nt" else "arcnix")


def verify_key_creation(helper, endpoint):
    from concurrent.futures import ThreadPoolExecutor

    def create(key, fields):
        try:
            helper.op_write({"endpoint": endpoint, "db": 2, "create_only": True, "write_op": {"key": key, **fields}})
        except helper.RespError as exc:
            if str(exc) not in ("A key with this name already exists", "The key changed during creation. Try again"):
                raise
            return False
        return True

    for index, fields in enumerate((
        {"kind": "set_string", "value": "first", "ttl_seconds": 90},
        {"kind": "set_hash_field", "field": "field", "value": "first"},
        {"kind": "push_list", "value": "first"},
        {"kind": "add_set_member", "member": "first"},
        {"kind": "add_zset_member", "member": "first", "score": 1},
    )):
        key = f"create:{index}"
        assert create(key, fields)
        changed = {**fields, ("value" if "value" in fields else "member"): "second"}
        assert not create(key, changed)
        assert not create(key, {"kind": "set_string", "value": "replacement"})
        value = helper.op_get_key({"endpoint": endpoint, "db": 2, "key": key})
        if fields["kind"] == "set_string":
            assert value["string_value"] == "first" and 0 < value["ttl"] <= 90
        else:
            assert [item["value"] for item in value["entries"]] == ["first"]

    def race(i):
        return create("create:race", {"kind": "push_list", "value": str(i)})

    with ThreadPoolExecutor(max_workers=4) as pool:
        winners = [i for i, won in enumerate(pool.map(race, range(8))) if won]
    assert len(winners) == 1
    value = helper.op_get_key({"endpoint": endpoint, "db": 2, "key": "create:race"})
    assert [item["value"] for item in value["entries"]] == [str(winners[0])]

    def edit(key):
        helper.op_write({"endpoint": endpoint, "db": 2, "write_op": {
            "kind": "set_string", "key": key, "value": "edited", "keep_ttl": True,
        }})

    edit("create:0")
    value = helper.op_get_key({"endpoint": endpoint, "db": 2, "key": "create:0"})
    assert value["string_value"] == "edited" and 0 < value["ttl"] <= 90
    for key in ("create:missing", "create:race"):
        try:
            edit(key)
            raise AssertionError("Stale string edit succeeded")
        except helper.RespError as exc:
            assert str(exc) == "This key no longer holds a string"


def run_helper_unit_tests():
    import importlib.util
    from unittest.mock import patch
    from redis_fixture import RespTestServer

    helper_path = ROOT / "crates/arcnix-core/src/software/redis_helper.py"
    spec = importlib.util.spec_from_file_location("arcnix_redis_helper_test", helper_path)
    mod = importlib.util.module_from_spec(spec)
    assert spec and spec.loader
    spec.loader.exec_module(mod)

    # 1. Container detection excludes exporters/UIs and includes official/valkey images.
    assert mod.looks_like_redis("redis:7-alpine", "docker-entrypoint.sh redis-server")
    assert mod.looks_like_redis("valkey/valkey:8", "valkey-server")
    assert mod.looks_like_redis("redis/redis-stack:latest", "/entrypoint.sh")
    assert mod.looks_like_redis("registry.example:5000/cache/redis:7", "/entrypoint.sh")
    assert not mod.looks_like_redis("oliver006/redis_exporter:latest", "/redis_exporter")
    assert not mod.looks_like_redis("rediscommander/redis-commander:latest", "node app.js")
    assert not mod.looks_like_redis("redis/redisinsight:latest", "node dist/main.js")

    # 2. Config parser handles bind IPv6, quoted unixsocket, comments, and invalid ports.
    ep_v6 = mod._parse_config_text("bind ::1\nport 6380 # custom\n")
    assert ep_v6["host"] == "::1" and ep_v6["port"] == 6380
    ep_sock = mod._parse_config_text('port 0\nunixsocket "/var/run/redis/redis.sock"\n')
    assert ep_sock["socket"] == "/var/run/redis/redis.sock"
    assert mod._parse_config_text("port 0\ntls-port 6380\n") is None
    ep_overflow = mod._parse_config_text("port 999999\n")
    assert ep_overflow["port"] == 6379

    # 3. Container endpoint prefers 6379/tcp over 8001/tcp and preserves IPv6 ::1.
    host, port, _ = mod._pick_container_endpoint(
        {
            "8001/tcp": [{"HostIp": "127.0.0.1", "HostPort": "8001"}],
            "6379/tcp": [{"HostIp": "::1", "HostPort": "16379"}],
        },
        {},
    )
    assert host == "::1" and port == 16379
    other_ports = {"8001/tcp": [{"HostIp": "127.0.0.1", "HostPort": "8001"}]}
    assert mod._pick_container_endpoint(other_ports, {})[:2] == (None, None)
    assert mod._pick_container_endpoint(other_ports, {"Networks": {"bridge": {"IPAddress": "172.31.0.2"}}})[:2] == ("172.31.0.2", 6379)
    isolated = {"id": "a" * 64, "name": "isolated", "state": "running", "host": None, "port": None}
    with (
        patch.object(mod, "read_host_service", return_value=None),
        patch.object(mod, "host_installed", return_value=False),
        patch.object(mod, "read_host_config_endpoint", return_value=(None, None)),
        patch.object(mod, "read_docker_containers", return_value=([isolated], "")),
        patch.object(mod, "probe_instance_info") as probe,
    ):
        instance, = mod.op_discover({"auth": {"docker:" + isolated["id"]: {"password": "test"}}})["instances"]
        assert instance["endpoint"] is None and instance["auth_status"]["kind"] == "unreachable"
        probe.assert_not_called()

    # 4. Truncating multi-byte UTF-8 text does not misclassify as binary.
    zh = ("你好世界" * 20).encode("utf-8")
    text, is_bin, is_trunc = mod.decode_bytes(zh, limit=7)
    assert not is_bin and is_trunc and text == "你好"

    # 5. Binary data is safely detected and hex-encoded.
    _, is_bin2, _ = mod.decode_bytes(b"\x89PNG\x00\xff")
    assert is_bin2

    # 6. Multi-cursor HSCAN / SSCAN iteration collects all items across cursors.
    class FakeClient:
        def __init__(self):
            self.hcalls = 0
            self.scalls = 0

        def command(self, *args):
            if args[0] == "HSCAN":
                self.hcalls += 1
                if args[2] == "0":
                    return [b"1", []]  # Real Redis can return empty batch with non-zero cursor!
                return [b"0", [b"f1", b"v1", b"f2", b"v2"]]
            if args[0] == "SSCAN":
                self.scalls += 1
                if args[2] == "0":
                    return [b"1", [b"a"]]
                return [b"0", [b"b", b"a"]]  # duplicate across cursors
            raise AssertionError(args)

    fake = FakeClient()
    h_entries, _, h_trunc = mod._scan_hash(fake, "k")
    assert len(h_entries) == 2 and not h_trunc and fake.hcalls == 2
    s_entries, _, s_trunc = mod._scan_set(fake, "k")
    assert len(s_entries) == 2 and not s_trunc and fake.scalls == 2

    # 7. read_host_service puts --no-pager and --property BEFORE -- so systemctl accepts them.
    recorded_cmds = []
    orig_run_cmd = mod.run_cmd
    orig_which = mod.shutil.which
    try:
        mod.shutil.which = lambda name: "/usr/bin/systemctl" if name == "systemctl" else orig_which(name)

        def fake_run_cmd(args, timeout=5):
            recorded_cmds.append(list(args))
            return 0, "LoadState=loaded\nActiveState=active\nSubState=running\nUnitFileState=enabled\nActiveEnterTimestampMonotonic=1000\n", ""

        mod.run_cmd = fake_run_cmd
        svc = mod.read_host_service()
        assert svc and svc["unit"] == "redis-server.service"
        cmd = recorded_cmds[0]
        sep = cmd.index("--")
        assert cmd.index("--no-pager") < sep
        assert any(a.startswith("--property=") for a in cmd[:sep])
        assert cmd[sep + 1] == "redis-server.service"
    finally:
        mod.run_cmd = orig_run_cmd
        mod.shutil.which = orig_which

    # 8. INFO / CLIENT LIST with non-UTF-8 bytes decode lossily (never hex-dumped).
    lossy_info = mod._decode_lossy_text(b"redis_version:7.2.5\r\nexecutable:/tmp/\xff/redis\r\n")
    assert "redis_version:7.2.5" in lossy_info and "\ufffd" in lossy_info

    # 9. op_scan marks >4096-byte UTF-8 key names as binary (non-selectable).
    orig_connect = mod.connect_and_auth
    try:
        long_key = ("k" * 5000).encode("utf-8")

        class LongKeyClient:
            def command(self, *args):
                if args[0] == "SCAN":
                    return [b"0", [long_key]]
                if args[0] == "TYPE":
                    return b"string"
                if args[0] == "TTL":
                    return -1
                if args[0] == "MEMORY":
                    return 5100
                raise AssertionError(args)

            def close(self):
                pass

        mod.connect_and_auth = lambda *a, **kw: LongKeyClient()
        page = mod.op_scan({"endpoint": {"host": "127.0.0.1", "port": 6379}, "db": 0, "cursor": "0", "pattern": "*"})
        assert len(page["keys"]) == 1 and page["keys"][0]["binary"] is True
    finally:
        mod.connect_and_auth = orig_connect

    # 10. RespClient rejects oversized bulk string and excessive nesting depth.
    c = mod.RespClient.__new__(mod.RespClient)
    c.buf = bytearray(f"${mod.MAX_RESP_BULK_BYTES + 1}\r\n".encode("ascii"))
    c.total_read = 0
    try:
        c._read_reply(0)
        raise AssertionError("Expected oversized bulk string error")
    except ValueError:
        pass
    try:
        c._read_reply(mod.MAX_RESP_DEPTH + 1)
        raise AssertionError("Expected depth limit error")
    except ValueError:
        pass

    for version in ("7.2.5", "5.0.14"):
        server = RespTestServer(version=version)
        try:
            verify_key_creation(mod, {"host": "127.0.0.1", "port": server.port})
        finally:
            server.close()

    print("[PASS] redis_helper.py direct unit tests passed", flush=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--helper-only", action="store_true", help="Run the Redis helper regression tests without SSH or a desktop.")
    parser.add_argument(
        "--preview",
        action="store_true",
        help="Start the local SSH + Redis test server and keep Arcnix open for interactive manual E2E testing.",
    )
    args = parser.parse_args()
    if args.helper_only:
        run_helper_unit_tests()
        return 0

    run_dir = Path(tempfile.mkdtemp(prefix="arcnix-redis-e2e-"))
    ready_file = run_dir / "fixture-ready.json"
    peer_log = (run_dir / "peer.log").open("w", encoding="utf-8")
    peer = subprocess.Popen(
        [sys.executable, str(ROOT / "tools/sftp_fixture.py"), "--ready", str(ready_file)],
        cwd=ROOT,
        stdout=peer_log,
        stderr=peer_log,
    )
    try:
        info = eventually(lambda: json.loads(ready_file.read_text(encoding="utf-8")))
        fixture_root = Path(info["root"])
        (fixture_root / "redis-enabled").write_text("1\n", encoding="utf-8")
        addr = info["address"]
        print(f"Local SSH + Redis test server listening at {addr} (root: {fixture_root})", flush=True)

        if args.preview:
            binary = find_binary()
            if not binary.is_file():
                subprocess.run(["cargo", "build", "--locked", "-p", "arcnix", "--bin", "arcnix"], cwd=ROOT, check=True)
            config_path = write_test_config(run_dir, info)
            print("Interactive Redis E2E preview ready:", flush=True)
            print("  - Server: Redis Test Server (one-click connect, Ed25519 key pre-configured)", flush=True)
            print("  - Host Redis (This server): Redis 7.2.5, no password, db0 (123 keys) & db1 (2 keys)", flush=True)
            print("  - Docker Redis (redis-auth): Redis 7.4.1, password = s3cr3t", flush=True)
            print("Close the Arcnix window when finished to shut down the local test server.", flush=True)
            proc = subprocess.Popen([str(binary), "--config", str(config_path)], cwd=ROOT)
            return proc.wait()

        env = os.environ.copy()
        env["ARCNIX_TEST_SSH_ADDR"] = addr
        env["ARCNIX_TEST_SSH_KEY"] = info["client_key_ed25519"]

        print("\n=== Suite 0: redis_helper.py direct unit tests ===", flush=True)
        run_helper_unit_tests()

        print("\n=== Suite 1: arcnix-core SSH + RESP2 integration tests (TC-01..TC-12) ===", flush=True)
        subprocess.run(
            [
                "cargo",
                "test",
                "--locked",
                "-p",
                "arcnix-core",
                "--test",
                "redis_integration",
                "--",
                "--ignored",
                "--nocapture",
            ],
            cwd=ROOT,
            env=env,
            check=True,
        )

        print("\n=== Suite 2: arcnix live UI + SSH worker E2E flow ===", flush=True)
        subprocess.run(
            [
                "cargo",
                "test",
                "--locked",
                "-p",
                "arcnix",
                "software::redis_ui::tests::live_ssh_ui_e2e_flow",
                "--",
                "--ignored",
                "--nocapture",
            ],
            cwd=ROOT,
            env=env,
            check=True,
        )

        print("\n=== Suite 3: Build & launch real Arcnix desktop binary against local test server ===", flush=True)
        subprocess.run(["cargo", "build", "--locked", "-p", "arcnix", "--bin", "arcnix"], cwd=ROOT, check=True)
        binary = find_binary()
        assert binary.is_file(), f"Missing built binary: {binary}"

        config_path = write_test_config(run_dir, info)
        app_log_path = run_dir / "arcnix-app.log"
        with app_log_path.open("w", encoding="utf-8") as app_log:
            proc = subprocess.Popen(
                [str(binary), "--config", str(config_path)],
                cwd=ROOT,
                stdout=app_log,
                stderr=app_log,
            )
            try:
                time.sleep(3.5)
                rc = proc.poll()
                assert rc is None, f"Arcnix exited unexpectedly with code {rc}: {app_log_path.read_text()}"
                print(f"[PASS] Real Arcnix desktop binary launched and remained healthy (PID {proc.pid})", flush=True)
            finally:
                proc.terminate()
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait(timeout=5)

        # Security audit: confirm no Redis password ever appeared in SSH command lines.
        queries_path = fixture_root / "server-app-queries.jsonl"
        assert queries_path.is_file(), "Expected server-app-queries.jsonl audit log"
        queries_text = queries_path.read_text(encoding="utf-8")
        assert "# arcnix:apps:discover" in queries_text
        assert "# arcnix:redis:helper" in queries_text
        assert "s3cr3t" not in queries_text, "SECURITY FAILURE: Redis password leaked into SSH command line!"
        assert "wrong-pass" not in queries_text, "SECURITY FAILURE: Redis password leaked into SSH command line!"
        print("[PASS] Security audit: zero Redis credentials in SSH command audit log", flush=True)
        print("\nALL REDIS E2E VERIFICATION SUITES PASSED.", flush=True)
        return 0
    finally:
        peer.terminate()
        try:
            peer.wait(timeout=10)
        except subprocess.TimeoutExpired:
            peer.kill()
            peer.wait(timeout=5)
        peer_log.close()


if __name__ == "__main__":
    raise SystemExit(main())
