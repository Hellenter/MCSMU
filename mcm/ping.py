"""Server List Ping: онлайн, версия и MOTD (Minecraft 1.7+)."""
import json
import socket
import struct
import time


def _varint(n):
    out = b""
    n &= 0xFFFFFFFF
    while True:
        b = n & 0x7F
        n >>= 7
        if n:
            out += bytes([b | 0x80])
        else:
            return out + bytes([b])


def _read_exact(sock, n):
    buf = b""
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            raise ConnectionError("соединение закрыто")
        buf += chunk
    return buf


def _read_varint(sock):
    num = 0
    for i in range(5):
        b = _read_exact(sock, 1)[0]
        num |= (b & 0x7F) << (7 * i)
        if not b & 0x80:
            return num
    raise ValueError("слишком длинный VarInt")


def _packet(pid, data=b""):
    body = _varint(pid) + data
    return _varint(len(body)) + body


def _flatten_text(component):
    if isinstance(component, str):
        return component
    if isinstance(component, list):
        return "".join(_flatten_text(c) for c in component)
    if isinstance(component, dict):
        text = component.get("text", "")
        for extra in component.get("extra", []):
            text += _flatten_text(extra)
        return text
    return ""


def strip_colors(text):
    out, skip = [], False
    for c in text:
        if skip:
            skip = False
            continue
        if c == "§":
            skip = True
            continue
        out.append(c)
    return "".join(out)


def ping(host="127.0.0.1", port=25565, timeout=2.0):
    """Возвращает dict(online, max, players, version, motd, latency_ms) или бросает исключение."""
    with socket.create_connection((host, port), timeout=timeout) as s:
        s.settimeout(timeout)
        host_b = host.encode("utf-8")
        handshake = _varint(-1) + _varint(len(host_b)) + host_b + struct.pack(">H", port) + _varint(1)
        s.sendall(_packet(0x00, handshake) + _packet(0x00))
        _read_varint(s)              # длина пакета
        _read_varint(s)              # id пакета
        length = _read_varint(s)
        data = json.loads(_read_exact(s, length).decode("utf-8", "replace"))
        t0 = time.perf_counter()
        s.sendall(_packet(0x01, struct.pack(">q", 1)))
        latency = None
        try:
            _read_varint(s)
            _read_varint(s)
            _read_exact(s, 8)
            latency = round((time.perf_counter() - t0) * 1000)
        except (OSError, ValueError):
            pass
    players = data.get("players", {}) or {}
    return {
        "online": players.get("online", 0),
        "max": players.get("max", 0),
        "players": [p.get("name", "?") for p in players.get("sample", []) or []],
        "version": (data.get("version") or {}).get("name", "?"),
        "motd": strip_colors(_flatten_text(data.get("description", ""))),
        "latency_ms": latency,
    }
