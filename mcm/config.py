"""Хранение настроек приложения и списка серверов."""
import json
import os
import sys
import uuid

APP_NAME = "MCServerManager"
DEFAULT_TUNNEL = "ssh -R 25565:localhost:25565 minecraft@ex1.ru"


def app_dir():
    if sys.platform == "win32":
        base = os.environ.get("APPDATA") or os.path.expanduser("~")
    else:
        base = os.environ.get("XDG_CONFIG_HOME") or os.path.expanduser("~/.config")
    path = os.path.join(base, APP_NAME)
    os.makedirs(path, exist_ok=True)
    return path


CONFIG_PATH = os.path.join(app_dir(), "config.json")


def new_server(name="Новый сервер", path=""):
    return {
        "id": uuid.uuid4().hex,
        "name": name,
        "path": path,
        "launch_mode": "jar",      # "jar" или "args"
        "jar": "",
        "custom_args": "",          # для Forge 1.17+: @user_jvm_args.txt @libraries/.../win_args.txt
        "java": "java",
        "xms": "1G",
        "xmx": "4G",
        "jvm_args": "",
        "server_args": "nogui",
        "backup_dir": "",           # пусто = <папка сервера>/backups
        "keep_backups": 10,         # 0 = хранить все
        "auto_backup_min": 0,       # 0 = выключено
        "backup_on_stop": False,
    }


DEFAULTS = {
    "servers": [],
    "last_server": None,
    "tunnel": {
        "command": DEFAULT_TUNNEL,
        "new_window": sys.platform == "win32",
        "auto_reconnect": True,
        "start_with_server": False,
        "stop_with_server": False,
    },
    "stop_timeout": 60,
    "geometry": "1200x760",
}


class Config:
    def __init__(self, path=CONFIG_PATH):
        self.path = path
        self.data = json.loads(json.dumps(DEFAULTS))
        self.load()

    def load(self):
        if not os.path.exists(self.path):
            return
        try:
            with open(self.path, "r", encoding="utf-8") as f:
                loaded = json.load(f)
        except (OSError, ValueError):
            return
        for k, v in loaded.items():
            if isinstance(v, dict) and isinstance(self.data.get(k), dict):
                self.data[k].update(v)
            else:
                self.data[k] = v
        # дополняем старые записи новыми полями
        for i, s in enumerate(self.data["servers"]):
            full = new_server()
            full.update(s)
            self.data["servers"][i] = full

    def save(self):
        tmp = self.path + ".tmp"
        with open(tmp, "w", encoding="utf-8") as f:
            json.dump(self.data, f, ensure_ascii=False, indent=2)
        os.replace(tmp, self.path)

    @property
    def servers(self):
        return self.data["servers"]

    @property
    def tunnel(self):
        return self.data["tunnel"]

    def get_server(self, sid):
        for s in self.servers:
            if s["id"] == sid:
                return s
        return None
