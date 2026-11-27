"""Stateful multi-DB RESP2 test server for Arcnix Redis end-to-end verification.

Runs loopback-only TCP servers that speak real RESP2 so Arcnix's real
`redis_helper.py` exercises actual socket I/O, framing, AUTH, SCAN pagination,
all six Redis data types, binary/large previews, CRUD/TTL mutations, CLIENT
LIST, and SLOWLOG GET without needing a system `redis-server` binary.
"""
from __future__ import annotations

import fnmatch
import json
import socket
import threading
import time
from pathlib import Path


def _encode_resp(val):
    if val is None:
        return b"$-1\r\n"
    if isinstance(val, bool):
        return b":1\r\n" if val else b":0\r\n"
    if isinstance(val, int):
        return f":{val}\r\n".encode("ascii")
    if isinstance(val, str):
        return f"+{val}\r\n".encode("utf-8")
    if isinstance(val, bytes):
        return f"${len(val)}\r\n".encode("ascii") + val + b"\r\n"
    if isinstance(val, list):
        return f"*{len(val)}\r\n".encode("ascii") + b"".join(_encode_resp(x) for x in val)
    raise TypeError(f"Unsupported RESP value: {type(val)}")


def _read_resp_commands(conn):
    f = conn.makefile("rb")
    while True:
        line = f.readline()
        if not line:
            return
        if not line.startswith(b"*"):
            return
        count = int(line[1:].strip())
        args = []
        for _ in range(count):
            header = f.readline()
            if not header.startswith(b"$"):
                return
            length = int(header[1:].strip())
            data = f.read(length)
            f.read(2)
            args.append(data)
        yield args


class RespTestServer:
    """In-memory stateful multi-DB RESP2 server bound to 127.0.0.1:0."""

    def __init__(self, *, password=None, username="", version="7.2.5", seed_large=True):
        self.password = password
        self.username = username
        self.version = version
        self.started_at = time.time()
        self.lock = threading.RLock()
        self.key_versions = {}
        self.dbs: dict[int, dict[str, dict]] = {i: {} for i in range(16)}
        self.hits = 940
        self.misses = 60
        self.ops = 312
        self.slowlog = [
            [
                2,
                int(time.time()) - 30,
                18400,
                [b"HGETALL", b"user:1001"],
                b"127.0.0.1:51200",
                b"api-worker",
            ],
            [
                1,
                int(time.time()) - 120,
                5200,
                [b"LRANGE", b"jobs:pending", b"0", b"-1"],
                b"127.0.0.1:51201",
                b"scheduler",
            ],
        ]
        self._seed(seed_large=seed_large)
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.sock.bind(("127.0.0.1", 0))
        self.sock.listen(32)
        self.port = self.sock.getsockname()[1]
        self.closed = False
        self.thread = threading.Thread(target=self._accept_loop, daemon=True)
        self.thread.start()

    def _seed(self, *, seed_large: bool):
        db0 = self.dbs[0]
        db0["app:config"] = {
            "kind": "string",
            "value": b'{"env":"staging","feature_flags":{"redis_app":true,"retries":3}}',
            "expire_at": time.time() + 3600,
        }
        db0["user:1001"] = {
            "kind": "hash",
            "value": {
                "name": b"Alice",
                "role": b"admin",
                "email": b"alice@example.com",
            },
            "expire_at": None,
        }
        db0["jobs:pending"] = {
            "kind": "list",
            "value": [b"send-welcome-email", b"generate-report", b"sync-metrics"],
            "expire_at": None,
        }
        db0["tags:featured"] = {
            "kind": "set",
            "value": {b"rust", b"redis", b"ssh"},
            "expire_at": None,
        }
        db0["leaderboard:daily"] = {
            "kind": "zset",
            "value": {b"alice": 98.5, b"bob": 84.0, b"carol": 71.25},
            "expire_at": None,
        }
        db0["events:audit"] = {
            "kind": "stream",
            "value": [
                ("1730000001000-0", {"actor": b"alice", "action": b"deploy"}),
                ("1730000002000-0", {"actor": b"bob", "action": b"restart"}),
            ],
            "expire_at": None,
        }
        db0["blob:binary"] = {
            "kind": "string",
            "value": b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\xff\xfe",
            "expire_at": None,
        }
        db0["blob:large"] = {
            "kind": "string",
            "value": b"A" * (140 * 1024),
            "expire_at": None,
        }
        if seed_large:
            for i in range(1, 116):
                db0[f"session:{i:03d}"] = {
                    "kind": "string",
                    "value": f"token-{i:03d}".encode("utf-8"),
                    "expire_at": time.time() + 1800,
                }
        self.dbs[1]["cache:homepage"] = {
            "kind": "string",
            "value": b"<h1>Cached Home</h1>",
            "expire_at": time.time() + 600,
        }
        self.dbs[1]["user:2002"] = {
            "kind": "hash",
            "value": {"name": b"Bob"},
            "expire_at": None,
        }

    def close(self):
        self.closed = True
        try:
            self.sock.close()
        except OSError:
            pass

    def _accept_loop(self):
        while not self.closed:
            try:
                conn, addr = self.sock.accept()
            except OSError:
                break
            threading.Thread(target=self._handle_conn, args=(conn, addr), daemon=True).start()

    def _ttl_of(self, entry):
        exp = entry.get("expire_at")
        if exp is None:
            return -1
        rem = int(exp - time.time())
        return max(1, rem)

    def _info_text(self):
        lines = [
            "# Server",
            f"redis_version:{self.version}",
            "redis_mode:standalone",
            f"uptime_in_seconds:{max(1, int(time.time() - self.started_at) + 7200)}",
            "# Clients",
            "connected_clients:3",
            "blocked_clients:0",
            "# Memory",
            "used_memory:4194304",
            "used_memory_rss:6291456",
            "maxmemory:268435456",
            "maxmemory_policy:allkeys-lru",
            "mem_fragmentation_ratio:1.50",
            "# Persistence",
            f"rdb_last_save_time:{int(time.time()) - 120}",
            "rdb_last_bgsave_status:ok",
            "aof_enabled:1",
            "# Stats",
            f"instantaneous_ops_per_sec:{self.ops}",
            f"keyspace_hits:{self.hits}",
            f"keyspace_misses:{self.misses}",
            "# Replication",
            "role:master",
            "# Keyspace",
        ]
        for idx in range(16):
            db = self.dbs[idx]
            if not db:
                continue
            expires = sum(1 for e in db.values() if e.get("expire_at") is not None)
            lines.append(f"db{idx}:keys={len(db)},expires={expires},avg_ttl=1800000")
        return "\r\n".join(lines) + "\r\n"

    def _handle_conn(self, conn, addr):
        with conn:
            authed = self.password is None
            db_idx = 0
            watched = {}
            queued = None
            for raw_args in _read_resp_commands(conn):
                if not raw_args:
                    continue
                cmd = raw_args[0].decode("utf-8", errors="replace").upper()
                try:
                    args = raw_args[1:]
                    with self.lock:
                        if authed and cmd == "WATCH":
                            for key in args:
                                watched[(db_idx, key)] = self.key_versions.get((db_idx, key), 0)
                            reply = _encode_resp("OK")
                        elif authed and cmd == "MULTI":
                            queued = []
                            reply = _encode_resp("OK")
                        elif authed and cmd == "EXEC" and queued is not None:
                            if any(self.key_versions.get(k, 0) != v for k, v in watched.items()):
                                reply = b"*-1\r\n"
                            else:
                                replies = [self._execute(c, a, authed, db_idx, addr) for c, a in queued]
                                reply = f"*{len(replies)}\r\n".encode("ascii") + b"".join(replies)
                            watched.clear()
                            queued = None
                        elif queued is not None:
                            queued.append((cmd, args))
                            reply = _encode_resp("QUEUED")
                        else:
                            reply = self._execute(cmd, args, authed, db_idx, addr)
                    if isinstance(reply, tuple) and reply[0] == "__STATE__":
                        _, authed, db_idx, payload = reply
                        conn.sendall(payload)
                    else:
                        conn.sendall(reply)
                except Exception as exc:
                    conn.sendall(f"-ERR {exc}\r\n".encode("utf-8"))

    def _execute(self, cmd, args, authed, db_idx, addr):
        reply = self._dispatch(cmd, args, authed, db_idx, addr)
        if cmd in ("SET", "HSET", "HDEL", "RPUSH", "SADD", "SREM", "ZADD", "ZREM", "DEL", "PERSIST", "EXPIRE"):
            if not reply.startswith(b"-") and reply != _encode_resp(None):
                key = (db_idx, args[0])
                self.key_versions[key] = self.key_versions.get(key, 0) + 1
        return reply

    def _dispatch(self, cmd, args, authed, db_idx, addr):
        if cmd == "PING":
            return _encode_resp("PONG")
        if cmd == "AUTH":
            if self.password is None:
                return ("__STATE__", True, db_idx, _encode_resp("OK"))
            if len(args) == 1:
                user, pw = "", args[0].decode("utf-8", errors="replace")
            elif len(args) >= 2:
                user = args[0].decode("utf-8", errors="replace")
                pw = args[1].decode("utf-8", errors="replace")
            else:
                return b"-ERR wrong number of arguments for 'auth' command\r\n"
            expected_user = self.username
            if pw == self.password and (not expected_user or user in ("", "default", expected_user)):
                return ("__STATE__", True, db_idx, _encode_resp("OK"))
            return b"-WRONGPASS invalid username-password pair or user is disabled.\r\n"
        if not authed:
            return b"-NOAUTH Authentication required.\r\n"
        if cmd == "SELECT":
            new_db = int(args[0].decode("ascii"))
            if not (0 <= new_db < 16):
                return b"-ERR DB index is out of range\r\n"
            return ("__STATE__", authed, new_db, _encode_resp("OK"))

        with self.lock:
            db = self.dbs[db_idx]
            if cmd == "EXISTS":
                return _encode_resp(int(args[0].decode("utf-8") in db))
            if cmd == "INFO":
                return _encode_resp(self._info_text().encode("utf-8"))
            if cmd == "SCAN":
                cur = int(args[0].decode("ascii"))
                pattern = "*"
                count = 100
                i = 1
                while i + 1 < len(args):
                    opt = args[i].decode("ascii").upper()
                    if opt == "MATCH":
                        pattern = args[i + 1].decode("utf-8", errors="replace")
                    elif opt == "COUNT":
                        count = max(1, int(args[i + 1].decode("ascii")))
                    i += 2
                all_keys = sorted(k for k in db if fnmatch.fnmatchcase(k, pattern))
                page_size = min(count, 40)
                batch = all_keys[cur : cur + page_size]
                next_cur = cur + page_size if cur + page_size < len(all_keys) else 0
                return _encode_resp([str(next_cur).encode("ascii"), [k.encode("utf-8") for k in batch]])
            if cmd == "TYPE":
                key = args[0].decode("utf-8", errors="replace")
                entry = db.get(key)
                return _encode_resp(entry["kind"] if entry else "none")
            if cmd == "TTL":
                key = args[0].decode("utf-8", errors="replace")
                entry = db.get(key)
                if not entry:
                    return _encode_resp(-2)
                return _encode_resp(self._ttl_of(entry))
            if cmd == "MEMORY":
                key = args[1].decode("utf-8", errors="replace")
                entry = db.get(key)
                if not entry:
                    return _encode_resp(None)
                val = entry["value"]
                size = len(val) if isinstance(val, bytes) else 128
                return _encode_resp(size + 48)
            if cmd == "OBJECT":
                return _encode_resp(b"embstr")
            if cmd == "STRLEN":
                entry = db.get(args[0].decode("utf-8", errors="replace"))
                return _encode_resp(len(entry["value"]) if entry and entry["kind"] == "string" else 0)
            if cmd == "GET":
                entry = db.get(args[0].decode("utf-8", errors="replace"))
                return _encode_resp(entry["value"] if entry and entry["kind"] == "string" else None)
            if cmd == "GETRANGE":
                entry = db.get(args[0].decode("utf-8", errors="replace"))
                if not entry or entry["kind"] != "string":
                    return _encode_resp(b"")
                start, end = int(args[1]), int(args[2])
                return _encode_resp(entry["value"][start : end + 1])
            if cmd == "SET":
                key = args[0].decode("utf-8", errors="replace")
                val = args[1]
                exp = None
                opts = [a.decode("ascii", errors="replace").upper() for a in args[2:]]
                if "KEEPTTL" in opts and int(self.version.split(".")[0]) < 6:
                    return b"-ERR syntax error\r\n"
                if ("NX" in opts and key in db) or ("XX" in opts and key not in db):
                    return _encode_resp(None)
                if "KEEPTTL" in opts and key in db:
                    exp = db[key].get("expire_at")
                elif len(args) >= 4 and opts[0] == "EX":
                    exp = time.time() + int(args[3].decode("ascii"))
                elif len(args) >= 4 and opts[0] == "PX":
                    exp = time.time() + int(args[3].decode("ascii")) / 1000.0
                db[key] = {"kind": "string", "value": val, "expire_at": exp}
                return _encode_resp("OK")
            if cmd == "PTTL":
                key = args[0].decode("utf-8", errors="replace")
                entry = db.get(key)
                if not entry:
                    return _encode_resp(-2)
                exp = entry.get("expire_at")
                if exp is None:
                    return _encode_resp(-1)
                return _encode_resp(max(1, int((exp - time.time()) * 1000)))
            if cmd == "HLEN":
                entry = db.get(args[0].decode("utf-8", errors="replace"))
                return _encode_resp(len(entry["value"]) if entry and entry["kind"] == "hash" else 0)
            if cmd == "HSCAN":
                entry = db.get(args[0].decode("utf-8", errors="replace"))
                flat = []
                if entry and entry["kind"] == "hash":
                    for fk in sorted(entry["value"]):
                        flat.append(fk.encode("utf-8"))
                        flat.append(entry["value"][fk])
                return _encode_resp([b"0", flat])
            if cmd == "HSET":
                key = args[0].decode("utf-8", errors="replace")
                field = args[1].decode("utf-8", errors="replace")
                val = args[2]
                entry = db.setdefault(key, {"kind": "hash", "value": {}, "expire_at": None})
                if entry["kind"] != "hash":
                    return b"-WRONGTYPE Operation against a key holding the wrong kind of value\r\n"
                added = 0 if field in entry["value"] else 1
                entry["value"][field] = val
                return _encode_resp(added)
            if cmd == "HDEL":
                key = args[0].decode("utf-8", errors="replace")
                field = args[1].decode("utf-8", errors="replace")
                entry = db.get(key)
                if entry and entry["kind"] == "hash" and field in entry["value"]:
                    del entry["value"][field]
                    return _encode_resp(1)
                return _encode_resp(0)
            if cmd == "LLEN":
                entry = db.get(args[0].decode("utf-8", errors="replace"))
                return _encode_resp(len(entry["value"]) if entry and entry["kind"] == "list" else 0)
            if cmd == "LRANGE":
                entry = db.get(args[0].decode("utf-8", errors="replace"))
                if not entry or entry["kind"] != "list":
                    return _encode_resp([])
                start, end = int(args[1]), int(args[2])
                items = entry["value"]
                if end < 0:
                    end = len(items) + end
                return _encode_resp(items[start : end + 1])
            if cmd == "RPUSH":
                key = args[0].decode("utf-8", errors="replace")
                entry = db.setdefault(key, {"kind": "list", "value": [], "expire_at": None})
                if entry["kind"] != "list":
                    return b"-WRONGTYPE Operation against a key holding the wrong kind of value\r\n"
                entry["value"].extend(args[1:])
                return _encode_resp(len(entry["value"]))
            if cmd == "SCARD":
                entry = db.get(args[0].decode("utf-8", errors="replace"))
                return _encode_resp(len(entry["value"]) if entry and entry["kind"] == "set" else 0)
            if cmd == "SSCAN":
                entry = db.get(args[0].decode("utf-8", errors="replace"))
                items = sorted(entry["value"]) if entry and entry["kind"] == "set" else []
                return _encode_resp([b"0", items])
            if cmd == "SADD":
                key = args[0].decode("utf-8", errors="replace")
                entry = db.setdefault(key, {"kind": "set", "value": set(), "expire_at": None})
                if entry["kind"] != "set":
                    return b"-WRONGTYPE Operation against a key holding the wrong kind of value\r\n"
                before = len(entry["value"])
                for m in args[1:]:
                    entry["value"].add(m)
                return _encode_resp(len(entry["value"]) - before)
            if cmd == "SREM":
                key = args[0].decode("utf-8", errors="replace")
                entry = db.get(key)
                if entry and entry["kind"] == "set":
                    before = len(entry["value"])
                    for m in args[1:]:
                        entry["value"].discard(m)
                    return _encode_resp(before - len(entry["value"]))
                return _encode_resp(0)
            if cmd == "ZCARD":
                entry = db.get(args[0].decode("utf-8", errors="replace"))
                return _encode_resp(len(entry["value"]) if entry and entry["kind"] == "zset" else 0)
            if cmd == "ZRANGE":
                entry = db.get(args[0].decode("utf-8", errors="replace"))
                if not entry or entry["kind"] != "zset":
                    return _encode_resp([])
                ordered = sorted(entry["value"].items(), key=lambda kv: (kv[1], kv[0]))
                out = []
                for member, score in ordered:
                    out.append(member)
                    if any(a.upper() == b"WITHSCORES" for a in args[3:]):
                        out.append(f"{score:g}".encode("ascii"))
                return _encode_resp(out)
            if cmd == "ZADD":
                key = args[0].decode("utf-8", errors="replace")
                score = float(args[1].decode("ascii"))
                member = args[2]
                entry = db.setdefault(key, {"kind": "zset", "value": {}, "expire_at": None})
                if entry["kind"] != "zset":
                    return b"-WRONGTYPE Operation against a key holding the wrong kind of value\r\n"
                added = 0 if member in entry["value"] else 1
                entry["value"][member] = score
                return _encode_resp(added)
            if cmd == "ZREM":
                key = args[0].decode("utf-8", errors="replace")
                entry = db.get(key)
                if entry and entry["kind"] == "zset" and args[1] in entry["value"]:
                    del entry["value"][args[1]]
                    return _encode_resp(1)
                return _encode_resp(0)
            if cmd == "XLEN":
                entry = db.get(args[0].decode("utf-8", errors="replace"))
                return _encode_resp(len(entry["value"]) if entry and entry["kind"] == "stream" else 0)
            if cmd == "XREVRANGE":
                entry = db.get(args[0].decode("utf-8", errors="replace"))
                if not entry or entry["kind"] != "stream":
                    return _encode_resp([])
                out = []
                for eid, fields in reversed(entry["value"]):
                    flat = []
                    for fk, fv in fields.items():
                        flat.append(fk.encode("utf-8"))
                        flat.append(fv)
                    out.append([eid.encode("utf-8"), flat])
                return _encode_resp(out)
            if cmd == "EXPIRE":
                key = args[0].decode("utf-8", errors="replace")
                secs = int(args[1].decode("ascii"))
                entry = db.get(key)
                if not entry:
                    return _encode_resp(0)
                entry["expire_at"] = time.time() + secs
                return _encode_resp(1)
            if cmd == "PERSIST":
                key = args[0].decode("utf-8", errors="replace")
                entry = db.get(key)
                if not entry or entry.get("expire_at") is None:
                    return _encode_resp(0)
                entry["expire_at"] = None
                return _encode_resp(1)
            if cmd == "DEL":
                removed = 0
                for raw_key in args:
                    if db.pop(raw_key.decode("utf-8", errors="replace"), None) is not None:
                        removed += 1
                return _encode_resp(removed)
            if cmd == "CLIENT":
                clients = (
                    f"id=10 addr=127.0.0.1:51200 fd=8 name=api-worker age=420 idle=2 flags=N db=0 cmd=hgetall\n"
                    f"id=11 addr=127.0.0.1:51201 fd=9 name=scheduler age=310 idle=5 flags=N db=1 cmd=lrange\n"
                    f"id=12 addr={addr[0]}:{addr[1]} fd=10 name=arcnix-probe age=1 idle=0 flags=N db={db_idx} cmd=client|list\n"
                )
                return _encode_resp(clients.encode("utf-8"))
            if cmd == "SLOWLOG":
                return _encode_resp(self.slowlog)
        return f"-ERR unknown command '{cmd}'\r\n".encode("utf-8")


class RedisClusterFixture:
    """Manages host + password-protected Docker Redis test instances for one SSH fixture root."""

    def __init__(self, root: Path):
        self.root = root
        self.state_file = root / "redis-fixture.json"
        self.host_server = RespTestServer(password=None, version="7.2.5", seed_large=True)
        self.docker_server = RespTestServer(password="s3cr3t", version="7.4.1", seed_large=False)
        self.lock = threading.Lock()
        self.host_running = True
        self.docker_running = True
        self.docker_id = "c" * 64
        self._sync_state_file()

    def _sync_state_file(self):
        data = {
            "host_port": self.host_server.port,
            "host_running": self.host_running,
            "docker_id": self.docker_id,
            "docker_name": "redis-auth",
            "docker_port": self.docker_server.port,
            "docker_running": self.docker_running,
            "docker_password": "s3cr3t",
        }
        self.state_file.write_text(json.dumps(data), encoding="utf-8")

    def set_running(self, target: str, running: bool):
        with self.lock:
            if target == "host":
                self.host_running = running
            elif target == "docker":
                self.docker_running = running
            self._sync_state_file()

    def build_discover_response(self, helper_mod, auth_map):
        with self.lock:
            host_running = self.host_running
            docker_running = self.docker_running
        instances = []
        host_ep = {
            "host": "127.0.0.1",
            "port": self.host_server.port,
            "socket": None,
            "container_id": None,
        }
        if host_running:
            auth_status, info_text = helper_mod.probe_instance_info(host_ep, auth_map.get("host"))
        else:
            auth_status = {"kind": "unreachable", "message": "Redis is not running"}
            info_text = None
        instances.append(
            {
                "key": "host",
                "title": "On the server",
                "service": {
                    "unit": "redis-server.service",
                    "load": "loaded",
                    "active": "active" if host_running else "inactive",
                    "sub": "running" if host_running else "dead",
                    "enabled": "enabled",
                    "uptime": 7200 if host_running else None,
                },
                "docker": None,
                "endpoint": host_ep,
                "installed": True,
                "running": host_running,
                "auth_status": auth_status,
                "info": info_text,
                "config_path": "/etc/redis/redis.conf",
            }
        )

        docker_key = f"docker:{self.docker_id}"
        docker_ep = {
            "host": "127.0.0.1",
            "port": self.docker_server.port,
            "socket": None,
            "container_id": self.docker_id,
        }
        if docker_running:
            d_auth_status, d_info_text = helper_mod.probe_instance_info(docker_ep, auth_map.get(docker_key))
        else:
            d_auth_status = {"kind": "unreachable", "message": "Container is not running"}
            d_info_text = None
        instances.append(
            {
                "key": docker_key,
                "title": "redis-auth",
                "service": None,
                "docker": {
                    "id": self.docker_id,
                    "name": "redis-auth",
                    "image": "redis:7.4-alpine",
                    "state": "running" if docker_running else "exited",
                    "uptime": 3600 if docker_running else None,
                    "restart": "unless-stopped",
                    "project": "cache-stack",
                    "service": "redis",
                    "directory": "/srv/cache-stack",
                    "ports": [f"127.0.0.1:{self.docker_server.port}->6379/tcp"],
                    "host": "127.0.0.1",
                    "port": self.docker_server.port,
                },
                "endpoint": docker_ep,
                "installed": True,
                "running": docker_running,
                "auth_status": d_auth_status,
                "info": d_info_text,
                "config_path": None,
            }
        )
        return {"instances": instances, "docker_error": ""}

    def close(self):
        self.host_server.close()
        self.docker_server.close()


_clusters: dict[Path, RedisClusterFixture] = {}
_clusters_lock = threading.Lock()


def cluster_for(root: Path) -> RedisClusterFixture:
    with _clusters_lock:
        existing = _clusters.get(root)
        if existing is None:
            existing = RedisClusterFixture(root)
            _clusters[root] = existing
        return existing


def handle_helper_request(root: Path, stdin_bytes: bytes) -> tuple[int, str, str]:
    """Run Arcnix's real `redis_helper.py` handlers against the live RESP2 test servers."""
    import importlib.util

    helper_path = Path(__file__).resolve().parents[1] / "crates/arcnix-core/src/software/redis_helper.py"
    spec = importlib.util.spec_from_file_location("arcnix_redis_helper", helper_path)
    mod = importlib.util.module_from_spec(spec)
    assert spec and spec.loader
    spec.loader.exec_module(mod)

    cluster = cluster_for(root)
    try:
        req = json.loads(stdin_bytes.decode("utf-8")) if stdin_bytes.strip() else {"op": "discover"}
        op = req.get("op", "discover")
        if op == "discover":
            result = cluster.build_discover_response(mod, req.get("auth") or {})
        elif op == "scan":
            result = mod.op_scan(req)
        elif op == "get_key":
            result = mod.op_get_key(req)
        elif op == "diagnostics":
            result = mod.op_diagnostics(req)
        elif op == "write":
            result = mod.op_write(req)
        else:
            return 1, "", f"Unknown Redis helper operation: {op}\n"
        return 0, json.dumps(result, ensure_ascii=False), ""
    except mod.RespError as exc:
        return 2, "", f"REDIS_ERROR: {exc}\n"
    except Exception as exc:
        return 1, "", f"{exc}\n"
