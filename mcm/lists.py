"""Операторы, белый список и баны (ops.json, whitelist.json, banned-*.json)."""
import datetime
import hashlib
import json
import os
import urllib.request
import uuid as uuidlib

LISTS = {
    "ops": {"file": "ops.json", "title": "Операторы", "key": "name",
            "columns": [("name", "Ник", 160), ("uuid", "UUID", 290), ("level", "Уровень", 70)],
            "add": "op {name}", "remove": "deop {name}"},
    "whitelist": {"file": "whitelist.json", "title": "Белый список", "key": "name",
                  "columns": [("name", "Ник", 160), ("uuid", "UUID", 290)],
                  "add": "whitelist add {name}", "remove": "whitelist remove {name}"},
    "banned-players": {"file": "banned-players.json", "title": "Баны игроков", "key": "name",
                       "columns": [("name", "Ник", 140), ("reason", "Причина", 220), ("created", "Когда", 160),
                                   ("expires", "До", 90), ("source", "Кто", 90)],
                       "add": "ban {name} {reason}", "remove": "pardon {name}"},
    "banned-ips": {"file": "banned-ips.json", "title": "Баны IP", "key": "ip",
                   "columns": [("ip", "IP", 140), ("reason", "Причина", 220), ("created", "Когда", 160),
                               ("expires", "До", 90), ("source", "Кто", 90)],
                   "add": "ban-ip {name} {reason}", "remove": "pardon-ip {name}"},
}


def path_for(server_dir, kind):
    return os.path.join(server_dir, LISTS[kind]["file"])


def load(server_dir, kind):
    p = path_for(server_dir, kind)
    if not os.path.exists(p):
        return []
    try:
        with open(p, "r", encoding="utf-8") as f:
            data = json.load(f)
        return data if isinstance(data, list) else []
    except (OSError, ValueError):
        return []


def save(server_dir, kind, entries):
    with open(path_for(server_dir, kind), "w", encoding="utf-8") as f:
        json.dump(entries, f, ensure_ascii=False, indent=2)


def offline_uuid(name):
    """UUID как в Java UUID.nameUUIDFromBytes("OfflinePlayer:" + name)."""
    h = bytearray(hashlib.md5(("OfflinePlayer:" + name).encode("utf-8")).digest())
    h[6] = (h[6] & 0x0F) | 0x30
    h[8] = (h[8] & 0x3F) | 0x80
    return str(uuidlib.UUID(bytes=bytes(h)))


def online_uuid(name, timeout=5):
    url = "https://api.mojang.com/users/profiles/minecraft/" + name
    req = urllib.request.Request(url, headers={"User-Agent": "MCServerManager"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        data = json.loads(r.read().decode("utf-8"))
    return str(uuidlib.UUID(data["id"])), data.get("name", name)


def resolve_player(name, online_mode):
    if online_mode:
        try:
            return online_uuid(name)
        except Exception as e:
            raise RuntimeError("Не удалось получить UUID игрока «%s» у Mojang: %s" % (name, e))
    return offline_uuid(name), name


def _now():
    return datetime.datetime.now().astimezone().strftime("%Y-%m-%d %H:%M:%S %z")


def make_entry(kind, name, online_mode=True, reason="", op_level=4):
    if kind == "banned-ips":
        return {"ip": name, "created": _now(), "source": "MCServerManager", "expires": "forever",
                "reason": reason or "Banned by an operator."}
    uid, real = resolve_player(name, online_mode)
    if kind == "ops":
        return {"uuid": uid, "name": real, "level": op_level, "bypassesPlayerLimit": False}
    if kind == "whitelist":
        return {"uuid": uid, "name": real}
    return {"uuid": uid, "name": real, "created": _now(), "source": "MCServerManager",
            "expires": "forever", "reason": reason or "Banned by an operator."}


def add_offline(server_dir, kind, name, online_mode=True, reason=""):
    """Добавление напрямую в файл (когда сервер выключен)."""
    entries = load(server_dir, kind)
    key = LISTS[kind]["key"]
    entries = [e for e in entries if str(e.get(key, "")).lower() != name.lower()]
    entries.append(make_entry(kind, name, online_mode, reason))
    save(server_dir, kind, entries)


def remove_offline(server_dir, kind, name):
    key = LISTS[kind]["key"]
    entries = [e for e in load(server_dir, kind) if str(e.get(key, "")).lower() != name.lower()]
    save(server_dir, kind, entries)


def command(kind, action, name, reason=""):
    tpl = LISTS[kind][action]
    return tpl.format(name=name, reason=reason).strip()
