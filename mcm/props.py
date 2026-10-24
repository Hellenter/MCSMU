"""Чтение и запись server.properties с сохранением комментариев и порядка."""
import os
import re

# Подсказки и типы для часто используемых параметров
KNOWN = {
    "motd": ("Описание сервера в списке серверов", "str"),
    "server-port": ("Порт сервера", "int"),
    "server-ip": ("IP для привязки (обычно пусто)", "str"),
    "max-players": ("Максимум игроков", "int"),
    "online-mode": ("Проверка лицензии (false = пиратки могут заходить)", "bool"),
    "white-list": ("Белый список включён", "bool"),
    "enforce-whitelist": ("Кикать игроков не из белого списка при его перезагрузке", "bool"),
    "gamemode": ("Режим игры по умолчанию", ["survival", "creative", "adventure", "spectator"]),
    "force-gamemode": ("Принудительно ставить режим по умолчанию при входе", "bool"),
    "difficulty": ("Сложность", ["peaceful", "easy", "normal", "hard"]),
    "hardcore": ("Хардкор", "bool"),
    "pvp": ("PvP между игроками", "bool"),
    "level-name": ("Папка мира", "str"),
    "level-seed": ("Сид мира", "str"),
    "level-type": ("Тип мира", "str"),
    "generate-structures": ("Генерировать структуры", "bool"),
    "allow-nether": ("Разрешить Незер", "bool"),
    "allow-flight": ("Разрешить полёт (иначе кик за 'полёт')", "bool"),
    "spawn-monsters": ("Спавн монстров", "bool"),
    "spawn-animals": ("Спавн животных", "bool"),
    "spawn-npcs": ("Спавн жителей", "bool"),
    "spawn-protection": ("Радиус защиты спавна (0 = выкл.)", "int"),
    "view-distance": ("Дальность прорисовки (чанки)", "int"),
    "simulation-distance": ("Дальность симуляции (чанки)", "int"),
    "max-world-size": ("Максимальный радиус мира", "int"),
    "enable-command-block": ("Командные блоки", "bool"),
    "op-permission-level": ("Уровень прав операторов (1-4)", ["1", "2", "3", "4"]),
    "player-idle-timeout": ("Кик за AFK, минут (0 = выкл.)", "int"),
    "enable-rcon": ("Включить RCON", "bool"),
    "rcon.port": ("Порт RCON", "int"),
    "rcon.password": ("Пароль RCON", "str"),
    "enable-query": ("Включить Query", "bool"),
    "enable-status": ("Показывать сервер в списке (статус)", "bool"),
    "hide-online-players": ("Скрывать список игроков в статусе", "bool"),
    "resource-pack": ("Ссылка на ресурспак", "str"),
    "require-resource-pack": ("Обязательный ресурспак", "bool"),
    "network-compression-threshold": ("Порог сжатия пакетов", "int"),
    "max-tick-time": ("Watchdog: макс. время тика, мс (-1 = выкл.)", "int"),
    "enforce-secure-profile": ("Требовать подписанные сообщения чата", "bool"),
    "sync-chunk-writes": ("Синхронная запись чанков", "bool"),
}


def _unescape(s):
    out = []
    i = 0
    while i < len(s):
        c = s[i]
        if c == "\\" and i + 1 < len(s):
            n = s[i + 1]
            if n == "u" and re.fullmatch(r"[0-9a-fA-F]{4}", s[i + 2:i + 6] or ""):
                out.append(chr(int(s[i + 2:i + 6], 16)))
                i += 6
                continue
            out.append({"n": "\n", "t": "\t", "r": "\r", "f": "\f"}.get(n, n))
            i += 2
            continue
        out.append(c)
        i += 1
    return "".join(out)


def _escape(s):
    out = []
    for c in s:
        if c == "\\":
            out.append("\\\\")
        elif c == "\n":
            out.append("\\n")
        elif ord(c) > 126:
            out.append("\\u%04x" % ord(c) if ord(c) <= 0xFFFF else c)
        else:
            out.append(c)
    return "".join(out)


class ServerProperties:
    def __init__(self, path):
        self.path = path
        self.lines = []     # исходные строки
        self.values = {}    # key -> value (раскодированное)
        self.index = {}     # key -> номер строки
        self.load()

    @classmethod
    def for_server(cls, server_dir):
        return cls(os.path.join(server_dir, "server.properties"))

    @property
    def exists(self):
        return os.path.exists(self.path)

    def load(self):
        self.lines, self.values, self.index = [], {}, {}
        if not os.path.exists(self.path):
            return
        with open(self.path, "r", encoding="utf-8", errors="replace") as f:
            self.lines = f.read().splitlines()
        for n, line in enumerate(self.lines):
            st = line.strip()
            if not st or st[0] in "#!":
                continue
            m = re.match(r"^\s*((?:\\.|[^=:\s\\])+)\s*[=:]?\s*(.*)$", line)
            if not m:
                continue
            key = _unescape(m.group(1))
            self.values[key] = _unescape(m.group(2))
            self.index[key] = n

    def get(self, key, default=None):
        return self.values.get(key, default)

    def set(self, key, value):
        value = "" if value is None else str(value)
        line = "%s=%s" % (key, _escape(value))
        if key in self.index:
            self.lines[self.index[key]] = line
        else:
            self.index[key] = len(self.lines)
            self.lines.append(line)
        self.values[key] = value

    def save(self):
        with open(self.path, "w", encoding="utf-8", newline="\n") as f:
            f.write("\n".join(self.lines) + "\n")

    def items(self):
        return sorted(self.values.items())


def level_name(server_dir):
    p = ServerProperties.for_server(server_dir)
    return p.get("level-name") or "world"


def server_port(server_dir):
    p = ServerProperties.for_server(server_dir)
    try:
        return int(p.get("server-port") or 25565)
    except ValueError:
        return 25565
