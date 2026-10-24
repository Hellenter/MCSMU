"""Графический интерфейс менеджера серверов (tkinter)."""
import collections
import datetime
import glob
import os
import queue
import re
import subprocess
import sys
import threading
import time
import tkinter as tk
from tkinter import filedialog, messagebox, simpledialog, ttk
from tkinter.scrolledtext import ScrolledText

from . import backup, lists, ping
from .config import DEFAULT_TUNNEL, Config, new_server
from .props import KNOWN, ServerProperties, level_name, server_port
from .server import (RUNNING, STARTING, STATUS_RU, STOPPED, STOPPING, ServerProcess, detect_launch,
                     list_jars, psutil, split_args)
from .tunnel import Tunnel

APP_TITLE = "MC Server Manager"
STATUS_COLORS = {STOPPED: "#888888", STARTING: "#d18f00", RUNNING: "#2e9d3a", STOPPING: "#d18f00"}
SAVED_RE = r"Saved the (game|world)|Saving is already|Usage:|Incorrect argument|Unknown or incomplete command|Unknown command"


# ---------------------------------------------------------------- утилиты
def open_path(path):
    if not os.path.exists(path):
        messagebox.showerror(APP_TITLE, "Путь не найден:\n" + path)
        return
    if sys.platform == "win32":
        os.startfile(path)
    elif sys.platform == "darwin":
        subprocess.Popen(["open", path])
    else:
        subprocess.Popen(["xdg-open", path])


def fmt_duration(sec):
    sec = int(sec)
    d, sec = divmod(sec, 86400)
    h, sec = divmod(sec, 3600)
    m, s = divmod(sec, 60)
    if d:
        return "%dд %02d:%02d:%02d" % (d, h, m, s)
    return "%02d:%02d:%02d" % (h, m, s)


def find_java():
    """Ищет установленные Java (разные версии MC требуют разные Java)."""
    found = []
    if sys.platform == "win32":
        roots = [os.environ.get("ProgramFiles", r"C:\Program Files"),
                 os.environ.get("ProgramFiles(x86)", r"C:\Program Files (x86)")]
        vendors = ["Java", "Eclipse Adoptium", "Eclipse Foundation", "Microsoft", "Zulu", "BellSoft",
                   "Amazon Corretto", "AdoptOpenJDK", "Semeru", "Liberica"]
        for r in roots:
            for v in vendors:
                found += glob.glob(os.path.join(r, v, "*", "bin", "java.exe"))
                found += glob.glob(os.path.join(r, v, "*", "*", "bin", "java.exe"))
        # Java из лаунчера Minecraft
        local = os.environ.get("LOCALAPPDATA", "")
        for base in (os.path.join(roots[1], "Minecraft Launcher", "runtime"),
                     os.path.join(local, "Packages", "Microsoft.4297127D64EC6_8wekyb3d8bbwe", "LocalCache", "Local", "runtime")):
            found += glob.glob(os.path.join(base, "*", "*", "*", "bin", "java.exe"))
    else:
        found += glob.glob("/usr/lib/jvm/*/bin/java")
        found += glob.glob("/Library/Java/JavaVirtualMachines/*/Contents/Home/bin/java")
    seen, out = set(), []
    for f in found:
        k = os.path.normcase(os.path.realpath(f))
        if k not in seen:
            seen.add(k)
            out.append(f)
    return ["java"] + sorted(out)


def tunnel_address(command):
    """Из 'ssh -R 25565:localhost:25565 user@host' получает 'host' (+порт, если не 25565)."""
    args = split_args(command or "")
    host = None
    port = "25565"
    for i, a in enumerate(args):
        if a == "-R" and i + 1 < len(args):
            parts = args[i + 1].split(":")
            if len(parts) >= 3:
                port = parts[-3]
        if "@" in a and not a.startswith("-"):
            host = a.split("@", 1)[1]
    if not host:
        return None
    return host if port == "25565" else "%s:%s" % (host, port)


# ---------------------------------------------------------------- диалог сервера
class ServerDialog(tk.Toplevel):
    def __init__(self, master, srv, is_new):
        super().__init__(master)
        self.title("Новый сервер" if is_new else "Настройки сервера")
        self.transient(master)
        self.resizable(True, False)
        self.result = None
        self.srv = dict(srv)
        self.vars = {}
        frm = ttk.Frame(self, padding=12)
        frm.pack(fill="both", expand=True)
        frm.columnconfigure(1, weight=1)
        row = 0

        def add(label, key, widget=None, hint=None):
            nonlocal row
            ttk.Label(frm, text=label).grid(row=row, column=0, sticky="w", pady=3, padx=(0, 8))
            if widget is None:
                var = tk.StringVar(value=str(self.srv.get(key, "")))
                widget = ttk.Entry(frm, textvariable=var, width=60)
                self.vars[key] = var
            widget.grid(row=row, column=1, sticky="ew", pady=3)
            if hint:
                row += 1
                ttk.Label(frm, text=hint, foreground="#666").grid(row=row, column=1, sticky="w")
            row += 1
            return widget

        add("Название:", "name")

        pf = ttk.Frame(frm)
        self.vars["path"] = tk.StringVar(value=self.srv.get("path", ""))
        ttk.Entry(pf, textvariable=self.vars["path"]).pack(side="left", fill="x", expand=True)
        ttk.Button(pf, text="Обзор…", command=self.pick_folder).pack(side="left", padx=(4, 0))
        add("Папка сервера:", "path", pf)

        mf = ttk.Frame(frm)
        self.vars["launch_mode"] = tk.StringVar(value=self.srv.get("launch_mode", "jar"))
        ttk.Radiobutton(mf, text="jar-файл", value="jar", variable=self.vars["launch_mode"],
                        command=self._mode_changed).pack(side="left")
        ttk.Radiobutton(mf, text="свои аргументы (Forge 1.17+ и т.п.)", value="args",
                        variable=self.vars["launch_mode"], command=self._mode_changed).pack(side="left", padx=10)
        ttk.Button(mf, text="Определить автоматически", command=self.autodetect).pack(side="right")
        add("Запуск:", "launch_mode", mf)

        jf = ttk.Frame(frm)
        self.vars["jar"] = tk.StringVar(value=self.srv.get("jar", ""))
        self.jar_box = ttk.Combobox(jf, textvariable=self.vars["jar"])
        self.jar_box.pack(side="left", fill="x", expand=True)
        ttk.Button(jf, text="Обзор…", command=self.pick_jar).pack(side="left", padx=(4, 0))
        add("Jar-файл:", "jar", jf)
        self.args_entry = add("Аргументы запуска:", "custom_args",
                              hint="Например: @user_jvm_args.txt @libraries/net/minecraftforge/forge/1.20.1-47.2.0/win_args.txt")

        jvf = ttk.Frame(frm)
        self.vars["java"] = tk.StringVar(value=self.srv.get("java", "java"))
        self.java_box = ttk.Combobox(jvf, textvariable=self.vars["java"])
        self.java_box.pack(side="left", fill="x", expand=True)
        ttk.Button(jvf, text="Найти", command=self.scan_java).pack(side="left", padx=(4, 0))
        ttk.Button(jvf, text="Обзор…", command=self.pick_java).pack(side="left", padx=(4, 0))
        add("Java:", "java", jvf, hint="MC 1.20.5+ → Java 21;  1.18–1.20.4 → Java 17;  1.17 → Java 16;  1.16 и старше → Java 8")

        rf = ttk.Frame(frm)
        self.vars["xms"] = tk.StringVar(value=self.srv.get("xms", "1G"))
        self.vars["xmx"] = tk.StringVar(value=self.srv.get("xmx", "4G"))
        ttk.Label(rf, text="мин.").pack(side="left")
        ttk.Combobox(rf, textvariable=self.vars["xms"], width=6, values=["512M", "1G", "2G", "4G"]).pack(side="left", padx=4)
        ttk.Label(rf, text="макс.").pack(side="left", padx=(10, 0))
        ttk.Combobox(rf, textvariable=self.vars["xmx"], width=6,
                     values=["1G", "2G", "3G", "4G", "6G", "8G", "10G", "12G", "16G"]).pack(side="left", padx=4)
        add("Память (RAM):", "xmx", rf)
        add("Доп. аргументы JVM:", "jvm_args")
        add("Аргументы сервера:", "server_args", hint="Обычно: nogui")

        ttk.Separator(frm).grid(row=row, column=0, columnspan=2, sticky="ew", pady=8)
        row += 1
        bf = ttk.Frame(frm)
        self.vars["backup_dir"] = tk.StringVar(value=self.srv.get("backup_dir", ""))
        ttk.Entry(bf, textvariable=self.vars["backup_dir"]).pack(side="left", fill="x", expand=True)
        ttk.Button(bf, text="Обзор…", command=self.pick_backup_dir).pack(side="left", padx=(4, 0))
        add("Папка бэкапов:", "backup_dir", bf, hint="Пусто = папка backups внутри папки сервера")
        of = ttk.Frame(frm)
        self.vars["keep_backups"] = tk.StringVar(value=str(self.srv.get("keep_backups", 10)))
        self.vars["auto_backup_min"] = tk.StringVar(value=str(self.srv.get("auto_backup_min", 0)))
        ttk.Label(of, text="хранить последних:").pack(side="left")
        ttk.Spinbox(of, from_=0, to=999, width=5, textvariable=self.vars["keep_backups"]).pack(side="left", padx=4)
        ttk.Label(of, text="(0 = все)   автобэкап каждые").pack(side="left", padx=(6, 0))
        ttk.Spinbox(of, from_=0, to=1440, width=5, textvariable=self.vars["auto_backup_min"]).pack(side="left", padx=4)
        ttk.Label(of, text="мин. (0 = выкл.)").pack(side="left")
        add("Бэкапы:", "keep_backups", of)
        self.backup_on_stop = tk.BooleanVar(value=bool(self.srv.get("backup_on_stop")))
        add("", "backup_on_stop", ttk.Checkbutton(frm, text="Делать бэкап после каждой остановки сервера",
                                                  variable=self.backup_on_stop))

        btns = ttk.Frame(frm)
        btns.grid(row=row, column=0, columnspan=2, sticky="e", pady=(12, 0))
        ttk.Button(btns, text="Сохранить", command=self.ok).pack(side="left", padx=4)
        ttk.Button(btns, text="Отмена", command=self.destroy).pack(side="left")
        self.bind("<Escape>", lambda e: self.destroy())

        self._refresh_jars()
        self.java_box["values"] = [self.vars["java"].get()]
        self._mode_changed()
        self.grab_set()
        self.geometry("+%d+%d" % (master.winfo_rootx() + 80, master.winfo_rooty() + 60))
        if is_new:
            self.after(50, self.pick_folder)

    def _mode_changed(self):
        jar = self.vars["launch_mode"].get() == "jar"
        self.jar_box.configure(state="normal" if jar else "disabled")
        self.args_entry.configure(state="disabled" if jar else "normal")

    def _refresh_jars(self):
        self.jar_box["values"] = list_jars(self.vars["path"].get())

    def pick_folder(self):
        d = filedialog.askdirectory(parent=self, title="Папка сервера", initialdir=self.vars["path"].get() or None)
        if d:
            d = os.path.normpath(d)
            self.vars["path"].set(d)
            if not self.vars["name"].get() or self.vars["name"].get() == "Новый сервер":
                self.vars["name"].set(os.path.basename(d))
            self._refresh_jars()
            if not self.vars["jar"].get() and not self.vars["custom_args"].get():
                self.autodetect()

    def autodetect(self):
        mode, jar, args = detect_launch(self.vars["path"].get())
        self.vars["launch_mode"].set(mode)
        if jar:
            self.vars["jar"].set(jar)
        if args:
            self.vars["custom_args"].set(args)
        self._refresh_jars()
        self._mode_changed()

    def pick_jar(self):
        f = filedialog.askopenfilename(parent=self, title="Jar сервера", initialdir=self.vars["path"].get() or None,
                                       filetypes=[("Java archive", "*.jar"), ("Все файлы", "*.*")])
        if f:
            base = self.vars["path"].get()
            try:
                rel = os.path.relpath(f, base) if base else f
            except ValueError:
                rel = f
            self.vars["jar"].set(rel if not rel.startswith("..") else f)

    def pick_java(self):
        f = filedialog.askopenfilename(parent=self, title="java.exe",
                                       filetypes=[("java", "java.exe java javaw.exe"), ("Все файлы", "*.*")])
        if f:
            self.vars["java"].set(os.path.normpath(f))

    def scan_java(self):
        found = find_java()
        self.java_box["values"] = found
        self.java_box.event_generate("<Down>")
        if len(found) == 1:
            messagebox.showinfo(APP_TITLE, "Отдельно установленные Java не найдены, будет использоваться «java» из PATH.",
                                parent=self)

    def pick_backup_dir(self):
        d = filedialog.askdirectory(parent=self, title="Папка для бэкапов")
        if d:
            self.vars["backup_dir"].set(os.path.normpath(d))

    def ok(self):
        data = {k: v.get().strip() for k, v in self.vars.items()}
        if not data["name"]:
            messagebox.showerror(APP_TITLE, "Укажите название", parent=self)
            return
        if not os.path.isdir(data["path"]):
            messagebox.showerror(APP_TITLE, "Папка сервера не существует", parent=self)
            return
        try:
            data["keep_backups"] = int(data["keep_backups"] or 0)
            data["auto_backup_min"] = int(data["auto_backup_min"] or 0)
        except ValueError:
            messagebox.showerror(APP_TITLE, "Число бэкапов и интервал должны быть числами", parent=self)
            return
        if data["launch_mode"] == "jar" and not data["jar"]:
            if not messagebox.askyesno(APP_TITLE, "Jar-файл не выбран. Сохранить всё равно?", parent=self):
                return
        data["backup_on_stop"] = self.backup_on_stop.get()
        self.srv.update(data)
        self.result = self.srv
        self.destroy()


# ---------------------------------------------------------------- график
class MiniChart(tk.Canvas):
    def __init__(self, master, title, color, unit, fmt="%.1f", **kw):
        super().__init__(master, height=110, background="#ffffff", highlightthickness=1,
                         highlightbackground="#cccccc", **kw)
        self.title, self.color, self.unit, self.fmt = title, color, unit, fmt
        self.bind("<Configure>", lambda e: self.draw())
        self.data = []
        self.max_hint = None

    def set(self, data, max_hint=None):
        self.data = list(data)
        self.max_hint = max_hint
        self.draw()

    def draw(self):
        self.delete("all")
        w, h = self.winfo_width(), self.winfo_height()
        pad_l, pad_t, pad_b = 6, 20, 6
        vals = [v for v in self.data if v is not None]
        top = max([self.max_hint or 0, max(vals) * 1.15 if vals else 0, 1])
        for i in range(1, 4):
            y = pad_t + (h - pad_t - pad_b) * i / 4
            self.create_line(pad_l, y, w - 4, y, fill="#eeeeee")
        cur = ((self.fmt + " %s") % (vals[-1], self.unit)).strip() if vals else "—"
        self.create_text(pad_l + 2, 4, anchor="nw", text="%s: %s" % (self.title, cur), fill="#333", font=("Segoe UI", 9, "bold"))
        self.create_text(w - 6, 4, anchor="ne", text="макс. шкалы %.0f %s" % (top, self.unit), fill="#999", font=("Segoe UI", 8))
        if len(vals) < 2:
            return
        n = len(self.data)
        step = (w - pad_l - 4) / max(n - 1, 1)
        pts = []
        for i, v in enumerate(self.data):
            if v is None:
                continue
            x = pad_l + i * step
            y = h - pad_b - (h - pad_t - pad_b) * (v / top)
            pts += [x, y]
        if len(pts) >= 4:
            self.create_line(*pts, fill=self.color, width=2, smooth=False)


# ---------------------------------------------------------------- главное окно
class App:
    def __init__(self, root):
        self.root = root
        self.cfg = Config()
        self.q = queue.Queue()
        self.procs = {}
        self.current = None           # id выбранного сервера
        self.props = None             # ServerProperties выбранного сервера
        self.props_dirty = False
        self.history = collections.defaultdict(lambda: collections.deque(maxlen=150))
        self.last_stats = {}
        self.last_auto_backup = {}
        self.busy = 0
        self.cmd_history, self.cmd_pos = [], 0
        self.tunnel = Tunnel(self.cfg.tunnel, lambda k, p: self.q.put(("tunnel", k, p)))

        root.title(APP_TITLE)
        root.geometry(self.cfg.data.get("geometry", "1200x760"))
        root.minsize(980, 620)
        self._style()
        self._build()
        self.refresh_server_list()
        last = self.cfg.data.get("last_server")
        if self.cfg.get_server(last):
            self.select_server(last)
        elif self.cfg.servers:
            self.select_server(self.cfg.servers[0]["id"])
        else:
            self.select_server(None)

        root.protocol("WM_DELETE_WINDOW", self.on_close)
        root.after(100, self._drain)
        root.after(30000, self._auto_backup_tick)
        threading.Thread(target=self._stats_loop, daemon=True).start()

    # ---------- оформление ----------
    def _style(self):
        st = ttk.Style()
        if sys.platform == "win32" and "vista" in st.theme_names():
            st.theme_use("vista")
        elif "clam" in st.theme_names():
            st.theme_use("clam")
        st.configure("Title.TLabel", font=("Segoe UI", 14, "bold"))
        st.configure("Big.TLabel", font=("Segoe UI", 11))
        st.configure("Value.TLabel", font=("Segoe UI", 10, "bold"))
        st.configure("Accent.TButton", font=("Segoe UI", 10, "bold"))

    def _build(self):
        sb = ttk.Frame(self.root, padding=(8, 2))
        sb.pack(fill="x", side="bottom")
        self.lbl_bar = ttk.Label(sb, text="Готово")
        self.lbl_bar.pack(side="left")
        self.progress = ttk.Progressbar(sb, mode="indeterminate", length=160)
        self.lbl_running = ttk.Label(sb, text="")
        self.lbl_running.pack(side="right")

        main = ttk.PanedWindow(self.root, orient="horizontal")
        main.pack(fill="both", expand=True)

        # --- левая панель: список серверов
        left = ttk.Frame(main, padding=(8, 8, 4, 8))
        main.add(left, weight=0)
        ttk.Label(left, text="Серверы", style="Title.TLabel").pack(anchor="w")
        self.srv_tree = ttk.Treeview(left, columns=("core",), show="tree headings", selectmode="browse", height=20)
        self.srv_tree.heading("#0", text="Название")
        self.srv_tree.heading("core", text="Ядро")
        self.srv_tree.column("#0", width=150)
        self.srv_tree.column("core", width=90)
        self.srv_tree.tag_configure("running", foreground=STATUS_COLORS[RUNNING])
        self.srv_tree.tag_configure("starting", foreground=STATUS_COLORS[STARTING])
        self.srv_tree.pack(fill="both", expand=True, pady=6)
        self.srv_tree.bind("<<TreeviewSelect>>", self._on_tree_select)
        self.srv_tree.bind("<Double-1>", lambda e: self.edit_server())
        bf = ttk.Frame(left)
        bf.pack(fill="x")
        for i, (text, cmd) in enumerate((("Добавить", self.add_server), ("Изменить", self.edit_server),
                                         ("Удалить", self.remove_server))):
            ttk.Button(bf, text=text, width=1, command=cmd).grid(row=0, column=i, sticky="ew", padx=1)
            bf.columnconfigure(i, weight=1, uniform="b")
        bf2 = ttk.Frame(left)
        bf2.pack(fill="x", pady=(2, 0))
        ttk.Button(bf2, text="↑", width=3, command=lambda: self.move_server(-1)).pack(side="left")
        ttk.Button(bf2, text="↓", width=3, command=lambda: self.move_server(1)).pack(side="left", padx=2)
        ttk.Button(bf2, text="Открыть папку", command=self.open_server_folder).pack(side="left", fill="x", expand=True)

        # --- правая часть
        right = ttk.Frame(main, padding=(4, 8, 8, 4))
        main.add(right, weight=1)
        self.root.after(80, lambda: main.sashpos(0, 280))

        top = ttk.Frame(right)
        top.pack(fill="x")
        self.lbl_name = ttk.Label(top, text="—", style="Title.TLabel")
        self.lbl_name.pack(side="left")
        self.status_dot = tk.Canvas(top, width=16, height=16, highlightthickness=0)
        self.status_dot.pack(side="left", padx=(12, 4))
        self.lbl_status = ttk.Label(top, text="", style="Big.TLabel")
        self.lbl_status.pack(side="left", padx=(0, 20))

        ctl = ttk.Frame(right)
        ctl.pack(fill="x", pady=(8, 6))
        self.btn_start = ttk.Button(ctl, text="▶  Запустить", style="Accent.TButton", command=self.start_server)
        self.btn_start.pack(side="left")
        self.btn_stop = ttk.Button(ctl, text="■  Остановить", command=self.stop_server)
        self.btn_stop.pack(side="left", padx=4)
        self.btn_restart = ttk.Button(ctl, text="⟳  Перезапустить", command=self.restart_server)
        self.btn_restart.pack(side="left")
        self.btn_kill = ttk.Button(ctl, text="✖  Убить процесс", command=self.kill_server)
        self.btn_kill.pack(side="left", padx=4)
        self.btn_backup_now = ttk.Button(ctl, text="💾  Бэкап сейчас", command=lambda: self.backup_now())
        self.btn_backup_now.pack(side="left", padx=(12, 0))

        tf = ttk.Frame(top)
        tf.pack(side="right")
        ttk.Label(tf, text="Доступ из интернета:").pack(side="left", padx=(0, 6))
        self.tunnel_dot = tk.Canvas(tf, width=14, height=14, highlightthickness=0)
        self.tunnel_dot.pack(side="left")
        self.lbl_tunnel = ttk.Label(tf, text="Закрыт", cursor="hand2")
        self.lbl_tunnel.pack(side="left", padx=(4, 8))
        self.lbl_tunnel.bind("<Button-1>", self.copy_address)
        self.btn_tunnel = ttk.Button(tf, text="Открыть доступ", width=16, command=self.toggle_tunnel)
        self.btn_tunnel.pack(side="left")

        self.nb = ttk.Notebook(right)
        self.nb.pack(fill="both", expand=True)
        self._build_console()
        self._build_stats()
        self._build_lists()
        self._build_props()
        self._build_worlds()
        self._build_settings()
        self._update_tunnel_ui()

    # ---------- вкладка: консоль
    def _build_console(self):
        f = ttk.Frame(self.nb, padding=4)
        self.nb.add(f, text="Консоль")
        self.console = ScrolledText(f, wrap="word", state="disabled", background="#1e1e1e", foreground="#d4d4d4",
                                    insertbackground="#d4d4d4", font=("Consolas", 10))
        self.console.pack(fill="both", expand=True)
        self.console.tag_configure("warn", foreground="#e5c07b")
        self.console.tag_configure("error", foreground="#f07178")
        self.console.tag_configure("info", foreground="#61afef")
        self.console.tag_configure("cmd", foreground="#98c379")
        bar = ttk.Frame(f)
        bar.pack(fill="x", pady=(4, 0))
        ttk.Label(bar, text="Команда:").pack(side="left")
        self.cmd_var = tk.StringVar()
        self.cmd_entry = ttk.Entry(bar, textvariable=self.cmd_var, font=("Consolas", 10))
        self.cmd_entry.pack(side="left", fill="x", expand=True, padx=4)
        self.cmd_entry.bind("<Return>", lambda e: self.send_command())
        self.cmd_entry.bind("<Up>", lambda e: self._hist(-1))
        self.cmd_entry.bind("<Down>", lambda e: self._hist(1))
        ttk.Button(bar, text="Отправить", command=self.send_command).pack(side="left")
        self.autoscroll = tk.BooleanVar(value=True)
        ttk.Checkbutton(bar, text="Автопрокрутка", variable=self.autoscroll).pack(side="left", padx=8)
        ttk.Button(bar, text="Очистить", command=self.clear_console).pack(side="left")
        quick = ttk.Frame(f)
        quick.pack(fill="x", pady=(4, 0))
        ttk.Label(quick, text="Быстрые команды:").pack(side="left")
        for label, cmd in (("Список игроков", "list"), ("Сохранить мир", "save-all"), ("День", "time set day"),
                           ("Ясно", "weather clear"), ("TPS (Paper)", "tps")):
            ttk.Button(quick, text=label, command=lambda c=cmd: self.send_command(c)).pack(side="left", padx=2)
        ttk.Button(quick, text="Объявление…", command=self.say_dialog).pack(side="left", padx=2)

    def _hist(self, d):
        if not self.cmd_history:
            return "break"
        self.cmd_pos = max(0, min(len(self.cmd_history), self.cmd_pos + d))
        self.cmd_var.set(self.cmd_history[self.cmd_pos] if self.cmd_pos < len(self.cmd_history) else "")
        self.cmd_entry.icursor("end")
        return "break"

    # ---------- вкладка: статистика
    def _build_stats(self):
        f = ttk.Frame(self.nb, padding=10)
        self.nb.add(f, text="Статистика и игроки")
        f.columnconfigure(0, weight=1)
        f.columnconfigure(1, weight=0, minsize=230)
        f.rowconfigure(1, weight=1)
        info = ttk.LabelFrame(f, text="Сервер", padding=10)
        info.grid(row=0, column=0, sticky="nsew", padx=(0, 8))
        self.stat_vars = {}
        fields = [("status", "Статус"), ("uptime", "Время работы"), ("online", "Игроки онлайн"),
                  ("version", "Версия"), ("motd", "MOTD"), ("latency", "Пинг (локально)"),
                  ("cpu", "CPU (процесс)"), ("ram", "RAM (процесс)"), ("threads", "Потоков"),
                  ("sysram", "RAM системы"), ("world", "Размер мира")]
        info.columnconfigure(1, weight=1)
        info.columnconfigure(3, weight=1)
        for i, (k, label) in enumerate(fields):
            r, c = i % 6, (i // 6) * 2
            ttk.Label(info, text=label + ":").grid(row=r, column=c, sticky="w", pady=2, padx=(0 if c == 0 else 12, 4))
            v = tk.StringVar(value="—")
            ttk.Label(info, textvariable=v, style="Value.TLabel").grid(row=r, column=c + 1, sticky="w")
            self.stat_vars[k] = v
        if psutil is None:
            ttk.Label(info, text="Модуль psutil не установлен — CPU/RAM недоступны (pip install psutil)",
                      foreground="#b00").grid(row=6, column=0, columnspan=4, sticky="w", pady=(6, 0))

        pl = ttk.LabelFrame(f, text="Игроки онлайн", padding=8)
        pl.grid(row=0, column=1, rowspan=2, sticky="nsew")
        self.players_list = tk.Listbox(pl, height=12, width=18, font=("Segoe UI", 11), activestyle="none")
        self.players_list.pack(fill="both", expand=True)
        self.players_list.bind("<Button-3>", self._player_menu)
        self.players_list.bind("<Double-1>", lambda e: self.player_action("tell"))
        pb = ttk.Frame(pl)
        pb.pack(fill="x", pady=(6, 0))
        acts = (("Кик", "kick"), ("Бан", "ban"), ("Сообщение", "tell"), ("OP", "op"), ("Снять OP", "deop"),
                ("Режим…", "mode"), ("В вайтлист", "wl"), ("Обновить", "refresh"))
        for i, (text, act) in enumerate(acts):
            ttk.Button(pb, text=text, command=lambda a=act: self.player_action(a)).grid(
                row=i // 3, column=i % 3, sticky="ew", padx=1, pady=1)
        for c in range(3):
            pb.columnconfigure(c, weight=1)
        ttk.Label(pl, text="Правый клик по игроку — меню действий", foreground="#666").pack(anchor="w", pady=(4, 0))

        charts = ttk.Frame(f)
        charts.grid(row=1, column=0, sticky="nsew", padx=(0, 8), pady=(8, 0))
        self.chart_cpu = MiniChart(charts, "CPU", "#d9534f", "%")
        self.chart_cpu.pack(fill="both", expand=True)
        self.chart_ram = MiniChart(charts, "RAM", "#0a58ca", "МБ")
        self.chart_ram.pack(fill="both", expand=True, pady=(6, 0))
        self.chart_players = MiniChart(charts, "Игроки", "#2e9d3a", "", fmt="%.0f")
        self.chart_players.pack(fill="both", expand=True, pady=(6, 0))
        ttk.Label(charts, text="Графики за последние 5 минут", foreground="#666").pack(anchor="w")

    # ---------- вкладка: доступ (ops/whitelist/баны)
    def _build_lists(self):
        f = ttk.Frame(self.nb, padding=6)
        self.nb.add(f, text="Операторы, вайтлист, баны")
        top = ttk.Frame(f)
        top.pack(fill="x", pady=(0, 6))
        self.wl_enabled = tk.BooleanVar()
        ttk.Checkbutton(top, text="Белый список включён", variable=self.wl_enabled,
                        command=self.toggle_whitelist).pack(side="left")
        self.lbl_lists_mode = ttk.Label(top, text="", foreground="#666")
        self.lbl_lists_mode.pack(side="left", padx=16)
        self.lists_nb = ttk.Notebook(f)
        self.lists_nb.pack(fill="both", expand=True)
        self.list_trees = {}
        for kind, meta in lists.LISTS.items():
            fr = ttk.Frame(self.lists_nb, padding=4)
            self.lists_nb.add(fr, text=meta["title"])
            cols = [c[0] for c in meta["columns"]]
            tree = ttk.Treeview(fr, columns=cols, show="headings", selectmode="extended")
            for key, title, width in meta["columns"]:
                tree.heading(key, text=title)
                tree.column(key, width=width, stretch=key in ("reason", "uuid"))
            ys = ttk.Scrollbar(fr, orient="vertical", command=tree.yview)
            tree.configure(yscrollcommand=ys.set)
            tree.pack(side="left", fill="both", expand=True)
            ys.pack(side="left", fill="y")
            bb = ttk.Frame(fr, padding=(6, 0))
            bb.pack(side="left", fill="y")
            ttk.Button(bb, text="Добавить…", command=lambda k=kind: self.list_add(k)).pack(fill="x")
            ttk.Button(bb, text="Удалить", command=lambda k=kind: self.list_remove(k)).pack(fill="x", pady=4)
            ttk.Button(bb, text="Обновить", command=self.reload_lists).pack(fill="x")
            ttk.Button(bb, text="Открыть файл", command=lambda k=kind: self._open_list_file(k)).pack(fill="x", pady=4)
            self.list_trees[kind] = tree

    # ---------- вкладка: server.properties
    def _build_props(self):
        f = ttk.Frame(self.nb, padding=6)
        self.nb.add(f, text="server.properties")
        top = ttk.Frame(f)
        top.pack(fill="x", pady=(0, 6))
        ttk.Label(top, text="Поиск:").pack(side="left")
        self.props_filter = tk.StringVar()
        self.props_filter.trace_add("write", lambda *a: self.fill_props())
        ttk.Entry(top, textvariable=self.props_filter, width=24).pack(side="left", padx=4)
        self.only_known = tk.BooleanVar(value=False)
        ttk.Checkbutton(top, text="Только основные", variable=self.only_known,
                        command=self.fill_props).pack(side="left", padx=8)
        self.lbl_props_state = ttk.Label(top, text="", foreground="#b36b00")
        self.lbl_props_state.pack(side="left", padx=10)
        ttk.Button(top, text="Открыть в блокноте", command=self._open_props_file).pack(side="right")
        ttk.Button(top, text="Перечитать", command=self.load_props).pack(side="right", padx=4)
        ttk.Button(top, text="Сохранить", style="Accent.TButton", width=14, command=self.save_props).pack(side="right")
        body = ttk.Frame(f)
        body.pack(fill="both", expand=True)
        self.props_tree = ttk.Treeview(body, columns=("key", "value", "desc"), show="headings")
        for key, title, w in (("key", "Параметр", 220), ("value", "Значение", 220), ("desc", "Описание", 380)):
            self.props_tree.heading(key, text=title)
            self.props_tree.column(key, width=w, stretch=key == "desc")
        ys = ttk.Scrollbar(body, orient="vertical", command=self.props_tree.yview)
        self.props_tree.configure(yscrollcommand=ys.set)
        self.props_tree.pack(side="left", fill="both", expand=True)
        ys.pack(side="left", fill="y")
        self.props_tree.bind("<Double-1>", lambda e: self.edit_prop())
        self.props_tree.bind("<Return>", lambda e: self.edit_prop())
        bb = ttk.Frame(f)
        bb.pack(fill="x", pady=(6, 0))
        ttk.Button(bb, text="Изменить…", command=self.edit_prop).pack(side="left")
        ttk.Button(bb, text="Добавить параметр…", command=self.add_prop).pack(side="left", padx=4)
        ttk.Label(bb, text="Применяется после перезапуска сервера.",
                  foreground="#666").pack(side="left", padx=10)

    # ---------- вкладка: миры и бэкапы
    def _build_worlds(self):
        f = ttk.Frame(self.nb, padding=6)
        self.nb.add(f, text="Мир и бэкапы")
        wi = ttk.LabelFrame(f, text="Текущий мир", padding=8)
        wi.pack(fill="x")
        self.lbl_world = ttk.Label(wi, text="—", style="Big.TLabel")
        self.lbl_world.pack(side="left")
        ttk.Button(wi, text="Открыть папку мира", command=self.open_world_folder).pack(side="right")
        ttk.Button(wi, text="Сменить мир (level-name)…", command=self.change_level_name).pack(side="right", padx=4)

        act = ttk.LabelFrame(f, text="Действия с миром", padding=8)
        act.pack(fill="x", pady=6)
        r1 = ttk.Frame(act)
        r1.pack(fill="x")
        ttk.Button(r1, text="💾 Создать бэкап", command=lambda: self.backup_now()).pack(side="left")
        ttk.Button(r1, text="Бэкап с пометкой…", command=self.backup_labeled).pack(side="left", padx=4)
        ttk.Button(r1, text="Экспорт мира в zip…", command=self.export_world).pack(side="left")
        r2 = ttk.Frame(act)
        r2.pack(fill="x", pady=(4, 0))
        ttk.Button(r2, text="Копировать мир в другой сервер…", command=self.copy_world_dialog).pack(side="left")
        ttk.Button(r2, text="Импорт мира из папки…", command=self.import_world).pack(side="left", padx=4)

        bl = ttk.LabelFrame(f, text="Бэкапы", padding=6)
        bl.pack(fill="both", expand=True)
        self.lbl_backup_dir = ttk.Label(bl, text="", foreground="#666")
        self.lbl_backup_dir.pack(anchor="w")
        body = ttk.Frame(bl)
        body.pack(fill="both", expand=True, pady=4)
        self.backup_tree = ttk.Treeview(body, columns=("date", "size"), show="tree headings", selectmode="extended")
        self.backup_tree.heading("#0", text="Файл")
        self.backup_tree.heading("date", text="Дата")
        self.backup_tree.heading("size", text="Размер")
        self.backup_tree.column("#0", width=420)
        self.backup_tree.column("date", width=160, stretch=False)
        self.backup_tree.column("size", width=100, stretch=False, anchor="e")
        ys = ttk.Scrollbar(body, orient="vertical", command=self.backup_tree.yview)
        self.backup_tree.configure(yscrollcommand=ys.set)
        self.backup_tree.pack(side="left", fill="both", expand=True)
        ys.pack(side="left", fill="y")
        bb = ttk.Frame(bl)
        bb.pack(fill="x")
        ttk.Button(bb, text="Восстановить выбранный", command=self.restore_selected).pack(side="left")
        ttk.Button(bb, text="Удалить", command=self.delete_backups).pack(side="left", padx=4)
        ttk.Button(bb, text="Обновить", command=self.refresh_backups).pack(side="left")
        ttk.Button(bb, text="Открыть папку бэкапов", command=self.open_backup_folder).pack(side="left", padx=4)
        ttk.Label(bl, text="Старые бэкапы удаляются автоматически (см. «Изменить» сервер), кроме помеченных словом keep.",
                  foreground="#666").pack(anchor="w", pady=(4, 0))

    # ---------- вкладка: настройки
    def _build_settings(self):
        f = ttk.Frame(self.nb, padding=10)
        self.nb.add(f, text="Настройки")
        tf = ttk.LabelFrame(f, text="SSH-туннель", padding=10)
        tf.pack(fill="x")
        tf.columnconfigure(1, weight=1)
        t = self.cfg.tunnel
        ttk.Label(tf, text="Команда:").grid(row=0, column=0, sticky="w")
        self.tun_cmd = tk.StringVar(value=t.get("command", DEFAULT_TUNNEL))
        ttk.Entry(tf, textvariable=self.tun_cmd, font=("Consolas", 10)).grid(row=0, column=1, sticky="ew", padx=6)
        ttk.Button(tf, text="По умолчанию", command=lambda: self.tun_cmd.set(DEFAULT_TUNNEL)).grid(row=0, column=2)
        self.tun_vars = {}
        opts = [("new_window", "Открывать туннель в отдельном окне (нужно, если ssh спрашивает пароль)"),
                ("auto_reconnect", "Переподключаться автоматически при обрыве"),
                ("start_with_server", "Открывать доступ автоматически при запуске сервера"),
                ("stop_with_server", "Закрывать доступ при остановке сервера")]
        for i, (k, label) in enumerate(opts, start=1):
            v = tk.BooleanVar(value=bool(t.get(k)))
            ttk.Checkbutton(tf, text=label, variable=v).grid(row=i, column=0, columnspan=3, sticky="w", pady=1)
            self.tun_vars[k] = v
        ttk.Label(tf, foreground="#666", justify="left", text=(
            "Чтобы туннель открывался без ввода пароля, один раз настройте вход по ключу:\n"
            "  ssh-keygen   (Enter на все вопросы),  затем добавьте содержимое %USERPROFILE%\\.ssh\\id_ed25519.pub\n"
            "  в файл ~/.ssh/authorized_keys пользователя minecraft на сервере ex1.ru."
        )).grid(row=len(opts) + 1, column=0, columnspan=3, sticky="w", pady=(6, 0))

        gf = ttk.LabelFrame(f, text="Общие", padding=10)
        gf.pack(fill="x", pady=8)
        ttk.Label(gf, text="Ждать корректной остановки сервера, сек.:").pack(side="left")
        self.stop_timeout = tk.StringVar(value=str(self.cfg.data.get("stop_timeout", 60)))
        ttk.Spinbox(gf, from_=10, to=600, width=6, textvariable=self.stop_timeout).pack(side="left", padx=6)
        ttk.Label(gf, text="(потом процесс завершается принудительно)", foreground="#666").pack(side="left")

        ttk.Button(f, text="Сохранить настройки", style="Accent.TButton", width=22, command=self.save_settings).pack(anchor="w")
        lf = ttk.LabelFrame(f, text="Журнал туннеля", padding=6)
        lf.pack(fill="both", expand=True, pady=(8, 0))
        self.tunnel_log = ScrolledText(lf, height=8, state="disabled", font=("Consolas", 9))
        self.tunnel_log.pack(fill="both", expand=True)
        ttk.Label(f, text="Настройки хранятся в: " + self.cfg.path, foreground="#888").pack(anchor="w", pady=(4, 0))

    # ================================================================ серверы
    def srv(self):
        return self.cfg.get_server(self.current) if self.current else None

    def proc(self, sid=None):
        sid = sid or self.current
        if not sid:
            return None
        if sid not in self.procs:
            s = self.cfg.get_server(sid)
            if not s:
                return None
            self.procs[sid] = ServerProcess(s, lambda k, i, p: self.q.put(("srv", k, i, p)))
        return self.procs[sid]

    def running_proc(self):
        for p in self.procs.values():
            if p.running:
                return p
        return None

    def refresh_server_list(self):
        self.srv_tree.delete(*self.srv_tree.get_children())
        for s in self.cfg.servers:
            p = self.procs.get(s["id"])
            st = p.status if p else STOPPED
            mark = {RUNNING: "● ", STARTING: "◐ ", STOPPING: "◐ "}.get(st, "○ ")
            core = os.path.basename(s.get("jar") or "") if s.get("launch_mode") == "jar" else "Forge/свои арг."
            tags = ("running",) if st == RUNNING else ("starting",) if st in (STARTING, STOPPING) else ()
            self.srv_tree.insert("", "end", iid=s["id"], text=mark + s["name"], values=(core,), tags=tags)
        if self.current and self.srv_tree.exists(self.current):
            self.srv_tree.selection_set(self.current)

    def _on_tree_select(self, _e):
        sel = self.srv_tree.selection()
        if sel and sel[0] != self.current:
            self.select_server(sel[0])

    def select_server(self, sid):
        if self.props_dirty and self.current and sid != self.current:
            if messagebox.askyesno(APP_TITLE, "Сохранить изменения server.properties?"):
                self.save_props()
        self.current = sid
        self.props_dirty = False
        self.cfg.data["last_server"] = sid
        if sid and self.srv_tree.exists(sid):
            self.srv_tree.selection_set(sid)
            self.srv_tree.see(sid)
        s = self.srv()
        self.lbl_name.configure(text=s["name"] if s else "Добавьте сервер кнопкой «Добавить»")
        self.reload_console()
        self.load_props()
        self.reload_lists()
        self.refresh_backups()
        self.update_world_info()
        p = self.procs.get(sid) if sid else None
        self.players_list.delete(0, "end")
        for n in sorted(p.players) if p else []:
            self.players_list.insert("end", n)
        for k in self.stat_vars:
            self.stat_vars[k].set("—")
        self._update_charts()
        self.update_controls()

    def add_server(self):
        dlg = ServerDialog(self.root, new_server(), True)
        self.root.wait_window(dlg)
        if dlg.result:
            self.cfg.servers.append(dlg.result)
            self.cfg.save()
            self.refresh_server_list()
            self.select_server(dlg.result["id"])
            if not os.path.exists(os.path.join(dlg.result["path"], "server.properties")):
                self.set_bar("Сервер добавлен. server.properties появится после первого запуска.")

    def edit_server(self):
        s = self.srv()
        if not s:
            return
        dlg = ServerDialog(self.root, s, False)
        self.root.wait_window(dlg)
        if dlg.result:
            s.update(dlg.result)
            self.cfg.save()
            self.refresh_server_list()
            self.select_server(s["id"])

    def remove_server(self):
        s = self.srv()
        if not s:
            return
        p = self.procs.get(s["id"])
        if p and p.running:
            messagebox.showwarning(APP_TITLE, "Сначала остановите сервер.")
            return
        if not messagebox.askyesno(APP_TITLE, "Убрать «%s» из списка?\n\nФайлы сервера на диске НЕ удаляются." % s["name"]):
            return
        self.cfg.servers.remove(s)
        self.procs.pop(s["id"], None)
        self.cfg.save()
        self.refresh_server_list()
        self.select_server(self.cfg.servers[0]["id"] if self.cfg.servers else None)

    def move_server(self, d):
        s = self.srv()
        if not s:
            return
        lst = self.cfg.servers
        i = lst.index(s)
        j = i + d
        if 0 <= j < len(lst):
            lst[i], lst[j] = lst[j], lst[i]
            self.cfg.save()
            self.refresh_server_list()

    def open_server_folder(self):
        s = self.srv()
        if s:
            open_path(s["path"])

    # ---------- старт/стоп
    def _check_eula(self, s):
        p = os.path.join(s["path"], "eula.txt")
        ok = False
        if os.path.exists(p):
            with open(p, encoding="utf-8", errors="replace") as f:
                ok = re.search(r"^\s*eula\s*=\s*true", f.read(), re.M | re.I) is not None
        if ok:
            return True
        if not messagebox.askyesno(APP_TITLE, "Для запуска нужно принять лицензионное соглашение Minecraft (EULA):\n"
                                   "https://aka.ms/MinecraftEULA\n\nПринять и записать eula=true?"):
            return False
        with open(p, "w", encoding="utf-8") as f:
            f.write("# Accepted via MC Server Manager %s\neula=true\n" % datetime.datetime.now().isoformat(timespec="seconds"))
        return True

    def start_server(self):
        s = self.srv()
        if not s:
            return
        other = self.running_proc()
        if other and other.sid != s["id"]:
            if not messagebox.askyesno(APP_TITLE, "Сейчас запущен «%s».\nОстановить его и запустить «%s»?"
                                       % (other.srv["name"], s["name"])):
                return
            other.restart_after_stop = False
            self._start_after = s["id"]
            other.stop(self._stop_timeout())
            self.set_bar("Останавливаю «%s»…" % other.srv["name"])
            return
        if self.props_dirty:
            if messagebox.askyesno(APP_TITLE, "Есть несохранённые изменения server.properties. Сохранить перед запуском?"):
                self.save_props()
        if not self._check_eula(s):
            return
        try:
            self.proc(s["id"]).start()
        except Exception as e:
            messagebox.showerror(APP_TITLE, str(e))
            return
        self.last_auto_backup[s["id"]] = time.time()
        self.nb.select(0)
        if self.cfg.tunnel.get("start_with_server") and not self.tunnel.active:
            self.toggle_tunnel()
        self.refresh_server_list()
        self.update_controls()

    def _stop_timeout(self):
        try:
            return max(10, int(self.cfg.data.get("stop_timeout", 60)))
        except ValueError:
            return 60

    def stop_server(self):
        p = self.proc()
        if p and p.running:
            p.restart_after_stop = False
            p.stop(self._stop_timeout())
            self.update_controls()

    def restart_server(self):
        p = self.proc()
        if p and p.running:
            p.restart_after_stop = True
            p.stop(self._stop_timeout())
            self.update_controls()
        else:
            self.start_server()

    def kill_server(self):
        p = self.proc()
        if p and p.running and messagebox.askyesno(APP_TITLE, "Завершить процесс без сохранения мира?\n"
                                                   "Используйте, только если сервер завис."):
            p.restart_after_stop = False
            p.kill()

    def _on_exited(self, sid):
        p = self.procs.get(sid)
        s = self.cfg.get_server(sid)
        if p and p.restart_after_stop:
            p.restart_after_stop = False
            if self.current != sid:
                self.select_server(sid)
            self.root.after(1500, self.start_server)
        elif getattr(self, "_start_after", None):
            nxt, self._start_after = self._start_after, None
            self.select_server(nxt)
            self.root.after(1000, self.start_server)
        elif self.cfg.tunnel.get("stop_with_server") and self.tunnel.active:
            self.tunnel.stop()
        if s and s.get("backup_on_stop"):
            self.backup_now(s, label="stop")
        if sid == self.current:
            self.update_world_info()

    def update_controls(self):
        s = self.srv()
        p = self.procs.get(self.current) if self.current else None
        st = p.status if p else STOPPED
        running = p is not None and p.running
        self.lbl_status.configure(text=STATUS_RU[st] if s else "")
        self.status_dot.delete("all")
        if s:
            self.status_dot.create_oval(2, 2, 14, 14, fill=STATUS_COLORS[st], outline="")
        state = lambda on: "normal" if on else "disabled"
        self.btn_start.configure(state=state(s is not None and not running))
        self.btn_stop.configure(state=state(running and st != STOPPING))
        self.btn_restart.configure(state=state(s is not None and st != STOPPING))
        self.btn_kill.configure(state=state(running))
        self.btn_backup_now.configure(state=state(s is not None))
        rp = self.running_proc()
        self.lbl_running.configure(text=("Запущен: %s (%s)" % (rp.srv["name"], STATUS_RU[rp.status])) if rp else "Ни один сервер не запущен")
        mode = "сервер запущен — изменения отправляются командами" if running and st == RUNNING else \
            "сервер выключен — изменения пишутся прямо в файлы"
        self.lbl_lists_mode.configure(text=mode)

    # ---------- консоль
    def reload_console(self):
        self.console.configure(state="normal")
        self.console.delete("1.0", "end")
        p = self.procs.get(self.current) if self.current else None
        if p:
            for line, level in p.log:
                self.console.insert("end", line + "\n", level)
        self.console.configure(state="disabled")
        self.console.see("end")

    def append_console(self, line, level):
        c = self.console
        c.configure(state="normal")
        c.insert("end", line + "\n", level)
        if int(c.index("end-1c").split(".")[0]) > 6000:
            c.delete("1.0", "1000.0")
        c.configure(state="disabled")
        if self.autoscroll.get():
            c.see("end")

    def clear_console(self):
        p = self.procs.get(self.current)
        if p:
            p.log.clear()
        self.reload_console()

    def send_command(self, cmd=None):
        text = cmd if cmd is not None else self.cmd_var.get()
        if not text.strip():
            return
        p = self.proc()
        if not p or not p.running:
            self.set_bar("Сервер не запущен")
            return
        try:
            p.send(text)
        except RuntimeError as e:
            messagebox.showerror(APP_TITLE, str(e))
            return
        if cmd is None:
            if not self.cmd_history or self.cmd_history[-1] != text:
                self.cmd_history.append(text)
            self.cmd_pos = len(self.cmd_history)
            self.cmd_var.set("")

    def say_dialog(self):
        text = simpledialog.askstring(APP_TITLE, "Сообщение всем игрокам:", parent=self.root)
        if text:
            self.send_command("say " + text)

    # ---------- игроки
    def _selected_player(self):
        sel = self.players_list.curselection()
        return self.players_list.get(sel[0]) if sel else None

    def _player_menu(self, e):
        idx = self.players_list.nearest(e.y)
        if idx < 0:
            return
        self.players_list.selection_clear(0, "end")
        self.players_list.selection_set(idx)
        m = tk.Menu(self.root, tearoff=0)
        for label, act in (("Написать сообщение…", "tell"), ("Кикнуть…", "kick"), ("Забанить…", "ban"),
                           ("Сделать оператором", "op"), ("Снять оператора", "deop"), ("Добавить в белый список", "wl")):
            m.add_command(label=label, command=lambda a=act: self.player_action(a))
        gm = tk.Menu(m, tearoff=0)
        for mode, label in (("survival", "Выживание"), ("creative", "Творческий"), ("adventure", "Приключение"),
                            ("spectator", "Наблюдатель")):
            gm.add_command(label=label, command=lambda md=mode: self._player_cmd("gamemode %s {p}" % md))
        m.add_cascade(label="Режим игры", menu=gm)
        m.tk_popup(e.x_root, e.y_root)

    def _player_cmd(self, tpl):
        name = self._selected_player()
        if name:
            self.send_command(tpl.format(p=name))

    def player_action(self, act):
        if act == "refresh":
            self.send_command("list")
            return
        name = self._selected_player()
        if not name:
            self.set_bar("Выберите игрока в списке")
            return
        if act == "tell":
            t = simpledialog.askstring(APP_TITLE, "Сообщение для %s:" % name, parent=self.root)
            if t:
                self.send_command("tell %s %s" % (name, t))
        elif act in ("kick", "ban"):
            r = simpledialog.askstring(APP_TITLE, "Причина (%s %s):" % ("кик" if act == "kick" else "бан", name),
                                       parent=self.root)
            if r is not None:
                self.send_command(("%s %s %s" % (act, name, r)).strip())
                if act == "ban":
                    self.root.after(1500, self.reload_lists)
        elif act in ("op", "deop"):
            self.send_command("%s %s" % (act, name))
            self.root.after(1500, self.reload_lists)
        elif act == "wl":
            self.send_command("whitelist add " + name)
            self.root.after(1500, self.reload_lists)
        elif act == "mode":
            mode = simpledialog.askstring(APP_TITLE, "Режим для %s (survival / creative / adventure / spectator):" % name,
                                          parent=self.root, initialvalue="survival")
            if mode:
                self.send_command("gamemode %s %s" % (mode.strip(), name))

    # ---------- статистика (фоновый поток)
    def _stats_loop(self):
        while True:
            time.sleep(2)
            try:
                for sid, p in list(self.procs.items()):
                    if not p.running:
                        continue
                    data = {"uptime": p.uptime(), "usage": p.resource_usage(), "ping": None}
                    if p.status == RUNNING:
                        try:
                            data["ping"] = ping.ping("127.0.0.1", server_port(p.srv["path"]), timeout=1.5)
                        except Exception:
                            data["ping"] = None
                    if psutil:
                        vm = psutil.virtual_memory()
                        data["sys"] = (vm.used, vm.total)
                    self.q.put(("stats", sid, data))
            except Exception:
                pass

    def _apply_stats(self, sid, d):
        p = self.procs.get(sid)
        if not p:
            return
        h = self.history[sid]
        usage = d.get("usage")
        pg = d.get("ping")
        h.append((usage[0] if usage else None, usage[1] / 1048576 if usage else None,
                  pg["online"] if pg else len(p.players)))
        if pg and pg.get("players") and not p.players:
            p.players = set(pg["players"])
            if sid == self.current:
                self._fill_players(p)
        if sid != self.current:
            return
        v = self.stat_vars
        v["status"].set(STATUS_RU[p.status])
        v["uptime"].set(fmt_duration(d["uptime"]))
        if usage:
            v["cpu"].set("%.1f %%" % usage[0])
            v["ram"].set(backup.human_size(usage[1]))
            v["threads"].set(str(usage[2]))
        if d.get("sys"):
            v["sysram"].set("%s / %s" % (backup.human_size(d["sys"][0]), backup.human_size(d["sys"][1])))
        if pg:
            v["online"].set("%d / %d" % (pg["online"], pg["max"]))
            v["version"].set(pg["version"])
            motd = pg["motd"].replace("\n", " ")
            v["motd"].set(motd[:60] + ("…" if len(motd) > 60 else ""))
            v["latency"].set("%s мс" % pg["latency_ms"] if pg["latency_ms"] is not None else "—")
        else:
            v["online"].set("%d" % len(p.players))
        self._update_charts()

    def _update_charts(self):
        h = list(self.history.get(self.current, [])) if self.current else []
        self.chart_cpu.set([x[0] for x in h], 10)
        s = self.srv()
        xmx = None
        if s:
            m = re.match(r"(\d+)([GgMm])", s.get("xmx") or "")
            if m:
                xmx = int(m.group(1)) * (1024 if m.group(2) in "Gg" else 1)
        self.chart_ram.set([x[1] for x in h], xmx)
        self.chart_players.set([x[2] for x in h], 5)

    def _fill_players(self, p):
        sel = self._selected_player()
        self.players_list.delete(0, "end")
        for i, n in enumerate(sorted(p.players)):
            self.players_list.insert("end", n)
            if n == sel:
                self.players_list.selection_set(i)

    # ================================================================ списки доступа
    def reload_lists(self):
        s = self.srv()
        for kind, tree in self.list_trees.items():
            tree.delete(*tree.get_children())
            if not s:
                continue
            cols = [c[0] for c in lists.LISTS[kind]["columns"]]
            for e in lists.load(s["path"], kind):
                tree.insert("", "end", values=[e.get(c, "") for c in cols])
        if s:
            pr = ServerProperties.for_server(s["path"])
            self.wl_enabled.set((pr.get("white-list") or "false").lower() == "true")

    def _live(self):
        p = self.procs.get(self.current) if self.current else None
        return p if p and p.running and p.status == RUNNING else None

    def _online_mode(self, s):
        return (ServerProperties.for_server(s["path"]).get("online-mode") or "true").lower() == "true"

    def list_add(self, kind):
        s = self.srv()
        if not s:
            return
        is_ip = kind == "banned-ips"
        name = simpledialog.askstring(APP_TITLE, "IP-адрес:" if is_ip else "Ник игрока:", parent=self.root)
        if not name or not name.strip():
            return
        name = name.strip()
        reason = ""
        if kind.startswith("banned"):
            reason = simpledialog.askstring(APP_TITLE, "Причина бана (можно оставить пустой):", parent=self.root) or ""
        live = self._live()
        if live:
            self.send_command(lists.command(kind, "add", name, reason))
            self.root.after(1500, self.reload_lists)
            return
        online = self._online_mode(s)
        self.run_bg("Добавляю %s…" % name, lambda: lists.add_offline(s["path"], kind, name, online, reason),
                    lambda _r: self.reload_lists())

    def list_remove(self, kind):
        s = self.srv()
        tree = self.list_trees[kind]
        sel = tree.selection()
        if not s or not sel:
            return
        names = [str(tree.item(i, "values")[0]) for i in sel]
        if not messagebox.askyesno(APP_TITLE, "Удалить из списка «%s»:\n%s" % (lists.LISTS[kind]["title"], ", ".join(names))):
            return
        live = self._live()
        for n in names:
            if live:
                self.send_command(lists.command(kind, "remove", n))
            else:
                lists.remove_offline(s["path"], kind, n)
        if live:
            self.root.after(1500, self.reload_lists)
        else:
            self.reload_lists()

    def toggle_whitelist(self):
        s = self.srv()
        if not s:
            return
        on = self.wl_enabled.get()
        live = self._live()
        if live:
            self.send_command("whitelist " + ("on" if on else "off"))
        pr = ServerProperties.for_server(s["path"])
        if pr.exists:
            pr.set("white-list", "true" if on else "false")
            if not live:
                pr.save()
            self.load_props()
        self.set_bar("Белый список " + ("включён" if on else "выключен"))

    def _open_list_file(self, kind):
        s = self.srv()
        if s:
            p = lists.path_for(s["path"], kind)
            if not os.path.exists(p):
                lists.save(s["path"], kind, [])
            open_path(p)

    # ================================================================ server.properties
    def load_props(self):
        s = self.srv()
        self.props = ServerProperties.for_server(s["path"]) if s else None
        self.props_dirty = False
        self.fill_props()

    def fill_props(self):
        tree = self.props_tree
        tree.delete(*tree.get_children())
        if not self.props:
            self.lbl_props_state.configure(text="")
            return
        if not self.props.exists:
            self.lbl_props_state.configure(text="Файл server.properties ещё не создан — запустите сервер один раз.")
            return
        flt = self.props_filter.get().strip().lower()
        for k, v in self.props.items():
            desc = KNOWN.get(k, ("", None))[0]
            if self.only_known.get() and k not in KNOWN:
                continue
            if flt and flt not in k.lower() and flt not in desc.lower() and flt not in v.lower():
                continue
            tree.insert("", "end", iid=k, values=(k, v, desc))
        self.lbl_props_state.configure(text="● Есть несохранённые изменения" if self.props_dirty else "")

    def edit_prop(self, key=None):
        if not self.props or not self.props.exists:
            return
        if key is None:
            sel = self.props_tree.selection()
            if not sel:
                return
            key = sel[0]
        cur = self.props.get(key, "")
        desc, kind = KNOWN.get(key, ("", "str"))
        dlg = tk.Toplevel(self.root)
        dlg.title(key)
        dlg.transient(self.root)
        dlg.resizable(False, False)
        fr = ttk.Frame(dlg, padding=12)
        fr.pack(fill="both")
        ttk.Label(fr, text=key, style="Value.TLabel").pack(anchor="w")
        if desc:
            ttk.Label(fr, text=desc, foreground="#555").pack(anchor="w", pady=(0, 6))
        var = tk.StringVar(value=cur)
        if kind == "bool":
            w = ttk.Combobox(fr, textvariable=var, values=["true", "false"], state="readonly", width=40)
        elif isinstance(kind, list):
            w = ttk.Combobox(fr, textvariable=var, values=kind, width=40)
        else:
            w = ttk.Entry(fr, textvariable=var, width=50)
        w.pack(fill="x")
        w.focus_set()

        def ok(_e=None):
            val = var.get()
            if kind == "int" and val.strip() and not re.fullmatch(r"-?\d+", val.strip()):
                messagebox.showerror(APP_TITLE, "Нужно целое число", parent=dlg)
                return
            if val != cur:
                self.props.set(key, val)
                self.props_dirty = True
                self.fill_props()
                if self.props_tree.exists(key):
                    self.props_tree.selection_set(key)
                    self.props_tree.see(key)
            dlg.destroy()
        bb = ttk.Frame(fr)
        bb.pack(anchor="e", pady=(10, 0))
        ttk.Button(bb, text="OK", command=ok).pack(side="left", padx=4)
        ttk.Button(bb, text="Отмена", command=dlg.destroy).pack(side="left")
        dlg.bind("<Return>", ok)
        dlg.bind("<Escape>", lambda e: dlg.destroy())
        dlg.geometry("+%d+%d" % (self.root.winfo_rootx() + 200, self.root.winfo_rooty() + 200))
        dlg.grab_set()

    def add_prop(self):
        if not self.props or not self.props.exists:
            return
        key = simpledialog.askstring(APP_TITLE, "Имя параметра:", parent=self.root)
        if key and key.strip():
            key = key.strip()
            if key not in self.props.values:
                self.props.set(key, "")
                self.props_dirty = True
                self.fill_props()
            self.edit_prop(key)

    def save_props(self):
        if not self.props or not self.props.exists:
            return
        try:
            self.props.save()
        except OSError as e:
            messagebox.showerror(APP_TITLE, "Не удалось сохранить: %s" % e)
            return
        self.props_dirty = False
        self.fill_props()
        self.reload_lists()
        self.update_world_info()
        p = self._live()
        self.set_bar("server.properties сохранён" + (" — перезапустите сервер, чтобы применить" if p else ""))

    def _open_props_file(self):
        if self.props and self.props.exists:
            open_path(self.props.path)

    # ================================================================ мир и бэкапы
    def update_world_info(self):
        s = self.srv()
        if not s:
            self.lbl_world.configure(text="—")
            self.lbl_backup_dir.configure(text="")
            return
        name = level_name(s["path"])
        dirs = backup.world_dirs(s["path"])
        self.lbl_world.configure(text="Мир: %s   (%s)" % (name, ", ".join(dirs) if dirs else "ещё не создан"))
        self.lbl_backup_dir.configure(text="Папка: " + backup.backup_dir(s))
        sid = s["id"]

        def calc():
            return sum(backup.dir_size(os.path.join(s["path"], d)) for d in dirs)

        def done(size):
            if self.current == sid:
                self.stat_vars["world"].set(backup.human_size(size) if dirs else "—")
                self.lbl_world.configure(text="Мир: %s   (%s)   %s" % (name, ", ".join(dirs) or "ещё не создан",
                                                                       backup.human_size(size) if dirs else ""))
        self.run_bg(None, calc, done)

    def refresh_backups(self):
        tree = self.backup_tree
        tree.delete(*tree.get_children())
        s = self.srv()
        if not s:
            return
        for b in backup.list_backups(s):
            tree.insert("", "end", iid=b["path"], text=b["name"],
                        values=(datetime.datetime.fromtimestamp(b["mtime"]).strftime("%d.%m.%Y %H:%M:%S"),
                                backup.human_size(b["size"])))

    def _flush_world(self, p):
        """Перед копированием работающего мира: выключить автосохранение и сбросить чанки на диск."""
        p.send("save-off")
        item = p.listen(SAVED_RE)
        p.send("save-all flush")
        line = p.await_item(item, 30)
        if line and not re.search(r"Saved the", line):
            item = p.listen(r"Saved the (game|world)")
            p.send("save-all")
            p.await_item(item, 30)
        time.sleep(1)

    def _with_flushed(self, s, fn):
        p = self.procs.get(s["id"])
        live = p if p and p.running and p.status == RUNNING else None
        if live:
            self._flush_world(live)
        try:
            return fn()
        finally:
            if live and live.running:
                live.send("save-on")

    def backup_now(self, s=None, label=""):
        s = s or self.srv()
        if not s:
            return
        if not backup.world_dirs(s["path"]):
            if s is self.srv():
                messagebox.showinfo(APP_TITLE, "Мир ещё не создан — нечего бэкапить.")
            return

        def job():
            path, skipped = self._with_flushed(s, lambda: backup.make_backup(s, label))
            removed = backup.rotate(s)
            return path, skipped, removed

        def done(r):
            path, skipped, removed = r
            msg = "Бэкап создан: %s" % os.path.basename(path)
            if removed:
                msg += " (удалено старых: %d)" % len(removed)
            if skipped:
                msg += " — пропущено файлов: %d" % len(skipped)
            self.set_bar(msg)
            self.refresh_backups()
            p = self.procs.get(s["id"])
            if p:
                p.info(msg)
        self.last_auto_backup[s["id"]] = time.time()
        self.run_bg("Создаю бэкап «%s»…" % s["name"], job, done)

    def backup_labeled(self):
        label = simpledialog.askstring(APP_TITLE, "Пометка для бэкапа (например, «перед_обновлением»).\n"
                                       "Добавьте слово keep, чтобы бэкап не удалялся автоматически:", parent=self.root)
        if label is not None:
            self.backup_now(label=label)

    def _auto_backup_tick(self):
        try:
            for sid, p in list(self.procs.items()):
                s = self.cfg.get_server(sid)
                if not s or not p.running or p.status != RUNNING:
                    continue
                every = int(s.get("auto_backup_min") or 0) * 60
                if every and time.time() - self.last_auto_backup.get(sid, 0) >= every:
                    self.backup_now(s, label="auto")
        finally:
            self.root.after(30000, self._auto_backup_tick)

    def _require_stopped(self, s, what):
        p = self.procs.get(s["id"])
        if p and p.running:
            messagebox.showwarning(APP_TITLE, "Чтобы %s, сначала остановите сервер «%s»." % (what, s["name"]))
            return False
        return True

    def restore_selected(self):
        s = self.srv()
        sel = self.backup_tree.selection()
        if not s or not sel:
            self.set_bar("Выберите бэкап в списке")
            return
        if not self._require_stopped(s, "восстановить мир"):
            return
        zp = sel[0]
        if not messagebox.askyesno(APP_TITLE, "Восстановить мир из «%s»?\n\nТекущий мир будет заменён. "
                                   "Перед этим автоматически создастся бэкап текущего мира." % os.path.basename(zp)):
            return

        def job():
            if backup.world_dirs(s["path"]):
                backup.make_backup(s, "before-restore")
            return backup.restore_backup(s, zp)

        def done(r):
            dirs, main = r
            cur = level_name(s["path"])
            if main and main != cur:
                if messagebox.askyesno(APP_TITLE, "В бэкапе мир называется «%s», а в server.properties — «%s».\n"
                                       "Переключить level-name на «%s»?" % (main, cur, main)):
                    pr = ServerProperties.for_server(s["path"])
                    pr.set("level-name", main)
                    pr.save()
                    self.load_props()
            self.set_bar("Мир восстановлен: " + ", ".join(dirs))
            self.refresh_backups()
            self.update_world_info()
        self.run_bg("Восстанавливаю мир…", job, done)

    def delete_backups(self):
        sel = self.backup_tree.selection()
        if not sel:
            return
        if not messagebox.askyesno(APP_TITLE, "Удалить выбранные бэкапы (%d шт.) с диска?" % len(sel)):
            return
        for p in sel:
            try:
                os.remove(p)
            except OSError as e:
                messagebox.showerror(APP_TITLE, str(e))
        self.refresh_backups()

    def open_backup_folder(self):
        s = self.srv()
        if s:
            d = backup.backup_dir(s)
            os.makedirs(d, exist_ok=True)
            open_path(d)

    def open_world_folder(self):
        s = self.srv()
        if s:
            open_path(os.path.join(s["path"], level_name(s["path"])))

    def export_world(self):
        s = self.srv()
        if not s or not backup.world_dirs(s["path"]):
            messagebox.showinfo(APP_TITLE, "Мир не найден")
            return
        dest = filedialog.asksaveasfilename(title="Сохранить мир как", defaultextension=".zip",
                                            initialfile="%s_%s.zip" % (level_name(s["path"]), datetime.date.today()),
                                            filetypes=[("ZIP", "*.zip")])
        if not dest:
            return
        self.run_bg("Экспорт мира…", lambda: self._with_flushed(s, lambda: backup.make_backup(s, dest=dest)),
                    lambda r: self.set_bar("Мир сохранён в " + r[0]))

    def copy_world_dialog(self):
        s = self.srv()
        if not s:
            return
        others = [o for o in self.cfg.servers if o["id"] != s["id"]]
        if not others:
            messagebox.showinfo(APP_TITLE, "Добавьте ещё хотя бы один сервер.")
            return
        dlg = tk.Toplevel(self.root)
        dlg.title("Копирование мира")
        dlg.transient(self.root)
        fr = ttk.Frame(dlg, padding=12)
        fr.pack(fill="both")
        direction = tk.StringVar(value="to")
        ttk.Radiobutton(fr, text="Из «%s» в другой сервер" % s["name"], value="to", variable=direction).pack(anchor="w")
        ttk.Radiobutton(fr, text="Из другого сервера в «%s»" % s["name"], value="from", variable=direction).pack(anchor="w")
        ttk.Label(fr, text="Другой сервер:").pack(anchor="w", pady=(8, 0))
        target = ttk.Combobox(fr, values=[o["name"] for o in others], state="readonly", width=40)
        target.current(0)
        target.pack(fill="x")
        ttk.Label(fr, text="Мир получателя будет заменён (перед этим создаётся его бэкап).\n"
                           "Учтите: мир новой версии нельзя открыть на старой версии сервера.",
                  foreground="#b36b00").pack(anchor="w", pady=8)

        def go():
            other = others[target.current()]
            src, dst = (s, other) if direction.get() == "to" else (other, s)
            dlg.destroy()
            if not self._require_stopped(dst, "заменить мир"):
                return
            if not backup.world_dirs(src["path"]):
                messagebox.showerror(APP_TITLE, "В «%s» нет мира." % src["name"])
                return

            def job():
                if backup.world_dirs(dst["path"]):
                    backup.make_backup(dst, "before-copy")
                self._with_flushed(src, lambda: backup.copy_world(src, dst))
            self.run_bg("Копирую мир «%s» → «%s»…" % (src["name"], dst["name"]), job,
                        lambda _r: (self.set_bar("Мир скопирован в «%s»" % dst["name"]), self.refresh_backups(),
                                    self.update_world_info()))
        bb = ttk.Frame(fr)
        bb.pack(anchor="e")
        ttk.Button(bb, text="Копировать", command=go).pack(side="left", padx=4)
        ttk.Button(bb, text="Отмена", command=dlg.destroy).pack(side="left")
        dlg.grab_set()

    def import_world(self):
        s = self.srv()
        if not s or not self._require_stopped(s, "импортировать мир"):
            return
        start = os.path.join(os.environ.get("APPDATA", os.path.expanduser("~")), ".minecraft", "saves")
        folder = filedialog.askdirectory(title="Папка мира (с файлом level.dat)",
                                         initialdir=start if os.path.isdir(start) else None)
        if not folder:
            return
        if not messagebox.askyesno(APP_TITLE, "Заменить мир сервера «%s» миром из\n%s?\n\n"
                                   "Текущий мир будет сохранён в бэкап." % (s["name"], folder)):
            return

        def job():
            if backup.world_dirs(s["path"]):
                backup.make_backup(s, "before-import")
            backup.import_world(s, folder)
        self.run_bg("Импорт мира…", job, lambda _r: (self.set_bar("Мир импортирован"), self.refresh_backups(),
                                                     self.update_world_info()))

    def change_level_name(self):
        s = self.srv()
        if not s:
            return
        pr = ServerProperties.for_server(s["path"])
        if not pr.exists:
            messagebox.showinfo(APP_TITLE, "server.properties ещё не создан.")
            return
        try:
            candidates = [d for d in os.listdir(s["path"]) if os.path.exists(os.path.join(s["path"], d, "level.dat"))]
        except OSError:
            candidates = []
        name = simpledialog.askstring(APP_TITLE, "Папка мира (level-name).\nНайдены миры: %s\n\n"
                                      "Можно ввести новое имя — сервер создаст новый мир." % (", ".join(candidates) or "нет"),
                                      parent=self.root, initialvalue=pr.get("level-name") or "world")
        if name and name.strip():
            pr.set("level-name", name.strip())
            pr.save()
            self.load_props()
            self.update_world_info()
            self.set_bar("level-name = %s (применится после перезапуска)" % name.strip())

    # ================================================================ туннель
    def toggle_tunnel(self):
        if self.tunnel.active or self.tunnel.wanted:
            self.tunnel.stop()
        else:
            try:
                self.tunnel.start()
            except Exception as e:
                messagebox.showerror(APP_TITLE, str(e))
        self._update_tunnel_ui()

    def _update_tunnel_ui(self):
        active = self.tunnel.active
        reconnecting = self.tunnel.wanted and not active
        color = "#2e9d3a" if active else "#d18f00" if reconnecting else "#888888"
        self.tunnel_dot.delete("all")
        self.tunnel_dot.create_oval(2, 2, 12, 12, fill=color, outline="")
        addr = tunnel_address(self.cfg.tunnel.get("command"))
        if active:
            self.lbl_tunnel.configure(text="Открыт" + (" — %s (клик = копировать)" % addr if addr else ""), foreground="#0a58ca")
        else:
            self.lbl_tunnel.configure(text="Переподключение…" if reconnecting else "Закрыт", foreground="")
        self.btn_tunnel.configure(text="Закрыть доступ" if (active or reconnecting) else "Открыть доступ")

    def copy_address(self, _e=None):
        addr = tunnel_address(self.cfg.tunnel.get("command"))
        if addr:
            self.root.clipboard_clear()
            self.root.clipboard_append(addr)
            self.set_bar("Адрес «%s» скопирован" % addr)

    def _tunnel_log(self, text):
        t = self.tunnel_log
        t.configure(state="normal")
        t.insert("end", datetime.datetime.now().strftime("%H:%M:%S ") + text + "\n")
        t.configure(state="disabled")
        t.see("end")

    def save_settings(self):
        t = self.cfg.tunnel
        t["command"] = self.tun_cmd.get().strip()
        for k, v in self.tun_vars.items():
            t[k] = v.get()
        try:
            self.cfg.data["stop_timeout"] = int(self.stop_timeout.get())
        except ValueError:
            pass
        self.cfg.save()
        self._update_tunnel_ui()
        self.set_bar("Настройки сохранены" + (" — перезапустите туннель, чтобы применить" if self.tunnel.active else ""))

    # ================================================================ служебное
    def set_bar(self, text):
        self.lbl_bar.configure(text=text)

    def run_bg(self, text, fn, on_done=None):
        """Выполняет fn в фоне; on_done(result) вызывается в GUI-потоке. Ошибки показываются окном."""
        if text:
            self.busy += 1
            self.set_bar(text)
            self.progress.pack(side="left", padx=8)
            self.progress.start(12)

        def worker():
            try:
                r, err = fn(), None
            except Exception as e:
                r, err = None, e
            self.q.put(("done", text, on_done, r, err))
        threading.Thread(target=worker, daemon=True).start()

    def _drain(self):
        try:
            for _ in range(500):
                item = self.q.get_nowait()
                self._handle(item)
        except queue.Empty:
            pass
        self.root.after(100, self._drain)

    def _handle(self, item):
        kind = item[0]
        if kind == "srv":
            _, ev, sid, payload = item
            if ev == "line":
                if sid == self.current:
                    self.append_console(*payload)
            elif ev == "status":
                self.refresh_server_list()
                self.update_controls()
                if sid == self.current:
                    self.stat_vars["status"].set(STATUS_RU[payload])
                if payload == RUNNING:
                    self.set_bar("Сервер «%s» запущен" % self.cfg.get_server(sid)["name"])
            elif ev == "players":
                p = self.procs.get(sid)
                if p and sid == self.current:
                    self._fill_players(p)
            elif ev == "exited":
                self._on_exited(sid)
        elif kind == "tunnel":
            _, ev, payload = item
            if ev == "line":
                self._tunnel_log(payload)
            self._update_tunnel_ui()
        elif kind == "stats":
            self._apply_stats(item[1], item[2])
        elif kind == "done":
            _, text, on_done, r, err = item
            if text:
                self.busy = max(0, self.busy - 1)
                if not self.busy:
                    self.progress.stop()
                    self.progress.pack_forget()
                    self.set_bar("Готово")
            if err:
                messagebox.showerror(APP_TITLE, "Ошибка: %s" % err)
            elif on_done:
                on_done(r)

    def on_close(self):
        rp = self.running_proc()
        if rp:
            ans = messagebox.askyesnocancel(APP_TITLE, "Сервер «%s» запущен.\n\nДа — остановить сервер и выйти\n"
                                            "Нет — выйти, оставив сервер работать в фоне\nОтмена — вернуться" % rp.srv["name"])
            if ans is None:
                return
            if ans:
                rp.restart_after_stop = False
                rp.stop(self._stop_timeout())
                self.set_bar("Останавливаю сервер перед выходом…")
                self._wait_and_close(time.time())
                return
        self._final_close()

    def _wait_and_close(self, t0):
        if self.running_proc() and time.time() - t0 < self._stop_timeout() + 5:
            self.root.after(300, lambda: self._wait_and_close(t0))
        else:
            self._final_close()

    def _final_close(self):
        self.tunnel.stop()
        try:
            self.cfg.data["geometry"] = self.root.geometry()
            self.cfg.save()
        except OSError:
            pass
        self.root.destroy()


def main():
    if sys.platform == "win32":
        try:
            import ctypes
            ctypes.windll.shcore.SetProcessDpiAwareness(1)
        except Exception:
            pass
    root = tk.Tk()
    App(root)
    root.mainloop()
