"""Запуск/остановка процесса сервера, чтение консоли, учёт игроков."""
import collections
import glob
import os
import re
import shlex
import subprocess
import sys
import threading
import time

try:
    import psutil
except ImportError:  # приложение работает и без psutil, но без CPU/RAM
    psutil = None

STOPPED, STARTING, RUNNING, STOPPING = "stopped", "starting", "running", "stopping"
STATUS_RU = {STOPPED: "Остановлен", STARTING: "Запускается…", RUNNING: "Работает", STOPPING: "Останавливается…"}

NO_WINDOW = 0x08000000 if sys.platform == "win32" else 0

RE_JOIN = re.compile(r"(?:^|\]: )([A-Za-z0-9_.\-]{1,20}) joined the game")
RE_LEFT = re.compile(r"(?:^|\]: )([A-Za-z0-9_.\-]{1,20}) left the game")
RE_LIST = re.compile(r"There are (\d+) (?:of a max of |/)\s*(\d+) players online:?\s*(.*)")
RE_DONE = re.compile(r"Done \(\d+[.,]\d+s\)!")


def split_args(text):
    if not text.strip():
        return []
    if sys.platform == "win32":
        return _win_split(text)
    return shlex.split(text)


def _win_split(text):
    # простое разбиение с поддержкой кавычек, без экранирования обратных слэшей
    return [a.strip('"') for a in re.findall(r'"[^"]*"|\S+', text)]


def detect_launch(server_dir):
    """Пытается угадать, как запускать сервер в папке. Возвращает (mode, jar, custom_args)."""
    if not os.path.isdir(server_dir):
        return "jar", "", ""
    # Forge / NeoForge 1.17+: запуск через файл аргументов
    pattern = "win_args.txt" if sys.platform == "win32" else "unix_args.txt"
    for base in ("libraries/net/minecraftforge/forge", "libraries/net/neoforged/neoforge", "libraries/net/neoforged/forge"):
        hits = sorted(glob.glob(os.path.join(server_dir, base, "*", pattern)))
        if hits:
            rel = os.path.relpath(hits[-1], server_dir).replace("\\", "/")
            extra = "@user_jvm_args.txt " if os.path.exists(os.path.join(server_dir, "user_jvm_args.txt")) else ""
            return "args", "", extra + "@" + rel
    jars = list_jars(server_dir)
    pref = [j for j in jars if re.search(r"(paper|purpur|spigot|fabric|forge|server|minecraft|mohist|arclight|velocity)", j, re.I)
            and not re.search(r"installer", j, re.I)]
    return "jar", (pref or jars or [""])[0], ""


def list_jars(server_dir):
    try:
        return sorted(f for f in os.listdir(server_dir) if f.lower().endswith(".jar"))
    except OSError:
        return []


def build_command(srv):
    java = srv.get("java") or "java"
    cmd = [java]
    if srv.get("xms"):
        cmd.append("-Xms" + srv["xms"])
    if srv.get("xmx"):
        cmd.append("-Xmx" + srv["xmx"])
    cmd += ["-Dfile.encoding=UTF-8", "-Dstdout.encoding=UTF-8", "-Dstderr.encoding=UTF-8"]
    cmd += split_args(srv.get("jvm_args", ""))
    if srv.get("launch_mode") == "args":
        cmd += split_args(srv.get("custom_args", ""))
    else:
        if not srv.get("jar"):
            raise ValueError("Не выбран jar-файл сервера")
        cmd += ["-jar", srv["jar"]]
    cmd += split_args(srv.get("server_args", ""))
    return cmd


class ServerProcess:
    """Один сервер. События отправляются в callback(kind, server_id, payload) из фоновых потоков."""

    def __init__(self, srv, callback):
        self.srv = srv
        self.callback = callback
        self.proc = None
        self.status = STOPPED
        self.log = collections.deque(maxlen=5000)
        self.players = set()
        self.started_at = None
        self._listeners = []
        self._lock = threading.Lock()
        self._ps = None
        self.restart_after_stop = False

    @property
    def sid(self):
        return self.srv["id"]

    @property
    def running(self):
        return self.proc is not None and self.proc.poll() is None

    def _emit(self, kind, payload=None):
        try:
            self.callback(kind, self.sid, payload)
        except Exception:
            pass

    def _set_status(self, st):
        self.status = st
        self._emit("status", st)

    def info(self, text, level="info"):
        line = "[Менеджер] " + text
        self.log.append((line, level))
        self._emit("line", (line, level))

    # ---------- запуск ----------
    def start(self):
        if self.running:
            raise RuntimeError("Сервер уже запущен")
        path = self.srv["path"]
        if not os.path.isdir(path):
            raise RuntimeError("Папка сервера не найдена:\n" + path)
        cmd = build_command(self.srv)
        self.players.clear()
        self.info("Запуск: " + " ".join(cmd))
        try:
            self.proc = subprocess.Popen(
                cmd, cwd=path, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                bufsize=0, creationflags=NO_WINDOW,
            )
        except FileNotFoundError:
            raise RuntimeError("Не найдена Java: %s\nУкажите путь к java.exe в настройках сервера." % cmd[0])
        self.started_at = time.time()
        self._ps = None
        if psutil:
            try:
                self._ps = psutil.Process(self.proc.pid)
                self._ps.cpu_percent(None)
            except Exception:
                self._ps = None
        self._set_status(STARTING)
        threading.Thread(target=self._reader, daemon=True).start()

    def _reader(self):
        proc = self.proc
        stream = proc.stdout
        for raw in iter(stream.readline, b""):
            line = raw.decode("utf-8", "replace").rstrip("\r\n")
            if not line:
                continue
            level = "error" if ("ERROR" in line or "Exception" in line or "FATAL" in line) else \
                    "warn" if "WARN" in line else "normal"
            self.log.append((line, level))
            self._emit("line", (line, level))
            self._parse(line)
        code = proc.wait()
        self.proc = None
        self._ps = None
        self.started_at = None
        self.players.clear()
        if self.status == STOPPING:
            self.info("Сервер остановлен (код %s)" % code)
        else:
            self.info("Сервер завершился неожиданно (код %s)" % code, "error")
        self._set_status(STOPPED)
        self._emit("players", [])
        self._emit("exited", code)

    def _parse(self, line):
        if self.status == STARTING and RE_DONE.search(line):
            self._set_status(RUNNING)
        changed = False
        m = RE_JOIN.search(line)
        if m:
            self.players.add(m.group(1))
            changed = True
        m = RE_LEFT.search(line)
        if m:
            self.players.discard(m.group(1))
            changed = True
        m = RE_LIST.search(line)
        if m:
            names = [n.strip() for n in m.group(3).split(",") if n.strip()]
            if names or m.group(1) == "0":
                self.players = set(names)
                changed = True
        if changed:
            self._emit("players", sorted(self.players))
        with self._lock:
            for item in self._listeners:
                if not item["ev"].is_set() and item["re"].search(line):
                    item["line"] = line
                    item["ev"].set()

    # ---------- управление ----------
    def send(self, command):
        if not self.running:
            raise RuntimeError("Сервер не запущен")
        command = command.strip().lstrip("/")
        if not command:
            return
        self.log.append(("> " + command, "cmd"))
        self._emit("line", ("> " + command, "cmd"))
        try:
            self.proc.stdin.write((command + "\n").encode("utf-8"))
            self.proc.stdin.flush()
        except OSError as e:
            raise RuntimeError("Не удалось отправить команду: %s" % e)

    def listen(self, regex):
        """Подписка на строку консоли по regex. Подписываться нужно до отправки команды."""
        item = {"re": re.compile(regex), "ev": threading.Event(), "line": None}
        with self._lock:
            self._listeners.append(item)
        return item

    def await_item(self, item, timeout):
        """Ждёт подходящую строку; возвращает её или None по таймауту."""
        ok = item["ev"].wait(timeout)
        with self._lock:
            if item in self._listeners:
                self._listeners.remove(item)
        return item["line"] if ok else None

    def stop(self, timeout=60):
        """Корректная остановка в фоне: команда stop, по таймауту — kill."""
        if not self.running:
            return
        self._set_status(STOPPING)
        try:
            self.send("stop")
        except RuntimeError:
            pass

        def watchdog(proc=self.proc):
            try:
                proc.wait(timeout)
            except subprocess.TimeoutExpired:
                self.info("Сервер не остановился за %d с — принудительное завершение" % timeout, "error")
                self._kill_tree(proc)
        threading.Thread(target=watchdog, daemon=True).start()

    def kill(self):
        if self.running:
            self._set_status(STOPPING)
            self.info("Принудительное завершение процесса", "error")
            self._kill_tree(self.proc)

    @staticmethod
    def _kill_tree(proc):
        if psutil:
            try:
                for ch in psutil.Process(proc.pid).children(recursive=True):
                    ch.kill()
            except Exception:
                pass
        try:
            proc.kill()
        except OSError:
            pass

    # ---------- статистика ----------
    def resource_usage(self):
        """(cpu_percent от всей машины, rss_байт, threads) или None."""
        if not self._ps:
            return None
        try:
            with self._ps.oneshot():
                cpu = self._ps.cpu_percent(None) / (psutil.cpu_count() or 1)
                rss = self._ps.memory_info().rss
                threads = self._ps.num_threads()
            return cpu, rss, threads
        except Exception:
            return None

    def uptime(self):
        return time.time() - self.started_at if self.started_at else 0
