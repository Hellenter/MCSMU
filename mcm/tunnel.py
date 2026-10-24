"""SSH-туннель (ssh -R ...) для открытия доступа к серверу извне."""
import collections
import subprocess
import sys
import threading
import time

from .server import split_args

NEW_CONSOLE = 0x00000010 if sys.platform == "win32" else 0
NO_WINDOW = 0x08000000 if sys.platform == "win32" else 0

KEEPALIVE = ["-o", "ServerAliveInterval=30", "-o", "ServerAliveCountMax=3", "-o", "ExitOnForwardFailure=yes"]


class Tunnel:
    """callback(kind, payload): kind = 'status' (bool) | 'line' (str)."""

    def __init__(self, settings, callback):
        self.settings = settings
        self.callback = callback
        self.proc = None
        self.wanted = False
        self.log = collections.deque(maxlen=500)
        self.connected_at = None
        self._last_spawn = 0

    @property
    def active(self):
        return self.proc is not None and self.proc.poll() is None

    def _emit(self, kind, payload):
        try:
            self.callback(kind, payload)
        except Exception:
            pass

    def _line(self, text):
        self.log.append(text)
        self._emit("line", text)

    def build_command(self):
        cmd = split_args(self.settings.get("command", ""))
        if not cmd:
            raise ValueError("Команда туннеля не задана (Настройки)")
        if cmd[0].lower().replace("\\", "/").rsplit("/", 1)[-1] in ("ssh", "ssh.exe"):
            opts = []
            if not any("ServerAliveInterval" in a for a in cmd):
                opts += KEEPALIVE
            # -N: только перенаправление порта, без удалённой оболочки
            if "-N" not in cmd and not self.settings.get("new_window"):
                opts.append("-N")
            cmd = [cmd[0]] + opts + cmd[1:]
        return cmd

    def start(self):
        if self.active:
            return
        self.wanted = True
        self._spawn()
        threading.Thread(target=self._watch, daemon=True).start()

    def _spawn(self):
        cmd = self.build_command()
        self._line("Запуск туннеля: " + " ".join(cmd))
        try:
            if self.settings.get("new_window"):
                # отдельное окно: можно ввести пароль / подтвердить ключ хоста
                self.proc = subprocess.Popen(cmd, creationflags=NEW_CONSOLE)
            else:
                self.proc = subprocess.Popen(
                    cmd, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                    creationflags=NO_WINDOW,
                )
                threading.Thread(target=self._reader, args=(self.proc,), daemon=True).start()
        except FileNotFoundError:
            self.wanted = False
            self.proc = None
            self._line("Не найден ssh. В Windows 10/11: Параметры → Приложения → Доп. компоненты → «Клиент OpenSSH».")
            self._emit("status", False)
            raise RuntimeError("Не найдена программа ssh")
        self.connected_at = time.time()
        self._last_spawn = self.connected_at
        self._emit("status", True)

    def _reader(self, proc):
        for raw in iter(proc.stdout.readline, b""):
            text = raw.decode("utf-8", "replace").rstrip()
            if text:
                self._line(text)

    def _watch(self):
        delay = 5
        while self.wanted:
            proc = self.proc
            if proc is None:
                return
            code = proc.wait()
            self.connected_at = None
            self._emit("status", False)
            if not self.wanted:
                self._line("Туннель закрыт")
                return
            short = time.time() - self._last_spawn < 30
            self._line("Туннель оборвался (код %s)" % code)
            if not self.settings.get("auto_reconnect"):
                self.wanted = False
                return
            delay = min(delay * 2, 60) if short else 5
            self._line("Переподключение через %d с…" % delay)
            for _ in range(delay * 10):
                if not self.wanted:
                    return
                time.sleep(0.1)
            try:
                self._spawn()
            except Exception as e:
                self._line("Ошибка: %s" % e)
                self.wanted = False
                return

    def stop(self):
        self.wanted = False
        if self.active:
            try:
                self.proc.terminate()
            except OSError:
                pass
