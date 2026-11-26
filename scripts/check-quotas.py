#!/usr/bin/env python3
"""Report plan usage windows for antigravity, claude-code and codex as one JSON object.

Reads the local engine's account snapshots over its IPC WebSocket (`ListAgentAccounts` and
`ListManagedAccounts`, with `forceUsage` unless --cached), and runs the Antigravity CLI's
structured `agy --output-format json --print /usage`. Standard library only; the WebSocket
client is written here so this runs on any machine with python3.

  python3 -B scripts/check-quotas.py [--url ws://127.0.0.1:27654/] [--cached] [--timeout S]
                                     [--agy PATH | --no-agy]

The engine port defaults to MAST_IPC_PORT, else 27654. Only loopback URLs are accepted, since
the local IPC carries no bearer token and must not leave this machine.

Output: a JSON object on stdout. Each window has usedPercent and remainingPercent (0-100).
Exit: 0 when every harness has at least one window, 1 otherwise (the JSON still prints, with the
reasons in "errors"), 2 for a usage error.
"""
import argparse
import base64
import datetime
import hashlib
import itertools
import json
import os
import shutil
import socket
import struct
import subprocess
import sys
from urllib.parse import urlsplit

HARNESSES = ("antigravity", "claude-code", "codex")
WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
DEFAULT_PORT = 27654
LOOPBACK = ("127.0.0.1", "localhost")


class RpcFailure(Exception):
    """The engine answered a request with an error frame."""


class WebSocket:
    """Client side of RFC 6455 for text frames, with ping/pong and fragmented messages."""

    def __init__(self, url, timeout):
        parts = urlsplit(url)
        if parts.scheme != "ws" or parts.hostname not in LOOPBACK:
            raise ValueError(f"only ws:// loopback URLs are accepted, got {url}")
        self.sock = socket.create_connection((parts.hostname, parts.port or 80), timeout=timeout)
        self.buffer = b""
        path = (parts.path or "/") + (f"?{parts.query}" if parts.query else "")
        key = base64.b64encode(os.urandom(16)).decode()
        request = (
            f"GET {path} HTTP/1.1\r\nHost: {parts.netloc}\r\nUpgrade: websocket\r\n"
            f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )
        self.sock.sendall(request.encode())
        head = self._read_head()
        lines = head.split("\r\n")
        if not lines[0].startswith("HTTP/1.1 101"):
            raise ConnectionError(f"handshake refused: {lines[0]}")
        headers = {}
        for line in lines[1:]:
            if ":" in line:
                name, value = line.split(":", 1)
                headers[name.strip().lower()] = value.strip()
        expected = base64.b64encode(hashlib.sha1((key + WS_GUID).encode()).digest()).decode()
        if headers.get("sec-websocket-accept") != expected:
            raise ConnectionError("handshake: bad Sec-WebSocket-Accept")

    def _read_head(self):
        data = b""
        while b"\r\n\r\n" not in data:
            chunk = self.sock.recv(4096)
            if not chunk:
                raise ConnectionError("engine closed the connection during the handshake")
            data += chunk
        head, _, rest = data.partition(b"\r\n\r\n")
        self.buffer = rest
        return head.decode("latin-1")

    def _recv_exact(self, count):
        while len(self.buffer) < count:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise ConnectionError("engine closed the connection")
            self.buffer += chunk
        out, self.buffer = self.buffer[:count], self.buffer[count:]
        return out

    def _recv_frame(self):
        first, second = self._recv_exact(2)
        fin = bool(first & 0x80)
        opcode = first & 0x0F
        masked = bool(second & 0x80)
        length = second & 0x7F
        if length == 126:
            length = struct.unpack("!H", self._recv_exact(2))[0]
        elif length == 127:
            length = struct.unpack("!Q", self._recv_exact(8))[0]
        mask = self._recv_exact(4) if masked else None
        payload = self._recv_exact(length) if length else b""
        if mask:
            payload = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
        return fin, opcode, payload

    def send_frame(self, opcode, payload):
        # Client frames must be masked (RFC 6455 section 5.3).
        header = bytearray([0x80 | opcode])
        size = len(payload)
        if size < 126:
            header.append(0x80 | size)
        elif size < 1 << 16:
            header.append(0x80 | 126)
            header += struct.pack("!H", size)
        else:
            header.append(0x80 | 127)
            header += struct.pack("!Q", size)
        mask = os.urandom(4)
        header += mask
        masked = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
        self.sock.sendall(bytes(header) + masked)

    def recv_text(self):
        parts = []
        while True:
            fin, opcode, payload = self._recv_frame()
            if opcode == 0x9:  # ping
                self.send_frame(0xA, payload)
                continue
            if opcode == 0xA:  # pong
                continue
            if opcode == 0x8:
                raise ConnectionError("engine sent a close frame")
            if opcode in (0x1, 0x2):
                parts = []
            if opcode in (0x0, 0x1, 0x2):
                parts.append(payload)
                if fin:
                    return b"".join(parts).decode("utf-8")

    def call(self, ids, method, params):
        """One unary request. `ids` yields the integer u64 request ids."""
        request_id = next(ids)
        self.send_frame(0x1, json.dumps({"id": request_id, "method": method, "params": params}).encode())
        while True:
            frame = json.loads(self.recv_text())
            if frame.get("id") != request_id:
                continue
            if frame.get("err") is not None:
                raise RpcFailure(f"{method}: {frame['err']}")
            if "ok" in frame:
                return frame["ok"]

    def close(self):
        try:
            self.send_frame(0x8, b"")
        except OSError:
            pass
        self.sock.close()


def window(raw):
    used = min(max(float(raw.get("usedFraction") or 0.0), 0.0), 1.0)
    return {
        "label": raw.get("label"),
        "usedPercent": round(used * 100, 1),
        "remainingPercent": round((1.0 - used) * 100, 1),
        "resetsAt": raw.get("resetsAt"),
    }


def collect_engine(url, timeout, force, errors):
    """Accounts and windows from the engine. Returns (accounts, warnings, reachable)."""
    accounts, warnings = [], []
    try:
        client = WebSocket(url, timeout)
    except (OSError, ConnectionError, ValueError) as error:
        errors.append(f"engine at {url} is not reachable: {error}")
        return accounts, warnings, False
    ids = itertools.count(1)
    try:
        snapshot = client.call(ids, "ListAgentAccounts", {"forceUsage": force})
        for raw in snapshot.get("accounts") or []:
            accounts.append({
                "harness": raw.get("harness"),
                "source": "agent-account",
                "id": raw.get("id"),
                "label": None,
                "active": raw.get("active"),
                "signedIn": None,
                "planLabel": raw.get("planLabel"),
                "note": None,
                "windows": [window(w) for w in raw.get("usageWindows") or []],
            })
        for raw in snapshot.get("warnings") or []:
            warnings.append(f"{raw.get('harness')}: {raw.get('message')}")
        managed = client.call(ids, "ListManagedAccounts", {"forceUsage": force})
        for raw in managed.get("accounts") or []:
            accounts.append({
                "harness": raw.get("harness"),
                "source": "managed-account",
                "id": raw.get("id"),
                "label": raw.get("label"),
                "active": None,
                "signedIn": raw.get("signedIn"),
                "planLabel": raw.get("planLabel"),
                "note": raw.get("usageNote"),
                "windows": [window(w) for w in raw.get("usageWindows") or []],
            })
    except (RpcFailure, OSError, ConnectionError, ValueError) as error:
        errors.append(f"engine account snapshot failed: {error}")
    finally:
        client.close()
    return accounts, warnings, True


def agy_windows(body):
    """Windows from `agy --output-format json --print /usage` (command.data.groups[].buckets[])."""
    groups = ((body.get("command") or {}).get("data") or {}).get("groups")
    if not isinstance(groups, list):
        raise ValueError("agy /usage JSON has no command.data.groups list")
    names = {"5h": "Session", "weekly": "Week"}
    result = []
    for group in groups:
        group_name = group.get("name") or ""
        for bucket in group.get("buckets") or []:
            remaining = bucket.get("remaining_fraction")
            remaining = 1.0 if remaining is None else float(remaining)
            used = min(max(1.0 - remaining, 0.0), 1.0)
            kind = bucket.get("window") or ""
            label = names.get(kind) or bucket.get("name") or group_name or kind
            result.append({
                "label": label,
                "usedPercent": round(used * 100, 1),
                "remainingPercent": round((1.0 - used) * 100, 1),
                "resetsAt": bucket.get("reset_time"),
            })
    return result


def collect_agy(exe, timeout, errors):
    try:
        done = subprocess.run([exe, "--output-format", "json", "--print", "/usage"],
                              capture_output=True, text=True, timeout=timeout, check=False)
        if done.returncode != 0:
            raise ValueError(f"exit {done.returncode}")
        return agy_windows(json.loads(done.stdout))
    except (OSError, subprocess.SubprocessError, ValueError) as error:
        errors.append(f"agy /usage ({exe}): {error}")
        return None


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    port = os.environ.get("MAST_IPC_PORT", "").strip()
    parser.add_argument("--url", default=f"ws://127.0.0.1:{int(port) if port.isdigit() else DEFAULT_PORT}/")
    parser.add_argument("--cached", action="store_true", help="do not force a fresh usage probe")
    parser.add_argument("--timeout", type=float, default=90.0, help="seconds per engine call")
    parser.add_argument("--agy", default=os.environ.get("MAST_AGY") or shutil.which("agy") or "agy")
    parser.add_argument("--no-agy", action="store_true", help="skip the agy /usage read")
    args = parser.parse_args()

    errors = []
    accounts, warnings, reachable = collect_engine(args.url, args.timeout, not args.cached, errors)
    if not args.no_agy:
        windows = collect_agy(args.agy, min(args.timeout, 30.0), errors)
        if windows is not None:
            accounts.append({"harness": "antigravity", "source": "agy /usage", "id": "agy",
                             "label": None, "active": True, "signedIn": None, "planLabel": None,
                             "note": None,
                             "windows": windows})

    harnesses = {}
    for harness in HARNESSES:
        rows = [a for a in accounts if a["harness"] == harness]
        ready = any(a["windows"] for a in rows)
        if not ready:
            errors.append(f"no usage windows for {harness}")
        harnesses[harness] = {"ready": ready, "accounts": rows}

    result = {
        "checkedAt": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "engineReachable": reachable,
        "forcedUsageProbe": not args.cached,
        "ok": all(h["ready"] for h in harnesses.values()),
        "harnesses": harnesses,
        "warnings": warnings,
        "errors": errors,
    }
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0 if result["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
