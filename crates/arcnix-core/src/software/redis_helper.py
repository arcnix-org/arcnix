"""Bounded Redis / Valkey inspection and key operations for Arcnix.

Reads a single JSON request on stdin so credentials and values never appear on
command lines or in process listings. Speaks RESP2 directly over loopback TCP
or Unix domain sockets using the Python standard library (with container IP /
published-port discovery for Docker Redis containers).
"""
import json
import os
import re
import shlex
import shutil
import socket
import subprocess
import sys
import time

SOCKET_TIMEOUT = 5.0
CONTAINER_LIMIT = 20
SCAN_PAGE_LIMIT = 100
SCAN_ITERATION_LIMIT = 8
COLLECTION_ITEM_LIMIT = 200
STRING_PREVIEW_LIMIT = 128 * 1024
CLIENTS_LIMIT = 200
SLOWLOG_LIMIT = 64

STDIN_LIMIT = 2 * 1024 * 1024
MAX_RESP_LINE_BYTES = 64 * 1024
MAX_RESP_BULK_BYTES = 4 * 1024 * 1024
MAX_RESP_ARRAY_LEN = 10_000
MAX_RESP_DEPTH = 8
MAX_RESP_TOTAL_BYTES = 12 * 1024 * 1024

DOCKER_ENV_STRIP = ("DOCKER_HOST", "DOCKER_CONTEXT", "DOCKER_TLS_VERIFY", "DOCKER_CERT_PATH")


class RespError(Exception):
    pass


class RespClient:
    """Minimal bounded, binary-safe RESP2 client over TCP or Unix socket."""

    def __init__(self, host=None, port=6379, unix_socket=None, timeout=SOCKET_TIMEOUT):
        if unix_socket:
            sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            sock.settimeout(timeout)
            sock.connect(unix_socket)
        else:
            sock = socket.create_connection((host or "127.0.0.1", int(port)), timeout=timeout)
            sock.settimeout(timeout)
        self.sock = sock
        self.buf = bytearray()
        self.total_read = 0

    def close(self):
        try:
            self.sock.close()
        except OSError:
            pass

    def _recv_more(self, want):
        chunk = self.sock.recv(max(4096, min(want, 65536)))
        if not chunk:
            raise EOFError("Connection closed by Redis")
        self.total_read += len(chunk)
        if self.total_read > MAX_RESP_TOTAL_BYTES:
            raise ValueError("Redis reply exceeded total size limit")
        self.buf.extend(chunk)

    def _read_exact(self, n):
        if n > MAX_RESP_BULK_BYTES:
            raise ValueError("Redis bulk string exceeded size limit")
        while len(self.buf) < n:
            self._recv_more(n - len(self.buf))
        data = bytes(self.buf[:n])
        del self.buf[:n]
        return data

    def _read_line(self):
        while True:
            idx = self.buf.find(b"\r\n")
            if idx != -1:
                if idx > MAX_RESP_LINE_BYTES:
                    raise ValueError("Redis protocol line exceeded size limit")
                line = bytes(self.buf[:idx])
                del self.buf[: idx + 2]
                return line
            if len(self.buf) > MAX_RESP_LINE_BYTES:
                raise ValueError("Redis protocol line exceeded size limit")
            self._recv_more(4096)

    def _read_reply(self, depth=0):
        if depth > MAX_RESP_DEPTH:
            raise ValueError("Redis reply nesting exceeded depth limit")
        line = self._read_line()
        if not line:
            raise EOFError("Empty reply from Redis")
        prefix = line[:1]
        payload = line[1:]
        if prefix == b"+":
            return payload.decode("utf-8", errors="replace")
        if prefix == b"-":
            raise RespError(payload.decode("utf-8", errors="replace"))
        if prefix == b":":
            return int(payload)
        if prefix == b"$":
            length = int(payload)
            if length < 0:
                return None
            if length > MAX_RESP_BULK_BYTES:
                raise ValueError("Redis bulk string exceeded size limit")
            data = self._read_exact(length)
            crlf = self._read_exact(2)
            if crlf != b"\r\n":
                raise ValueError("Invalid RESP bulk string terminator")
            return data
        if prefix == b"*":
            count = int(payload)
            if count < 0:
                return None
            if count > MAX_RESP_ARRAY_LEN:
                raise ValueError("Redis array reply exceeded element limit")
            return [self._read_reply(depth + 1) for _ in range(count)]
        raise ValueError(f"Unsupported RESP reply prefix: {prefix!r}")

    def command(self, *args):
        parts = [f"*{len(args)}\r\n".encode("ascii")]
        for arg in args:
            if isinstance(arg, bytes):
                raw = arg
            else:
                raw = str(arg).encode("utf-8")
            parts.append(f"${len(raw)}\r\n".encode("ascii"))
            parts.append(raw)
            parts.append(b"\r\n")
        self.sock.sendall(b"".join(parts))
        return self._read_reply(0)


def _decode_utf8_maybe_truncated(raw, truncated):
    """Decode UTF-8; if truncated mid-codepoint, trim trailing incomplete bytes."""
    if not truncated:
        return raw.decode("utf-8")
    # At most 3 trailing bytes can belong to a split multi-byte UTF-8 character.
    for trim in range(min(4, len(raw) + 1)):
        candidate = raw[: len(raw) - trim] if trim else raw
        try:
            return candidate.decode("utf-8")
        except UnicodeDecodeError as exc:
            if exc.end < len(candidate):
                raise
    raise UnicodeDecodeError("utf-8", raw, 0, len(raw), "invalid utf-8")


def decode_bytes(value, limit=None, already_truncated=False):
    if value is None:
        return "", False, already_truncated
    if isinstance(value, str):
        raw = value.encode("utf-8", errors="replace")
    elif isinstance(value, bytes):
        raw = value
    else:
        return str(value), False, already_truncated
    truncated = already_truncated
    if limit is not None and len(raw) > limit:
        raw = raw[:limit]
        truncated = True
    try:
        text = _decode_utf8_maybe_truncated(raw, truncated)
        if "\x00" in text:
            raise UnicodeDecodeError("utf-8", raw, 0, 1, "nul byte")
        return text, False, truncated
    except UnicodeDecodeError:
        preview = " ".join(f"{b:02x}" for b in raw[:2048])
        return preview, True, truncated or len(raw) > 2048


def run_cmd(args, timeout=5):
    env = {k: v for k, v in os.environ.items() if k not in DOCKER_ENV_STRIP}
    try:
        proc = subprocess.run(
            args,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=timeout,
            env=env,
            check=False,
        )
        return proc.returncode, proc.stdout, proc.stderr
    except (OSError, subprocess.TimeoutExpired) as exc:
        return -1, "", str(exc)


def read_host_service():
    if not shutil.which("systemctl"):
        return None
    for unit in ("redis-server.service", "redis.service", "valkey-server.service", "valkey.service"):
        rc, out, _ = run_cmd(
            [
                "systemctl",
                "show",
                "--no-pager",
                "--property=LoadState,ActiveState,SubState,UnitFileState,ActiveEnterTimestampMonotonic",
                "--",
                unit,
            ],
            timeout=5,
        )
        if rc != 0:
            continue
        props = {}
        for line in out.splitlines():
            if "=" in line:
                k, v = line.split("=", 1)
                props[k.strip()] = v.strip()
        load = props.get("LoadState", "")
        if load and load != "not-found":
            uptime = None
            if props.get("ActiveState") == "active":
                raw_mono = props.get("ActiveEnterTimestampMonotonic", "")
                if raw_mono.isdigit() and int(raw_mono) > 0:
                    elapsed = int(time.monotonic() - (int(raw_mono) / 1_000_000.0))
                    if elapsed >= 0:
                        uptime = elapsed
            return {
                "unit": unit,
                "load": load,
                "active": props.get("ActiveState", ""),
                "sub": props.get("SubState", ""),
                "enabled": props.get("UnitFileState", ""),
                "uptime": uptime,
            }
    return None


def _parse_config_text(text):
    port = 6379
    unix_socket = None
    binds = []
    for raw_line in text.splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        try:
            parts = shlex.split(line, comments=True)
        except ValueError:
            parts = line.split()
        if len(parts) < 2:
            continue
        directive = parts[0].lower()
        if directive == "port" and parts[1].isdigit():
            parsed = int(parts[1])
            if 0 <= parsed <= 65535:
                port = parsed
        elif directive == "unixsocket":
            unix_socket = parts[1]
        elif directive == "bind":
            for token in parts[1:]:
                addr = token.lstrip("-")
                if addr:
                    binds.append(addr)

    host = "127.0.0.1"
    if binds:
        if "127.0.0.1" in binds or "0.0.0.0" in binds or "*" in binds:
            host = "127.0.0.1"
        elif "::1" in binds or "::" in binds:
            host = "::1"
        else:
            host = binds[0]
    if port == 0 and not unix_socket:
        return None
    return {"host": host, "port": port, "socket": unix_socket if port == 0 else None, "container_id": None}


def read_host_config_endpoint(unit_hint=None):
    valkey_first = bool(unit_hint and unit_hint.startswith("valkey"))
    candidates = (
        ["/etc/valkey/valkey.conf", "/etc/valkey.conf", "/etc/redis/redis.conf", "/etc/redis.conf"]
        if valkey_first
        else ["/etc/redis/redis.conf", "/etc/redis.conf", "/etc/valkey/valkey.conf", "/etc/valkey.conf"]
    )
    for path in candidates:
        if os.path.isfile(path):
            try:
                with open(path, "r", encoding="utf-8", errors="ignore") as f:
                    text = f.read(256 * 1024)
                return _parse_config_text(text), path
            except OSError:
                return {"host": "127.0.0.1", "port": 6379, "socket": None, "container_id": None}, path
    return {"host": "127.0.0.1", "port": 6379, "socket": None, "container_id": None}, None


def host_installed():
    for binary in ("redis-server", "valkey-server"):
        if shutil.which(binary):
            return True
    for path in (
        "/usr/bin/redis-server",
        "/usr/sbin/redis-server",
        "/usr/bin/valkey-server",
        "/usr/sbin/valkey-server",
    ):
        if os.path.isfile(path) and os.access(path, os.X_OK):
            return True
    return False


def docker_bin():
    found = shutil.which("docker")
    if found:
        return found
    if os.path.isfile("/snap/bin/docker") and os.access("/snap/bin/docker", os.X_OK):
        return "/snap/bin/docker"
    return None


def looks_like_redis(image, command):
    text = f"{image} {command}".lower()
    if any(skip in text for skip in ("exporter", "commander", "redisinsight", "redis-insight", "redis-cli")):
        return False
    # Match image repo names like redis, bitnami/redis, redis/redis-stack, valkey/valkey
    # or command binaries like redis-server / valkey-server.
    image_repo = image.lower().split("@", 1)[0].rsplit("/", 1)[-1].split(":", 1)[0]
    if image_repo in ("redis", "redis-stack", "redis-stack-server", "valkey", "valkey-bundle"):
        return True
    return bool(re.search(r'(^|[\s/"\'])(redis-server|valkey-server)([\s"\']|$)', command.lower()))


def parse_iso_uptime(started_at):
    if not started_at or started_at.startswith("0001-01-01"):
        return None
    cleaned = re.sub(r"\.\d+", "", started_at).replace("Z", "+00:00")
    try:
        from datetime import datetime, timezone

        dt = datetime.fromisoformat(cleaned)
        if dt.tzinfo is None:
            dt = dt.replace(tzinfo=timezone.utc)
        return max(0, int(time.time() - dt.timestamp()))
    except ValueError:
        return None


def _pick_container_endpoint(port_map, net_cfg):
    ports_list = []
    candidates = []
    for cport, bindings in (port_map or {}).items():
        if not bindings:
            continue
        for b in bindings:
            h_ip = b.get("HostIp") or "0.0.0.0"
            h_port = b.get("HostPort") or ""
            if not h_port:
                continue
            ports_list.append(f"{h_ip}:{h_port}->{cport}")
            if h_port.isdigit() and cport == "6379/tcp":
                port_num = int(h_port)
                if not (1 <= port_num <= 65535):
                    continue
                if h_ip in ("0.0.0.0", "", "127.0.0.1"):
                    target_host = "127.0.0.1"
                elif h_ip in ("::", "::1"):
                    target_host = "::1"
                else:
                    target_host = h_ip
                candidates.append((target_host, port_num))

    if candidates:
        connect_host, connect_port = candidates[0]
        return connect_host, connect_port, ports_list

    for net in (net_cfg.get("Networks") or {}).values():
        ip = (net or {}).get("IPAddress") or (net or {}).get("GlobalIPv6Address")
        if ip:
            return ip, 6379, ports_list

    return None, None, ports_list


def read_docker_containers():
    binary = docker_bin()
    if not binary:
        return [], ""
    rc, out, err = run_cmd([binary, "ps", "-a", "--no-trunc", "--format", "{{.ID}}\t{{.Image}}\t{{.Command}}"], timeout=6)
    if rc != 0:
        return [], (err.strip() or "Could not list Docker containers")
    ids = []
    for line in out.splitlines():
        parts = line.split("\t")
        if len(parts) >= 3 and looks_like_redis(parts[1], parts[2]):
            ids.append(parts[0].strip())
            if len(ids) >= CONTAINER_LIMIT:
                break
    if not ids:
        return [], ""
    rc, out, err = run_cmd([binary, "inspect", *ids], timeout=8)
    if rc != 0:
        return [], (err.strip() or "Could not inspect Docker containers")
    try:
        inspected = json.loads(out)
    except json.JSONDecodeError:
        return [], "Invalid Docker inspect output"

    containers = []
    for item in inspected:
        cid = item.get("Id", "")
        name = (item.get("Name") or "").lstrip("/") or cid[:12]
        config = item.get("Config") or {}
        state_obj = item.get("State") or {}
        host_cfg = item.get("HostConfig") or {}
        net_cfg = item.get("NetworkSettings") or {}
        labels = config.get("Labels") or {}

        state = state_obj.get("Status") or ("running" if state_obj.get("Running") else "exited")
        uptime = parse_iso_uptime(state_obj.get("StartedAt")) if state == "running" else None
        restart = (host_cfg.get("RestartPolicy") or {}).get("Name") or ""

        connect_host, connect_port, ports_list = _pick_container_endpoint(
            net_cfg.get("Ports") or {}, net_cfg
        )

        containers.append(
            {
                "id": cid,
                "name": name,
                "image": config.get("Image") or "",
                "state": state,
                "uptime": uptime,
                "restart": restart,
                "project": labels.get("com.docker.compose.project") or "",
                "service": labels.get("com.docker.compose.service") or "",
                "directory": labels.get("com.docker.compose.project.working_dir") or "",
                "ports": ports_list,
                "host": connect_host,
                "port": connect_port,
            }
        )
    return containers, ""


def connect_and_auth(endpoint, auth=None, db=None):
    client = RespClient(
        host=endpoint["host"],
        port=endpoint["port"],
        unix_socket=endpoint.get("socket") or None,
    )
    try:
        if auth and auth.get("password"):
            username = (auth.get("username") or "").strip()
            if username:
                client.command("AUTH", username, auth["password"])
            else:
                client.command("AUTH", auth["password"])
        if db is not None and int(db) > 0:
            client.command("SELECT", int(db))
        return client
    except Exception:
        client.close()
        raise


def classify_error(exc):
    msg = str(exc)
    upper = msg.upper()
    if "NOAUTH" in upper or "AUTHENTICATION REQUIRED" in upper:
        return {"kind": "required", "message": msg}
    if "WRONGPASS" in upper or "INVALID PASSWORD" in upper or "ERR AUTH" in upper or "NOPERM" in upper:
        return {"kind": "failed", "message": msg}
    return {"kind": "unreachable", "message": msg}


def _decode_lossy_text(value, limit=512 * 1024):
    if value is None:
        return ""
    if isinstance(value, bytes):
        return value[:limit].decode("utf-8", errors="replace")
    return str(value)[:limit]


def probe_instance_info(endpoint, auth=None):
    try:
        client = connect_and_auth(endpoint, auth=auth)
    except Exception as exc:
        return classify_error(exc), None
    try:
        raw = client.command("INFO")
        text = _decode_lossy_text(raw, limit=512 * 1024)
        return {"kind": "ok", "message": ""}, text
    except Exception as exc:
        return classify_error(exc), None
    finally:
        client.close()


def op_discover(req):
    auth_map = req.get("auth") or {}
    instances = []

    service = read_host_service()
    installed = host_installed() or service is not None
    endpoint, config_path = read_host_config_endpoint(service["unit"] if service else None)
    if installed:
        running = bool(service and service.get("active") == "active")
        auth_status = {"kind": "unreachable", "message": "Redis is not running"}
        info_text = None
        if endpoint is None:
            auth_status = {"kind": "unreachable", "message": "Redis has no configured plaintext port or Unix socket"}
        elif running or service is None:
            auth_status, info_text = probe_instance_info(endpoint, auth_map.get("host"))
            if auth_status["kind"] in ("ok", "required", "failed"):
                running = True
        instances.append(
            {
                "key": "host",
                "title": "On the server",
                "service": service,
                "docker": None,
                "endpoint": endpoint,
                "installed": True,
                "running": running,
                "auth_status": auth_status,
                "info": info_text,
                "config_path": config_path,
            }
        )

    containers, docker_error = read_docker_containers()
    for c in containers:
        key = f"docker:{c['id']}"
        ep = {
            "host": c["host"],
            "port": c["port"],
            "socket": None,
            "container_id": c["id"],
        } if c["host"] and c["port"] else None
        running = c["state"] == "running"
        if running and ep is not None:
            auth_status, info_text = probe_instance_info(ep, auth_map.get(key))
        elif running:
            auth_status = {"kind": "unreachable", "message": "No Redis endpoint found for this container"}
            info_text = None
        else:
            auth_status = {"kind": "unreachable", "message": "Container is not running"}
            info_text = None
        instances.append(
            {
                "key": key,
                "title": c["name"],
                "service": None,
                "docker": c,
                "endpoint": ep,
                "installed": True,
                "running": running,
                "auth_status": auth_status,
                "info": info_text,
                "config_path": None,
            }
        )

    return {"instances": instances, "docker_error": docker_error}


def op_scan(req):
    endpoint = req["endpoint"]
    auth = req.get("auth")
    db = int(req.get("db", 0))
    cursor = str(req.get("cursor", "0"))
    pattern = req.get("pattern") or "*"
    count = min(max(int(req.get("count", SCAN_PAGE_LIMIT)), 10), SCAN_PAGE_LIMIT)

    client = connect_and_auth(endpoint, auth=auth, db=db)
    try:
        found = []
        seen = set()
        cur = cursor
        for _ in range(SCAN_ITERATION_LIMIT):
            remaining = max(10, SCAN_PAGE_LIMIT - len(found))
            reply = client.command("SCAN", cur, "MATCH", pattern, "COUNT", min(count, remaining))
            if not isinstance(reply, list) or len(reply) != 2:
                raise ValueError("Unexpected SCAN response")
            next_cur = decode_bytes(reply[0])[0]
            batch = reply[1] or []
            for raw_key in batch:
                raw_bytes = raw_key if isinstance(raw_key, bytes) else str(raw_key).encode("utf-8")
                if raw_bytes in seen:
                    continue
                seen.add(raw_bytes)
                found.append(raw_bytes)
            cur = next_cur
            if cur == "0" or len(found) >= SCAN_PAGE_LIMIT:
                break

        truncated = cur != "0"
        keys = []
        for raw_bytes in found:
            name, is_binary, name_trunc = decode_bytes(raw_bytes, limit=4096)
            try:
                kind = client.command("TYPE", raw_bytes)
                if isinstance(kind, bytes):
                    kind = kind.decode("utf-8", errors="replace")
            except RespError:
                kind = "unknown"
            try:
                ttl = int(client.command("TTL", raw_bytes))
            except (RespError, ValueError, TypeError):
                ttl = -1
            size = None
            try:
                mem = client.command("MEMORY", "USAGE", raw_bytes, "SAMPLES", 5)
                if isinstance(mem, int):
                    size = mem
            except (RespError, ValueError, TypeError):
                size = None
            keys.append(
                {
                    "name": name,
                    "kind": kind or "unknown",
                    "ttl": ttl,
                    "size": size,
                    "binary": is_binary or name_trunc,
                }
            )

        return {"db": db, "cursor": cur, "keys": keys, "truncated": truncated}
    finally:
        client.close()


def _scan_hash(client, key):
    entries = []
    seen = set()
    binary = False
    truncated = False
    cur = "0"
    for _ in range(SCAN_ITERATION_LIMIT):
        reply = client.command("HSCAN", key, cur, "COUNT", COLLECTION_ITEM_LIMIT)
        if not isinstance(reply, list) or len(reply) != 2:
            break
        cur = decode_bytes(reply[0])[0]
        items = reply[1] or []
        for i in range(0, len(items) - 1, 2):
            if len(entries) >= COLLECTION_ITEM_LIMIT:
                truncated = True
                break
            fk, b1, t1 = decode_bytes(items[i], limit=4096)
            if fk in seen:
                continue
            seen.add(fk)
            fv, b2, t2 = decode_bytes(items[i + 1], limit=16 * 1024)
            binary = binary or b1 or b2
            truncated = truncated or t1 or t2
            entries.append({"label": fk, "value": fv})
        if cur == "0" or len(entries) >= COLLECTION_ITEM_LIMIT:
            if cur != "0":
                truncated = True
            break
    if cur != "0":
        truncated = True
    return entries, binary, truncated


def _scan_set(client, key):
    entries = []
    seen = set()
    binary = False
    truncated = False
    cur = "0"
    for _ in range(SCAN_ITERATION_LIMIT):
        reply = client.command("SSCAN", key, cur, "COUNT", COLLECTION_ITEM_LIMIT)
        if not isinstance(reply, list) or len(reply) != 2:
            break
        cur = decode_bytes(reply[0])[0]
        items = reply[1] or []
        for item in items:
            if len(entries) >= COLLECTION_ITEM_LIMIT:
                truncated = True
                break
            raw = item if isinstance(item, bytes) else str(item).encode("utf-8")
            if raw in seen:
                continue
            seen.add(raw)
            val, b, t = decode_bytes(raw, limit=16 * 1024)
            binary = binary or b
            truncated = truncated or t
            entries.append({"label": "", "value": val})
        if cur == "0" or len(entries) >= COLLECTION_ITEM_LIMIT:
            if cur != "0":
                truncated = True
            break
    if cur != "0":
        truncated = True
    return entries, binary, truncated


def op_get_key(req):
    endpoint = req["endpoint"]
    auth = req.get("auth")
    db = int(req.get("db", 0))
    key = req["key"]

    client = connect_and_auth(endpoint, auth=auth, db=db)
    try:
        kind = client.command("TYPE", key)
        if isinstance(kind, bytes):
            kind = kind.decode("utf-8", errors="replace")
        if kind == "none":
            raise RespError("Key does not exist")
        ttl = int(client.command("TTL", key))
        try:
            enc = client.command("OBJECT", "ENCODING", key)
            encoding = decode_bytes(enc)[0] if enc is not None else ""
        except (RespError, ValueError, TypeError):
            encoding = ""
        try:
            mem = client.command("MEMORY", "USAGE", key, "SAMPLES", 5)
            memory_bytes = mem if isinstance(mem, int) else None
        except (RespError, ValueError, TypeError):
            memory_bytes = None

        string_value = None
        entries = []
        length = None
        truncated = False
        binary = False

        if kind == "string":
            length = int(client.command("STRLEN", key))
            if length > STRING_PREVIEW_LIMIT:
                raw = client.command("GETRANGE", key, 0, STRING_PREVIEW_LIMIT - 1)
                truncated = True
            else:
                raw = client.command("GET", key)
            if raw is None:
                raise RespError("Key does not exist")
            text, binary, trunc2 = decode_bytes(raw, limit=STRING_PREVIEW_LIMIT, already_truncated=truncated)
            truncated = truncated or trunc2
            string_value = text
        elif kind == "hash":
            length = int(client.command("HLEN", key))
            entries, binary, truncated = _scan_hash(client, key)
            if length > len(entries):
                truncated = True
        elif kind == "list":
            length = int(client.command("LLEN", key))
            items = client.command("LRANGE", key, 0, COLLECTION_ITEM_LIMIT - 1) or []
            for idx, item in enumerate(items):
                val, b, t = decode_bytes(item, limit=16 * 1024)
                binary = binary or b
                truncated = truncated or t
                entries.append({"label": str(idx), "value": val})
            if length > len(entries):
                truncated = True
        elif kind == "set":
            length = int(client.command("SCARD", key))
            entries, binary, truncated = _scan_set(client, key)
            if length > len(entries):
                truncated = True
        elif kind == "zset":
            length = int(client.command("ZCARD", key))
            items = client.command("ZRANGE", key, 0, COLLECTION_ITEM_LIMIT - 1, "WITHSCORES") or []
            for i in range(0, len(items) - 1, 2):
                member, b, t = decode_bytes(items[i], limit=16 * 1024)
                score, _, _ = decode_bytes(items[i + 1], limit=128)
                binary = binary or b
                truncated = truncated or t
                entries.append({"label": score, "value": member})
            if length > len(entries):
                truncated = True
        elif kind == "stream":
            length = int(client.command("XLEN", key))
            items = client.command("XREVRANGE", key, "+", "-", "COUNT", min(50, COLLECTION_ITEM_LIMIT)) or []
            for entry in items:
                if isinstance(entry, list) and len(entry) == 2:
                    eid, _, _ = decode_bytes(entry[0], limit=128)
                    fields = entry[1] or []
                    pairs = {}
                    for i in range(0, len(fields) - 1, 2):
                        fk, _, _ = decode_bytes(fields[i], limit=1024)
                        fv, b, _ = decode_bytes(fields[i + 1], limit=4096)
                        binary = binary or b
                        pairs[fk] = fv
                    entries.append({"label": eid, "value": json.dumps(pairs, ensure_ascii=False)})
            if length > len(entries):
                truncated = True

        return {
            "name": key,
            "kind": kind,
            "ttl": ttl,
            "encoding": encoding,
            "memory_bytes": memory_bytes,
            "length": length,
            "truncated": truncated,
            "binary": binary,
            "string_value": string_value,
            "entries": entries,
        }
    finally:
        client.close()


def op_diagnostics(req):
    endpoint = req["endpoint"]
    auth = req.get("auth")
    client = connect_and_auth(endpoint, auth=auth)
    try:
        raw_clients = client.command("CLIENT", "LIST")
        clients_text = _decode_lossy_text(raw_clients, limit=512 * 1024)
        raw_slowlog = client.command("SLOWLOG", "GET", SLOWLOG_LIMIT) or []
        slowlog = []
        for entry in raw_slowlog:
            if not isinstance(entry, list) or len(entry) < 4:
                continue
            eid = int(entry[0])
            ts = int(entry[1])
            dur = int(entry[2])
            cmd_parts = entry[3] if isinstance(entry[3], list) else []
            cmd = " ".join(decode_bytes(p, limit=256)[0] for p in cmd_parts[:16])
            c_addr = decode_bytes(entry[4], limit=128)[0] if len(entry) > 4 else ""
            c_name = decode_bytes(entry[5], limit=128)[0] if len(entry) > 5 else ""
            slowlog.append(
                {
                    "id": eid,
                    "timestamp": ts,
                    "duration_us": dur,
                    "command": cmd,
                    "client_addr": c_addr,
                    "client_name": c_name,
                }
            )
        return {"clients_text": clients_text, "slowlog": slowlog}
    finally:
        client.close()


def op_write(req):
    endpoint = req["endpoint"]
    auth = req.get("auth")
    db = int(req.get("db", 0))
    op = req["write_op"]
    kind = op["kind"]
    create_only = req.get("create_only", False)

    client = connect_and_auth(endpoint, auth=auth, db=db)
    try:
        if create_only and kind not in ("set_string", "set_hash_field", "push_list", "add_set_member", "add_zset_member"):
            raise ValueError("Unsupported initial Redis value")
        if kind == "set_string":
            ttl = op.get("ttl_seconds")
            expiry = ("EX", int(ttl)) if ttl is not None else ()
            if create_only:
                if client.command("SET", op["key"], op["value"], *expiry, "NX") is None:
                    raise RespError("A key with this name already exists")
            else:
                edit_string(client, op["key"], op["value"], ("KEEPTTL",) if op.get("keep_ttl") else expiry)
        elif kind == "set_hash_field":
            write_collection(client, create_only, "HSET", op["key"], op["field"], op["value"])
        elif kind == "delete_hash_field":
            client.command("HDEL", op["key"], op["field"])
        elif kind == "push_list":
            write_collection(client, create_only, "RPUSH", op["key"], op["value"])
        elif kind == "add_set_member":
            write_collection(client, create_only, "SADD", op["key"], op["member"])
        elif kind == "remove_set_member":
            client.command("SREM", op["key"], op["member"])
        elif kind == "add_zset_member":
            write_collection(client, create_only, "ZADD", op["key"], str(op["score"]), op["member"])
        elif kind == "remove_zset_member":
            client.command("ZREM", op["key"], op["member"])
        elif kind == "set_ttl":
            ttl = op.get("ttl_seconds")
            if ttl is None:
                client.command("PERSIST", op["key"])
            else:
                client.command("EXPIRE", op["key"], int(ttl))
        elif kind == "delete_key":
            client.command("DEL", op["key"])
        else:
            raise ValueError(f"Unsupported write operation: {kind}")
        return {"ok": True}
    finally:
        client.close()


def edit_string(client, key, value, expiry):
    # TYPE and SET must observe the same key. XX also covers expirations on
    # older Redis versions where expiry alone does not invalidate WATCH.
    legacy_ttl = False
    while True:
        client.command("WATCH", key)
        if client.command("TYPE", key) != "string":
            raise RespError("This key no longer holds a string")
        if legacy_ttl:
            pttl = client.command("PTTL", key)
            if pttl == -2:
                raise RespError("This key no longer holds a string")
            expiry = () if pttl == -1 else ("PX", max(1, pttl))
        client.command("MULTI")
        client.command("SET", key, value, *expiry, "XX")
        try:
            result = client.command("EXEC")
        except RespError as exc:
            if expiry != ("KEEPTTL",) or "syntax" not in str(exc).lower():
                raise
            legacy_ttl = True
            continue
        if not result or result[0] is None:
            raise RespError("The key changed during editing. Reload it and try again")
        return


def write_collection(client, create_only, command, key, *args):
    if not create_only:
        client.command(command, key, *args)
        return
    # Collection commands have no key-level NX. Watch the absent key until
    # EXEC so a competing creator cannot have its initial value modified.
    client.command("WATCH", key)
    if client.command("EXISTS", key):
        raise RespError("A key with this name already exists")
    client.command("MULTI")
    client.command(command, key, *args)
    if client.command("EXEC") is None:
        raise RespError("The key changed during creation. Try again")


def main():
    raw = sys.stdin.read(STDIN_LIMIT + 1)
    if len(raw) > STDIN_LIMIT:
        raise ValueError("Redis request exceeds the size limit")
    if not raw.strip():
        raise ValueError("Missing Redis helper request")
    req = json.loads(raw)
    op = req.get("op")
    handlers = {
        "discover": op_discover,
        "scan": op_scan,
        "get_key": op_get_key,
        "diagnostics": op_diagnostics,
        "write": op_write,
    }
    if op not in handlers:
        raise ValueError(f"Unknown Redis helper operation: {op}")
    result = handlers[op](req)
    sys.stdout.write(json.dumps(result, ensure_ascii=False))


if __name__ == "__main__":
    try:
        main()
    except RespError as exc:
        sys.stderr.write(f"REDIS_ERROR: {exc}\n")
        sys.exit(2)
    except Exception as exc:
        sys.stderr.write(f"{exc}\n")
        sys.exit(1)
