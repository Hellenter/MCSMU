"""Моды и плагины: список, включение/выключение (.disabled, как в Prism Launcher)."""
import json
import os
import re
import shutil
import zipfile

DISABLED = ".disabled"


def mod_dirs(server_dir):
    """Папки, где сервер ищет моды/плагины, которые есть на диске."""
    out = []
    for d in ("mods", "plugins"):
        if os.path.isdir(os.path.join(server_dir, d)):
            out.append(d)
    return out


def _toml_value(text, key):
    m = re.search(r'^\s*%s\s*=\s*["\']([^"\']*)["\']' % re.escape(key), text, re.M)
    return m.group(1) if m else None


def read_meta(path):
    """Название, версия и загрузчик из jar-файла мода. Ничего не бросает."""
    meta = {"name": None, "version": None, "loader": None, "desc": ""}
    try:
        with zipfile.ZipFile(path) as z:
            names = set(z.namelist())

            def read(n):
                return z.read(n).decode("utf-8", "replace")

            if "fabric.mod.json" in names:
                d = json.loads(read("fabric.mod.json"), strict=False)
                meta.update(name=d.get("name") or d.get("id"), version=d.get("version"), loader="Fabric",
                            desc=d.get("description") or "")
            elif "quilt.mod.json" in names:
                d = json.loads(read("quilt.mod.json"), strict=False).get("quilt_loader", {})
                md = d.get("metadata", {})
                meta.update(name=md.get("name") or d.get("id"), version=d.get("version"), loader="Quilt",
                            desc=md.get("description") or "")
            else:
                toml = next((n for n in ("META-INF/neoforge.mods.toml", "META-INF/mods.toml") if n in names), None)
                if toml:
                    t = read(toml)
                    meta.update(name=_toml_value(t, "displayName") or _toml_value(t, "modId"),
                                version=_toml_value(t, "version"),
                                loader="NeoForge" if "neoforge" in toml else "Forge",
                                desc=(_toml_value(t, "description") or "").strip())
                    if meta["version"] and "${" in meta["version"] and "META-INF/MANIFEST.MF" in names:
                        m = re.search(r"^Implementation-Version:\s*(.+)$", read("META-INF/MANIFEST.MF"), re.M)
                        meta["version"] = m.group(1).strip() if m else None
                elif "mcmod.info" in names:
                    d = json.loads(read("mcmod.info"), strict=False)
                    if isinstance(d, dict):
                        d = d.get("modList", [])
                    if d:
                        meta.update(name=d[0].get("name"), version=d[0].get("version"), loader="Forge",
                                    desc=d[0].get("description") or "")
                else:
                    pl = next((n for n in ("paper-plugin.yml", "plugin.yml") if n in names), None)
                    if pl:
                        t = read(pl)
                        get = lambda k: (re.search(r"^%s:\s*['\"]?([^'\"\n]+)" % k, t, re.M) or [None, None])[1]
                        meta.update(name=get("name"), version=get("version"), loader="Плагин",
                                    desc=(get("description") or "").strip())
    except Exception:
        pass
    if meta["version"] and "${" in str(meta["version"]):
        meta["version"] = None
    return meta


def list_mods(server_dir):
    mods = []
    for d in mod_dirs(server_dir):
        folder = os.path.join(server_dir, d)
        for f in os.listdir(folder):
            low = f.lower()
            if not (low.endswith(".jar") or low.endswith(".jar" + DISABLED)):
                continue
            path = os.path.join(folder, f)
            enabled = not low.endswith(DISABLED)
            meta = read_meta(path)
            base = f[:-len(DISABLED)] if not enabled else f
            mods.append({"path": path, "folder": d, "file": base, "enabled": enabled,
                         "name": meta["name"] or os.path.splitext(base)[0], "version": meta["version"] or "",
                         "loader": meta["loader"] or "", "desc": meta["desc"], "size": os.path.getsize(path)})
    mods.sort(key=lambda m: (m["folder"], m["name"].lower()))
    return mods


def set_enabled(path, enabled):
    """Переименовывает x.jar <-> x.jar.disabled. Возвращает новый путь."""
    is_disabled = path.lower().endswith(DISABLED)
    if enabled and is_disabled:
        new = path[:-len(DISABLED)]
    elif not enabled and not is_disabled:
        new = path + DISABLED
    else:
        return path
    if os.path.exists(new):
        raise RuntimeError("Файл уже существует: " + os.path.basename(new))
    os.replace(path, new)
    return new


def add_mods(server_dir, files, folder="mods"):
    dest = os.path.join(server_dir, folder)
    os.makedirs(dest, exist_ok=True)
    for f in files:
        shutil.copy2(f, os.path.join(dest, os.path.basename(f)))
