//! Главное окно приложения.

use crate::backup::{self, BackupInfo};
use crate::config::{Config, Server, DEFAULT_TUNNEL};
use crate::launch::{self, java_label, tunnel_address};
use crate::lists::{self, ListKind};
use crate::mods::{self, Mod};
use crate::ping;
use crate::props::{self, Kind, Properties};
use crate::server::{flush_world, Level, ServerProcess, Status};
use crate::tunnel::{Tunnel, TunnelState};
use crate::ui::{self, AMBER, BLUE, GRAY, GREEN, RED};
use eframe::egui::{self, Color32, RichText, Ui};
use egui_extras::{Column, TableBuilder};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Console,
    Stats,
    Access,
    Props,
    World,
    Mods,
    Settings,
}

const TABS: [(Tab, &str); 7] = [
    (Tab::Console, "🖥  Консоль"),
    (Tab::Stats, "📊  Статистика"),
    (Tab::Access, "👥  Игроки и доступ"),
    (Tab::Props, "⚙  server.properties"),
    (Tab::World, "🌍  Мир и бэкапы"),
    (Tab::Mods, "🔌  Моды"),
    (Tab::Settings, "🔧  Настройки"),
];

enum TaskResult {
    Msg(String),
    Backup { sid: String, path: PathBuf, skipped: usize, removed: usize },
    Restored { sid: String, dirs: Vec<String>, main: Option<String> },
    WorldChanged { sid: String, msg: String },
    WorldSize { sid: String, size: u64 },
    Mods { sid: String, mods: Vec<Mod> },
    ListChanged,
}

struct TaskMsg {
    label: Option<String>,
    result: Result<TaskResult, String>,
}

#[derive(Default, Clone)]
struct Stats {
    cpu: Option<f32>,
    rss: Option<u64>,
    sys_used: u64,
    sys_total: u64,
    ping: Option<ping::PingInfo>,
}

enum Action {
    RemoveServer(String),
    StopOtherAndStart { other: String, next: String },
    AcceptEula(String),
    Kill,
    Restore(PathBuf),
    DeleteBackups(Vec<PathBuf>),
    DeleteMods(Vec<PathBuf>),
    SetAllMods(bool),
    ImportWorld(PathBuf),
    RemoveListEntries(ListKind, Vec<String>),
    SwitchLevelName(String, String),
    MoveBackups(String, Vec<BackupInfo>),
}

enum PromptAction {
    ListAdd(ListKind),
    Say,
    Tell(String),
    Kick(String),
    Ban(String),
    BackupLabel,
    AddProp,
    LevelName,
    NewBackupDir(PathBuf),
}

enum Dialog {
    Confirm { title: String, text: String, action: Action },
    Prompt { title: String, label: String, value: String, label2: Option<String>, value2: String, action: PromptAction },
    Info { title: String, text: String },
    CopyWorld { other: usize, to_other: bool },
    CloseApp,
}

struct ServerEdit {
    srv: Server,
    is_new: bool,
    jars: Vec<String>,
    javas: Vec<String>,
    error: Option<String>,
}

pub struct App {
    cfg: Config,
    procs: HashMap<String, Arc<ServerProcess>>,
    current: Option<String>,
    tab: Tab,
    tunnel: Tunnel,
    // консоль
    cmd_input: String,
    cmd_history: Vec<String>,
    cmd_pos: usize,
    autoscroll: bool,
    console_filter: String,
    // статистика
    stats: HashMap<String, Stats>,
    history: HashMap<String, VecDeque<(Option<f32>, Option<f32>, Option<f32>)>>,
    stats_rx: Receiver<(String, Stats)>,
    stats_tx: Sender<(String, Stats)>,
    stats_busy: Arc<Mutex<bool>>,
    last_stats: Instant,
    selected_player: Option<String>,
    // server.properties
    props: Option<Properties>,
    props_dirty: bool,
    props_filter: String,
    props_only_known: bool,
    // списки
    list_kind: ListKind,
    lists: HashMap<ListKind, Vec<serde_json::Value>>,
    list_sel: BTreeSet<String>,
    pending_list_reload: Option<Instant>,
    // мир и бэкапы
    backups: Vec<BackupInfo>,
    backup_sel: BTreeSet<PathBuf>,
    world_size: Option<u64>,
    last_auto_backup: HashMap<String, Instant>,
    last_auto_check: Instant,
    // моды
    mods: Vec<Mod>,
    mods_filter: String,
    mods_sel: BTreeSet<PathBuf>,
    mods_loading: bool,
    // прочее
    dialog: Option<Dialog>,
    edit: Option<ServerEdit>,
    tasks_tx: Sender<TaskMsg>,
    tasks_rx: Receiver<TaskMsg>,
    busy: Vec<String>,
    status_msg: String,
    pending_start: Option<(String, Instant)>,
    start_after: Option<String>,
    closing: Option<Instant>,
    allow_close: bool,
    tunnel_log_counter: u64,
    prev_status: HashMap<String, Status>,
}

fn open_path(p: &Path) {
    let _ = open::that_detached(p);
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> App {
        let cfg = Config::load();
        ui::apply_theme(&cc.egui_ctx, cfg.dark_theme);
        let (tasks_tx, tasks_rx) = channel();
        let (stats_tx, stats_rx) = channel();
        let tunnel = Tunnel::new(cfg.tunnel.clone());
        let current = cfg
            .last_server
            .clone()
            .filter(|id| cfg.server(id).is_some())
            .or_else(|| cfg.servers.first().map(|s| s.id.clone()));
        let mut app = App {
            cfg,
            procs: HashMap::new(),
            current: None,
            tab: Tab::Console,
            tunnel,
            cmd_input: String::new(),
            cmd_history: vec![],
            cmd_pos: 0,
            autoscroll: true,
            console_filter: String::new(),
            stats: HashMap::new(),
            history: HashMap::new(),
            stats_rx,
            stats_tx,
            stats_busy: Arc::new(Mutex::new(false)),
            last_stats: Instant::now(),
            selected_player: None,
            props: None,
            props_dirty: false,
            props_filter: String::new(),
            props_only_known: false,
            list_kind: ListKind::Ops,
            lists: HashMap::new(),
            list_sel: BTreeSet::new(),
            pending_list_reload: None,
            backups: vec![],
            backup_sel: BTreeSet::new(),
            world_size: None,
            last_auto_backup: HashMap::new(),
            last_auto_check: Instant::now(),
            mods: vec![],
            mods_filter: String::new(),
            mods_sel: BTreeSet::new(),
            mods_loading: false,
            dialog: None,
            edit: None,
            tasks_tx,
            tasks_rx,
            busy: vec![],
            status_msg: "Готово".into(),
            pending_start: None,
            start_after: None,
            closing: None,
            allow_close: false,
            tunnel_log_counter: 0,
            prev_status: HashMap::new(),
        };
        app.select(current);
        app
    }

    // ------------------------------------------------------------ helpers
    fn cur(&self) -> Option<Server> {
        self.current.as_ref().and_then(|id| self.cfg.server(id)).cloned()
    }

    fn proc(&mut self, sid: &str) -> Arc<ServerProcess> {
        self.procs.entry(sid.to_string()).or_insert_with(|| Arc::new(ServerProcess::new(sid))).clone()
    }

    fn cur_proc(&self) -> Option<Arc<ServerProcess>> {
        self.current.as_ref().and_then(|id| self.procs.get(id)).cloned()
    }

    fn running_proc(&self) -> Option<Arc<ServerProcess>> {
        self.procs.values().find(|p| p.running()).cloned()
    }

    fn status_of(&self, sid: &str) -> Status {
        self.procs.get(sid).map(|p| p.status()).unwrap_or(Status::Stopped)
    }

    fn live(&self) -> Option<Arc<ServerProcess>> {
        self.cur_proc().filter(|p| p.running() && p.status() == Status::Running)
    }

    fn save_cfg(&mut self) {
        self.cfg.last_server = self.current.clone();
        if let Err(e) = self.cfg.save() {
            self.status_msg = format!("Не удалось сохранить настройки: {}", e);
        }
    }

    fn info(&mut self, title: &str, text: impl Into<String>) {
        self.dialog = Some(Dialog::Info { title: title.into(), text: text.into() });
    }

    fn confirm(&mut self, title: &str, text: impl Into<String>, action: Action) {
        self.dialog = Some(Dialog::Confirm { title: title.into(), text: text.into(), action });
    }

    fn prompt(&mut self, title: &str, label: &str, value: &str, label2: Option<&str>, action: PromptAction) {
        self.dialog = Some(Dialog::Prompt {
            title: title.into(),
            label: label.into(),
            value: value.into(),
            label2: label2.map(String::from),
            value2: String::new(),
            action,
        });
    }

    fn run_bg<F>(&mut self, label: Option<String>, job: F)
    where
        F: FnOnce() -> Result<TaskResult, String> + Send + 'static,
    {
        if let Some(l) = &label {
            self.busy.push(l.clone());
            self.status_msg = l.clone();
        }
        let tx = self.tasks_tx.clone();
        thread::spawn(move || {
            let result = job();
            let _ = tx.send(TaskMsg { label, result });
        });
    }

    fn send_cmd(&mut self, cmd: &str) {
        match self.cur_proc().filter(|p| p.running()) {
            Some(p) => {
                if let Err(e) = p.send(cmd) {
                    self.status_msg = e;
                }
            }
            None => self.status_msg = "Сервер не запущен".into(),
        }
    }

    // ------------------------------------------------------------ выбор сервера
    fn select(&mut self, sid: Option<String>) {
        if self.props_dirty {
            self.save_props();
        }
        self.current = sid;
        self.props_dirty = false;
        self.selected_player = None;
        self.list_sel.clear();
        self.backup_sel.clear();
        self.mods_sel.clear();
        self.world_size = None;
        self.reload_all();
        self.cfg.last_server = self.current.clone();
    }

    fn reload_all(&mut self) {
        self.load_props();
        self.reload_lists();
        self.refresh_backups();
        self.refresh_world_size();
        self.refresh_mods();
    }

    // ------------------------------------------------------------ старт / стоп
    fn stop_timeout(&self) -> Duration {
        Duration::from_secs(self.cfg.stop_timeout.max(10))
    }

    fn start_server(&mut self, sid: &str) {
        let Some(srv) = self.cfg.server(sid).cloned() else { return };
        if let Some(other) = self.running_proc() {
            if other.sid != sid {
                let name = self.cfg.server(&other.sid).map(|s| s.name.clone()).unwrap_or_default();
                self.confirm(
                    "Сервер уже запущен",
                    format!("Сейчас запущен «{}».\nОстановить его и запустить «{}»?", name, srv.name),
                    Action::StopOtherAndStart { other: other.sid.clone(), next: sid.to_string() },
                );
                return;
            }
        }
        if self.props_dirty && self.current.as_deref() == Some(sid) {
            self.save_props();
        }
        let eula = Path::new(&srv.path).join("eula.txt");
        let ok = std::fs::read_to_string(&eula)
            .map(|t| t.lines().any(|l| l.trim().replace(' ', "").eq_ignore_ascii_case("eula=true")))
            .unwrap_or(false);
        if !ok {
            self.confirm(
                "EULA",
                "Для запуска нужно принять лицензионное соглашение Minecraft (EULA):\nhttps://aka.ms/MinecraftEULA\n\nПринять и записать eula=true?",
                Action::AcceptEula(sid.to_string()),
            );
            return;
        }
        let p = self.proc(sid);
        match p.start(&srv) {
            Ok(()) => {
                self.last_auto_backup.insert(sid.to_string(), Instant::now());
                self.history.entry(sid.to_string()).or_default().clear();
                self.tab = Tab::Console;
                self.status_msg = format!("Запускаю «{}»…", srv.name);
                if self.cfg.tunnel.start_with_server && self.tunnel.state() == TunnelState::Closed {
                    self.toggle_tunnel();
                }
            }
            Err(e) => self.info("Ошибка запуска", e),
        }
    }

    fn stop_server(&mut self) {
        if let Some(p) = self.cur_proc().filter(|p| p.running()) {
            p.restart_after_stop.store(false, std::sync::atomic::Ordering::SeqCst);
            p.stop(self.stop_timeout());
        }
    }

    fn restart_server(&mut self) {
        match self.cur_proc().filter(|p| p.running()) {
            Some(p) => {
                p.restart_after_stop.store(true, std::sync::atomic::Ordering::SeqCst);
                p.stop(self.stop_timeout());
            }
            None => {
                if let Some(id) = self.current.clone() {
                    self.start_server(&id);
                }
            }
        }
    }

    fn on_exited(&mut self, sid: &str) {
        let p = self.proc(sid);
        let restart = p.restart_after_stop.swap(false, std::sync::atomic::Ordering::SeqCst);
        if restart {
            self.pending_start = Some((sid.to_string(), Instant::now() + Duration::from_millis(1500)));
        } else if let Some(next) = self.start_after.take() {
            self.pending_start = Some((next, Instant::now() + Duration::from_millis(1000)));
        } else if self.cfg.tunnel.stop_with_server {
            self.tunnel.stop();
        }
        if self.cfg.server(sid).map(|s| s.backup_on_stop).unwrap_or(false) {
            self.backup_now(sid, "stop");
        }
        if self.current.as_deref() == Some(sid) {
            self.refresh_world_size();
            self.reload_lists();
        }
    }

    // ------------------------------------------------------------ туннель
    fn toggle_tunnel(&mut self) {
        if self.tunnel.state() != TunnelState::Closed || self.tunnel.wanted() {
            self.tunnel.stop();
        } else {
            self.tunnel.set_settings(self.cfg.tunnel.clone());
            if let Err(e) = self.tunnel.start() {
                self.info("Туннель", e);
            }
        }
    }

    // ------------------------------------------------------------ properties
    fn load_props(&mut self) {
        self.props = self.cur().map(|s| Properties::load(&s.path));
        self.props_dirty = false;
    }

    fn save_props(&mut self) {
        if let Some(p) = &self.props {
            if p.exists {
                match p.save() {
                    Ok(()) => {
                        self.props_dirty = false;
                        let live = self.live().is_some();
                        self.status_msg = format!("server.properties сохранён{}",
                            if live { " — перезапустите сервер, чтобы применить" } else { "" });
                    }
                    Err(e) => self.info("Ошибка", format!("Не удалось сохранить: {}", e)),
                }
            }
        }
    }

    // ------------------------------------------------------------ списки
    fn reload_lists(&mut self) {
        self.lists.clear();
        if let Some(s) = self.cur() {
            for k in lists::ALL {
                self.lists.insert(k, lists::load(&s.path, k));
            }
        }
    }

    fn list_add(&mut self, kind: ListKind, name: String, reason: String) {
        let Some(s) = self.cur() else { return };
        if self.live().is_some() {
            self.send_cmd(&kind.command(true, &name, &reason));
            self.pending_list_reload = Some(Instant::now() + Duration::from_millis(1500));
            return;
        }
        let online = Properties::load(&s.path).get_bool("online-mode", true);
        self.run_bg(Some(format!("Добавляю {}…", name)), move || {
            lists::add_offline(&s.path, kind, &name, online, &reason).map(|_| TaskResult::ListChanged)
        });
    }

    fn list_remove(&mut self, kind: ListKind, names: Vec<String>) {
        let Some(s) = self.cur() else { return };
        if self.live().is_some() {
            for n in &names {
                self.send_cmd(&kind.command(false, n, ""));
            }
            self.pending_list_reload = Some(Instant::now() + Duration::from_millis(1500));
        } else {
            for n in &names {
                if let Err(e) = lists::remove_offline(&s.path, kind, n) {
                    self.status_msg = e.to_string();
                }
            }
            self.reload_lists();
        }
        self.list_sel.clear();
    }

    // ------------------------------------------------------------ бэкапы
    fn refresh_backups(&mut self) {
        self.backups = self.cur().map(|s| backup::list_backups(&s)).unwrap_or_default();
        self.backup_sel.retain(|p| p.exists());
    }

    fn refresh_world_size(&mut self) {
        if let Some(s) = self.cur() {
            self.run_bg(None, move || {
                let size = backup::world_dirs(&s.path).iter().map(|d| backup::dir_size(&Path::new(&s.path).join(d))).sum();
                Ok(TaskResult::WorldSize { sid: s.id, size })
            });
        }
    }

    fn backup_now(&mut self, sid: &str, label: &str) {
        let Some(srv) = self.cfg.server(sid).cloned() else { return };
        if backup::world_dirs(&srv.path).is_empty() {
            if self.current.as_deref() == Some(sid) && label.is_empty() {
                self.info("Бэкап", "Мир ещё не создан — нечего бэкапить.");
            }
            return;
        }
        let p = self.procs.get(sid).cloned();
        let label = label.to_string();
        self.last_auto_backup.insert(sid.to_string(), Instant::now());
        self.run_bg(Some(format!("Создаю бэкап «{}»…", srv.name)), move || {
            let flushed = p.as_ref().map(|p| p.running() && flush_world(p)).unwrap_or(false);
            let r = backup::make_backup(&srv, &label, None);
            if flushed {
                if let Some(p) = &p {
                    let _ = p.send("save-on");
                }
            }
            let r = r.map_err(|e| e.to_string())?;
            let removed = backup::rotate(&srv);
            if let Some(p) = &p {
                p.info(&format!("Бэкап создан: {}", r.path.file_name().unwrap_or_default().to_string_lossy()), Level::Info);
            }
            Ok(TaskResult::Backup { sid: srv.id.clone(), path: r.path, skipped: r.skipped.len(), removed })
        });
    }

    fn export_world(&mut self) {
        let Some(s) = self.cur() else { return };
        if backup::world_dirs(&s.path).is_empty() {
            return self.info("Экспорт", "Мир не найден");
        }
        let name = format!("{}_{}.zip", props::level_name(&s.path), chrono::Local::now().format("%Y-%m-%d"));
        let Some(dest) = rfd::FileDialog::new().set_file_name(&name).add_filter("ZIP", &["zip"]).save_file() else { return };
        let p = self.procs.get(&s.id).cloned();
        self.run_bg(Some("Экспорт мира…".into()), move || {
            let flushed = p.as_ref().map(|p| p.running() && flush_world(p)).unwrap_or(false);
            let r = backup::make_backup(&s, "", Some(dest));
            if flushed {
                let _ = p.unwrap().send("save-on");
            }
            r.map(|r| TaskResult::Msg(format!("Мир сохранён в {}", r.path.display()))).map_err(|e| e.to_string())
        });
    }

    fn set_backup_dir(&mut self, path: String) {
        let Some(id) = self.current.clone() else { return };
        let old = self.backups.clone();
        let old_dir = self.cur().map(|s| backup::backup_dir(&s));
        if let Some(s) = self.cfg.server_mut(&id) {
            s.backup_dir = path;
        }
        self.save_cfg();
        self.refresh_backups();
        let new_dir = self.cur().map(|s| backup::backup_dir(&s));
        if !old.is_empty() && old_dir != new_dir {
            self.confirm(
                "Папка бэкапов",
                format!("В старой папке {} бэкап(ов). Перенести их в новую папку?", old.len()),
                Action::MoveBackups(id, old),
            );
        }
        if let Some(d) = new_dir {
            self.status_msg = format!("Папка бэкапов: {}", d.display());
        }
    }

    fn require_stopped(&mut self, sid: &str, what: &str) -> bool {
        if self.procs.get(sid).map(|p| p.running()).unwrap_or(false) {
            let name = self.cfg.server(sid).map(|s| s.name.clone()).unwrap_or_default();
            self.info("Сервер запущен", format!("Чтобы {}, сначала остановите сервер «{}».", what, name));
            return false;
        }
        true
    }

    // ------------------------------------------------------------ моды
    fn refresh_mods(&mut self) {
        let Some(s) = self.cur() else {
            self.mods.clear();
            return;
        };
        self.mods_loading = true;
        self.run_bg(None, move || Ok(TaskResult::Mods { sid: s.id.clone(), mods: mods::list_mods(&s.path) }));
    }

    fn set_mods_enabled(&mut self, paths: &BTreeSet<PathBuf>, enabled: Option<bool>) {
        let mut errors = vec![];
        let mut new_sel = BTreeSet::new();
        for m in self.mods.iter_mut().filter(|m| paths.contains(&m.path)) {
            let en = enabled.unwrap_or(!m.enabled);
            match mods::set_enabled(&m.path, en) {
                Ok(p) => {
                    m.path = p;
                    m.enabled = en;
                }
                Err(e) => errors.push(format!("{}: {}", m.file, e)),
            }
            new_sel.insert(m.path.clone());
        }
        if self.mods_sel.len() == paths.len() {
            self.mods_sel = new_sel;
        }
        if !errors.is_empty() {
            self.info("Моды", format!("Не удалось переключить:\n{}", errors.join("\n")));
        }
        let running = self.cur_proc().map(|p| p.running()).unwrap_or(false);
        self.status_msg = format!("Моды изменены{}", if running { " — перезапустите сервер, чтобы применить" } else { "" });
    }

    // ------------------------------------------------------------ события
    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.tasks_rx.try_recv() {
            if let Some(l) = &msg.label {
                if let Some(i) = self.busy.iter().position(|b| b == l) {
                    self.busy.remove(i);
                }
                if self.busy.is_empty() {
                    self.status_msg = "Готово".into();
                }
            }
            match msg.result {
                Err(e) => self.info("Ошибка", e),
                Ok(r) => self.handle_result(r),
            }
        }
        while let Ok((sid, st)) = self.stats_rx.try_recv() {
            let h = self.history.entry(sid.clone()).or_default();
            if h.len() >= 150 {
                h.pop_front();
            }
            let players = st.ping.as_ref().map(|p| p.online as f32)
                .or_else(|| self.procs.get(&sid).map(|p| p.players().len() as f32));
            h.push_back((st.cpu, st.rss.map(|r| r as f32 / 1048576.0), players));
            self.stats.insert(sid, st);
        }
        let changes: Vec<(String, Status)> = self.procs.values().map(|p| (p.sid.clone(), p.status())).collect();
        for (sid, st) in changes {
            let prev = self.prev_status.insert(sid.clone(), st);
            if prev != Some(st) && prev.is_some() {
                let name = self.cfg.server(&sid).map(|s| s.name.clone()).unwrap_or_default();
                self.status_msg = match st {
                    Status::Running => format!("Сервер «{}» запущен", name),
                    Status::Stopped => format!("Сервер «{}» остановлен", name),
                    Status::Stopping => format!("Останавливаю «{}»…", name),
                    Status::Starting => format!("Запускаю «{}»…", name),
                };
            }
        }
        let exited: Vec<String> = self.procs.values().filter_map(|p| p.take_exit().map(|_| p.sid.clone())).collect();
        for sid in exited {
            self.on_exited(&sid);
        }
        if let Some((sid, at)) = self.pending_start.clone() {
            if Instant::now() >= at {
                self.pending_start = None;
                if self.current.as_deref() != Some(&sid) {
                    self.select(Some(sid.clone()));
                }
                self.start_server(&sid);
            }
        }
        if let Some(at) = self.pending_list_reload {
            if Instant::now() >= at {
                self.pending_list_reload = None;
                self.reload_lists();
            }
        }
        // статистика раз в 2 секунды в фоне
        if self.last_stats.elapsed() >= Duration::from_secs(2) && !*self.stats_busy.lock().unwrap() {
            self.last_stats = Instant::now();
            let targets: Vec<(String, u32, Status, String)> = self
                .procs
                .values()
                .filter_map(|p| p.pid().map(|pid| (p.sid.clone(), pid, p.status(), self.cfg.server(&p.sid).map(|s| s.path.clone()).unwrap_or_default())))
                .collect();
            if !targets.is_empty() {
                *self.stats_busy.lock().unwrap() = true;
                let tx = self.stats_tx.clone();
                let busy = self.stats_busy.clone();
                thread::spawn(move || {
                    collect_stats(targets, tx);
                    *busy.lock().unwrap() = false;
                });
            }
        }
        // автобэкап
        if self.last_auto_check.elapsed() >= Duration::from_secs(30) {
            self.last_auto_check = Instant::now();
            let due: Vec<String> = self
                .cfg
                .servers
                .iter()
                .filter(|s| s.auto_backup_min > 0)
                .filter(|s| self.status_of(&s.id) == Status::Running)
                .filter(|s| {
                    self.last_auto_backup.get(&s.id).map(|t| t.elapsed() >= Duration::from_secs(s.auto_backup_min as u64 * 60)).unwrap_or(true)
                })
                .map(|s| s.id.clone())
                .collect();
            for sid in due {
                self.backup_now(&sid, "auto");
            }
        }
        // корректный выход после остановки
        if let Some(t0) = self.closing {
            if self.running_proc().is_none() || t0.elapsed() > self.stop_timeout() + Duration::from_secs(5) {
                self.allow_close = true;
                self.tunnel.stop();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        let active = self.running_proc().is_some() || !self.busy.is_empty() || self.tunnel.wanted() || self.pending_start.is_some();
        ctx.request_repaint_after(if active { Duration::from_millis(250) } else { Duration::from_secs(1) });
    }

    fn handle_result(&mut self, r: TaskResult) {
        match r {
            TaskResult::Msg(m) => self.status_msg = m,
            TaskResult::Backup { sid, path, skipped, removed } => {
                let mut m = format!("Бэкап создан: {}", path.file_name().unwrap_or_default().to_string_lossy());
                if removed > 0 {
                    m += &format!(" (удалено старых: {})", removed);
                }
                if skipped > 0 {
                    m += &format!(" — пропущено файлов: {}", skipped);
                }
                self.status_msg = m;
                if self.current.as_deref() == Some(&sid) {
                    self.refresh_backups();
                }
            }
            TaskResult::Restored { sid, dirs, main } => {
                self.status_msg = format!("Мир восстановлен: {}", dirs.join(", "));
                if let Some(s) = self.cfg.server(&sid).cloned() {
                    let cur = props::level_name(&s.path);
                    if let Some(m) = main.filter(|m| *m != cur) {
                        self.confirm(
                            "Имя мира",
                            format!("В бэкапе мир называется «{}», а в server.properties — «{}».\nПереключить level-name на «{}»?", m, cur, m),
                            Action::SwitchLevelName(sid.clone(), m),
                        );
                    }
                }
                self.refresh_backups();
                self.refresh_world_size();
            }
            TaskResult::WorldChanged { sid, msg } => {
                self.status_msg = msg;
                if self.current.as_deref() == Some(&sid) {
                    self.refresh_backups();
                    self.refresh_world_size();
                }
            }
            TaskResult::WorldSize { sid, size } => {
                if self.current.as_deref() == Some(&sid) {
                    self.world_size = Some(size);
                }
            }
            TaskResult::Mods { sid, mods } => {
                if self.current.as_deref() == Some(&sid) {
                    self.mods = mods;
                    self.mods_loading = false;
                }
            }
            TaskResult::ListChanged => self.reload_lists(),
        }
    }

    fn do_action(&mut self, a: Action) {
        match a {
            Action::RemoveServer(sid) => {
                self.cfg.servers.retain(|s| s.id != sid);
                self.procs.remove(&sid);
                let next = self.cfg.servers.first().map(|s| s.id.clone());
                self.select(next);
                self.save_cfg();
            }
            Action::StopOtherAndStart { other, next } => {
                if let Some(p) = self.procs.get(&other).cloned() {
                    p.restart_after_stop.store(false, std::sync::atomic::Ordering::SeqCst);
                    self.start_after = Some(next);
                    p.stop(self.stop_timeout());
                    self.status_msg = "Останавливаю текущий сервер…".into();
                }
            }
            Action::AcceptEula(sid) => {
                if let Some(s) = self.cfg.server(&sid).cloned() {
                    let text = format!("# Accepted via MC Server Manager {}\neula=true\n", chrono::Local::now().format("%Y-%m-%dT%H:%M:%S"));
                    match std::fs::write(Path::new(&s.path).join("eula.txt"), text) {
                        Ok(()) => self.start_server(&sid),
                        Err(e) => self.info("EULA", e.to_string()),
                    }
                }
            }
            Action::Kill => {
                if let Some(p) = self.cur_proc() {
                    p.restart_after_stop.store(false, std::sync::atomic::Ordering::SeqCst);
                    p.kill();
                }
            }
            Action::Restore(zip) => {
                let Some(s) = self.cur() else { return };
                self.run_bg(Some("Восстанавливаю мир…".into()), move || {
                    if !backup::world_dirs(&s.path).is_empty() {
                        backup::make_backup(&s, "before-restore", None).map_err(|e| e.to_string())?;
                    }
                    let (dirs, main) = backup::restore_backup(&s, &zip).map_err(|e| e.to_string())?;
                    Ok(TaskResult::Restored { sid: s.id.clone(), dirs, main })
                });
            }
            Action::DeleteBackups(paths) => {
                for p in paths {
                    if let Err(e) = std::fs::remove_file(&p) {
                        self.status_msg = e.to_string();
                    }
                }
                self.backup_sel.clear();
                self.refresh_backups();
            }
            Action::DeleteMods(paths) => {
                for p in paths {
                    if let Err(e) = std::fs::remove_file(&p) {
                        self.status_msg = e.to_string();
                    }
                }
                self.mods_sel.clear();
                self.refresh_mods();
            }
            Action::SetAllMods(en) => {
                let all: BTreeSet<PathBuf> = self.mods.iter().filter(|m| m.enabled != en).map(|m| m.path.clone()).collect();
                self.set_mods_enabled(&all, Some(en));
            }
            Action::ImportWorld(folder) => {
                let Some(s) = self.cur() else { return };
                self.run_bg(Some("Импорт мира…".into()), move || {
                    if !backup::world_dirs(&s.path).is_empty() {
                        backup::make_backup(&s, "before-import", None).map_err(|e| e.to_string())?;
                    }
                    backup::import_world(&s, &folder).map_err(|e| e.to_string())?;
                    Ok(TaskResult::WorldChanged { sid: s.id.clone(), msg: "Мир импортирован".into() })
                });
            }
            Action::RemoveListEntries(kind, names) => self.list_remove(kind, names),
            Action::SwitchLevelName(sid, name) => {
                if let Some(s) = self.cfg.server(&sid).cloned() {
                    let mut p = Properties::load(&s.path);
                    p.set("level-name", &name);
                    let _ = p.save();
                    self.load_props();
                    self.refresh_world_size();
                }
            }
            Action::MoveBackups(sid, old) => {
                let Some(s) = self.cfg.server(&sid).cloned() else { return };
                self.run_bg(Some("Переношу бэкапы…".into()), move || {
                    let dir = backup::backup_dir(&s);
                    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                    for b in old {
                        let target = dir.join(&b.name);
                        if std::fs::rename(&b.path, &target).is_err() {
                            std::fs::copy(&b.path, &target).map_err(|e| e.to_string())?;
                            let _ = std::fs::remove_file(&b.path);
                        }
                    }
                    Ok(TaskResult::WorldChanged { sid: s.id.clone(), msg: "Бэкапы перенесены".into() })
                });
            }
        }
    }

    fn do_prompt(&mut self, action: PromptAction, value: String, value2: String) {
        let v = value.trim().to_string();
        match action {
            PromptAction::ListAdd(kind) => {
                if !v.is_empty() {
                    self.list_add(kind, v, value2.trim().to_string());
                }
            }
            PromptAction::Say => self.send_cmd(&format!("say {}", v)),
            PromptAction::Tell(p) => self.send_cmd(&format!("tell {} {}", p, v)),
            PromptAction::Kick(p) => self.send_cmd(&format!("kick {} {}", p, v)),
            PromptAction::Ban(p) => {
                self.send_cmd(&format!("ban {} {}", p, v));
                self.pending_list_reload = Some(Instant::now() + Duration::from_millis(1500));
            }
            PromptAction::BackupLabel => {
                if let Some(id) = self.current.clone() {
                    let label = if v.is_empty() { "manual".to_string() } else { v };
                    self.backup_now(&id, &label);
                }
            }
            PromptAction::AddProp => {
                if let Some(p) = &mut self.props {
                    if !v.is_empty() && p.get(&v).is_none() {
                        p.set(&v, value2.trim());
                        self.props_dirty = true;
                        self.props_filter = v;
                    }
                }
            }
            PromptAction::LevelName => {
                if let (Some(s), false) = (self.cur(), v.is_empty()) {
                    let mut p = Properties::load(&s.path);
                    p.set("level-name", &v);
                    let _ = p.save();
                    self.load_props();
                    self.refresh_world_size();
                    self.status_msg = format!("level-name = {} (применится после перезапуска)", v);
                }
            }
            PromptAction::NewBackupDir(parent) => {
                if !v.is_empty() {
                    let path = parent.join(&v);
                    match std::fs::create_dir_all(&path) {
                        Ok(()) => self.set_backup_dir(path.to_string_lossy().to_string()),
                        Err(e) => self.info("Ошибка", format!("Не удалось создать папку: {}", e)),
                    }
                }
            }
        }
    }
}

fn collect_stats(targets: Vec<(String, u32, Status, String)>, tx: Sender<(String, Stats)>) {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    static SYS: std::sync::OnceLock<Mutex<System>> = std::sync::OnceLock::new();
    let sys = SYS.get_or_init(|| Mutex::new(System::new()));
    let mut sys = sys.lock().unwrap();
    let pids: Vec<Pid> = targets.iter().map(|t| Pid::from_u32(t.1)).collect();
    sys.refresh_processes_specifics(ProcessesToUpdate::Some(&pids), true, ProcessRefreshKind::nothing().with_cpu().with_memory());
    sys.refresh_memory();
    let cores = thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f32;
    for (sid, pid, status, path) in targets {
        let proc_ = sys.process(Pid::from_u32(pid));
        let ping = if status == Status::Running {
            ping::ping("127.0.0.1", props::server_port(&path), Duration::from_millis(1500)).ok()
        } else {
            None
        };
        let st = Stats {
            cpu: proc_.map(|p| p.cpu_usage() / cores),
            rss: proc_.map(|p| p.memory()),
            sys_used: sys.used_memory(),
            sys_total: sys.total_memory(),
            ping,
        };
        let _ = tx.send((sid, st));
    }
}

// ================================================================ отрисовка
impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close {
            if self.running_proc().is_some() && self.closing.is_none() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.dialog = Some(Dialog::CloseApp);
            } else {
                self.tunnel.stop();
                self.save_cfg();
            }
        }
        self.status_bar(ctx);
        self.server_panel(ctx);
        egui::CentralPanel::default().show(ctx, |ui| self.main_area(ui));
        self.dialogs(ctx);
        self.server_editor(ctx);
    }
}

impl App {
    fn status_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if !self.busy.is_empty() {
                    ui.spinner();
                }
                ui.add(egui::Label::new(&self.status_msg).truncate());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    match self.running_proc() {
                        Some(p) => {
                            let name = self.cfg.server(&p.sid).map(|s| s.name.clone()).unwrap_or_default();
                            ui.label(RichText::new(format!("Запущен: {} ({})", name, p.status().ru())).color(GREEN));
                        }
                        None => {
                            ui.weak("Ни один сервер не запущен");
                        }
                    }
                });
            });
        });
    }

    fn server_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("servers").resizable(true).default_width(250.0).width_range(180.0..=480.0).show(ctx, |ui| {
            ui.add_space(6.0);
            ui.heading("Серверы");
            ui.add_space(4.0);
            let mut clicked = None;
            let mut dbl = None;
            let bottom_h = 76.0;
            egui::ScrollArea::vertical().max_height(ui.available_height() - bottom_h).auto_shrink([false, false]).show(ui, |ui| {
                for s in &self.cfg.servers {
                    let st = self.procs.get(&s.id).map(|p| p.status()).unwrap_or(Status::Stopped);
                    let selected = self.current.as_deref() == Some(&s.id);
                    let vis = ui.visuals().clone();
                    let fill = if selected { vis.selection.bg_fill } else { vis.faint_bg_color };
                    let resp = egui::Frame::new()
                        .fill(fill)
                        .corner_radius(8)
                        .inner_margin(egui::Margin::symmetric(10, 8))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui::status_dot(ui, status_color(st), 14.0);
                                ui.add(egui::Label::new(RichText::new(&s.name).strong()).wrap());
                            });
                            let core = if s.launch_mode == "jar" { s.jar.clone() } else { "Forge / свои аргументы".into() };
                            let sub = RichText::new(format!("{} · {}", core, st.ru())).small();
                            let sub = if selected { sub.color(ui.visuals().strong_text_color().gamma_multiply(0.8)) } else { sub.weak() };
                            ui.add(egui::Label::new(sub).truncate());
                        })
                        .response
                        .interact(egui::Sense::click());
                    if resp.clicked() {
                        clicked = Some(s.id.clone());
                    }
                    if resp.double_clicked() {
                        dbl = Some(s.id.clone());
                    }
                    resp.on_hover_text(&s.path);
                    ui.add_space(2.0);
                }
                if self.cfg.servers.is_empty() {
                    ui.weak("Пока нет серверов.\nНажмите «Добавить» и выберите папку сервера.");
                }
            });
            if let Some(id) = clicked {
                if self.current.as_deref() != Some(&id) {
                    self.select(Some(id));
                }
            }
            if let Some(id) = dbl {
                self.open_editor(&id);
            }
            ui.separator();
            ui.horizontal(|ui| {
                let w = (ui.available_width() - 16.0) / 3.0;
                if ui.add_sized([w, 28.0], egui::Button::new("➕ Добавить")).clicked() {
                    self.new_server();
                }
                if ui.add_sized([w, 28.0], egui::Button::new("✏ Изменить")).clicked() {
                    if let Some(id) = self.current.clone() {
                        self.open_editor(&id);
                    }
                }
                if ui.add_sized([w, 28.0], egui::Button::new("🗑 Удалить")).clicked() {
                    if let Some(s) = self.cur() {
                        if self.require_stopped(&s.id, "убрать сервер из списка") {
                            self.confirm("Удалить", format!("Убрать «{}» из списка?\n\nФайлы сервера на диске НЕ удаляются.", s.name),
                                         Action::RemoveServer(s.id));
                        }
                    }
                }
            });
            ui.horizontal(|ui| {
                if ui.button("⬆").on_hover_text("Выше").clicked() {
                    self.move_server(-1);
                }
                if ui.button("⬇").on_hover_text("Ниже").clicked() {
                    self.move_server(1);
                }
                if ui.add_sized([ui.available_width(), 26.0], egui::Button::new("📁 Открыть папку")).clicked() {
                    if let Some(s) = self.cur() {
                        open_path(Path::new(&s.path));
                    }
                }
            });
        });
    }

    fn move_server(&mut self, d: i32) {
        let Some(id) = self.current.clone() else { return };
        let i = self.cfg.servers.iter().position(|s| s.id == id).unwrap_or(0) as i32;
        let j = i + d;
        if j >= 0 && (j as usize) < self.cfg.servers.len() {
            self.cfg.servers.swap(i as usize, j as usize);
            self.save_cfg();
        }
    }

    fn main_area(&mut self, ui: &mut Ui) {
        let Some(s) = self.cur() else {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("Добавьте сервер кнопкой «➕ Добавить» слева").size(18.0).weak());
            });
            return;
        };
        let p = self.proc(&s.id);
        let st = p.status();
        let running = p.running();
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.add(egui::Label::new(RichText::new(&s.name).heading().strong()).wrap());
            pill(ui, st.ru(), status_color(st));
        });
        let core = if s.launch_mode == "jar" { s.jar.clone() } else { "свои аргументы запуска".into() };
        ui.add(egui::Label::new(RichText::new(format!("Ядро: {}   ·   Java: {}   ·   RAM: {}–{}   ·   {}",
            core, java_label(&s.java), s.xms, s.xmx, s.path)).small().weak()).truncate());
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            let start = egui::Button::new(RichText::new("▶  Запустить").strong().color(Color32::WHITE)).fill(GREEN.gamma_multiply(0.85));
            if ui.add_enabled(!running, start).clicked() {
                self.start_server(&s.id);
            }
            let stop = egui::Button::new(RichText::new("⏹  Остановить").color(Color32::WHITE)).fill(RED.gamma_multiply(0.8));
            if ui.add_enabled(running && st != Status::Stopping, stop).clicked() {
                self.stop_server();
            }
            if ui.add_enabled(st != Status::Stopping, egui::Button::new("🔄  Перезапустить")).clicked() {
                self.restart_server();
            }
            if ui.add_enabled(running, egui::Button::new("✖  Убить")).on_hover_text("Завершить процесс без сохранения (если сервер завис)").clicked() {
                self.confirm("Убить процесс", "Завершить процесс без сохранения мира?\nИспользуйте, только если сервер завис.", Action::Kill);
            }
            ui.separator();
            if ui.button("💾  Бэкап сейчас").clicked() {
                self.backup_now(&s.id, "");
            }
            ui.separator();
            let ts = self.tunnel.state();
            let (txt, col) = match ts {
                TunnelState::Open => ("Доступ открыт", GREEN),
                TunnelState::Reconnecting => ("Переподключение…", AMBER),
                TunnelState::Closed => ("Доступ закрыт", GRAY),
            };
            let btn = if ts == TunnelState::Closed && !self.tunnel.wanted() { "🌐  Открыть доступ" } else { "🌐  Закрыть доступ" };
            if ui.button(btn).on_hover_text(&self.cfg.tunnel.command).clicked() {
                self.toggle_tunnel();
            }
            ui::status_dot(ui, col, 12.0);
            ui.label(RichText::new(txt).color(col));
            if ts == TunnelState::Open {
                if let Some(addr) = tunnel_address(&self.cfg.tunnel.command) {
                    if ui.link(format!("{} 📋", addr)).on_hover_text("Скопировать адрес").clicked() {
                        ui.ctx().copy_text(addr.clone());
                        self.status_msg = format!("Адрес «{}» скопирован", addr);
                    }
                }
            }
        });
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            for (t, label) in TABS {
                if ui.selectable_label(self.tab == t, RichText::new(label).size(15.0)).clicked() {
                    self.tab = t;
                }
            }
        });
        ui.separator();
        match self.tab {
            Tab::Console => self.tab_console(ui, &p),
            Tab::Stats => self.tab_stats(ui, &s, &p),
            Tab::Access => self.tab_access(ui, &s),
            Tab::Props => self.tab_props(ui),
            Tab::World => self.tab_world(ui, &s),
            Tab::Mods => self.tab_mods(ui, &s),
            Tab::Settings => self.tab_settings(ui),
        }
    }

    // ------------------------------------------------------------ консоль
    fn tab_console(&mut self, ui: &mut Ui, p: &Arc<ServerProcess>) {
        let bottom = 104.0;
        let lines: Vec<(String, Level)> = {
            let sh = p.shared.lock().unwrap();
            let f = self.console_filter.to_lowercase();
            sh.log.iter().filter(|l| f.is_empty() || l.text.to_lowercase().contains(&f)).map(|l| (l.text.clone(), l.level)).collect()
        };
        egui::Frame::new()
            .fill(Color32::from_rgb(16, 17, 20))
            .corner_radius(8)
            .inner_margin(egui::Margin::same(8))
            .show(ui, |ui| {
                let h = (ui.available_height() - bottom).max(100.0);
                let row_h = ui.text_style_height(&egui::TextStyle::Monospace) + 2.0;
                egui::ScrollArea::vertical()
                    .max_height(h)
                    .auto_shrink([false, false])
                    .stick_to_bottom(self.autoscroll)
                    .show_rows(ui, row_h, lines.len(), |ui, range| {
                        for (text, lvl) in &lines[range] {
                            let c = match lvl {
                                Level::Warn => Color32::from_rgb(229, 192, 123),
                                Level::Error => Color32::from_rgb(240, 113, 120),
                                Level::Info => Color32::from_rgb(97, 175, 239),
                                Level::Cmd => Color32::from_rgb(152, 195, 121),
                                Level::Normal => Color32::from_rgb(212, 212, 212),
                            };
                            ui.add(egui::Label::new(RichText::new(text).monospace().color(c)).truncate());
                        }
                    });
            });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("›");
            let w = (ui.available_width() - 400.0).max(150.0);
            let resp = ui.add(egui::TextEdit::singleline(&mut self.cmd_input).desired_width(w).font(egui::TextStyle::Monospace)
                .hint_text("Команда серверу, например: say Привет"));
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if resp.has_focus() {
                if ui.input(|i| i.key_pressed(egui::Key::ArrowUp)) && !self.cmd_history.is_empty() {
                    self.cmd_pos = self.cmd_pos.saturating_sub(1);
                    self.cmd_input = self.cmd_history[self.cmd_pos].clone();
                }
                if ui.input(|i| i.key_pressed(egui::Key::ArrowDown)) && !self.cmd_history.is_empty() {
                    self.cmd_pos = (self.cmd_pos + 1).min(self.cmd_history.len());
                    self.cmd_input = self.cmd_history.get(self.cmd_pos).cloned().unwrap_or_default();
                }
            }
            if (enter || ui.button("Отправить").clicked()) && !self.cmd_input.trim().is_empty() {
                let c = std::mem::take(&mut self.cmd_input);
                self.send_cmd(&c);
                if self.cmd_history.last() != Some(&c) {
                    self.cmd_history.push(c);
                }
                self.cmd_pos = self.cmd_history.len();
                resp.request_focus();
            }
            ui.checkbox(&mut self.autoscroll, "Автопрокрутка");
            if ui.button("Очистить").clicked() {
                p.clear_log();
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.weak("Быстро:");
            for (label, cmd) in [("Игроки", "list"), ("Сохранить мир", "save-all"), ("День", "time set day"),
                                 ("Ночь", "time set night"), ("Ясно", "weather clear"), ("TPS", "tps")] {
                if ui.small_button(label).on_hover_text(cmd).clicked() {
                    self.send_cmd(cmd);
                }
            }
            if ui.small_button("Объявление…").clicked() {
                self.prompt("Объявление", "Сообщение всем игрокам:", "", None, PromptAction::Say);
            }
            ui.add_space(12.0);
            ui.weak("Фильтр:");
            ui.add(egui::TextEdit::singleline(&mut self.console_filter).desired_width(140.0));
        });
    }

    // ------------------------------------------------------------ статистика
    fn tab_stats(&mut self, ui: &mut Ui, s: &Server, p: &Arc<ServerProcess>) {
        let st = self.stats.get(&s.id).cloned().unwrap_or_default();
        let running = p.running();
        let players = p.players();
        let players: Vec<String> = if players.is_empty() {
            st.ping.as_ref().map(|x| x.players.clone()).unwrap_or_default()
        } else {
            players
        };
        let full = ui.available_rect_before_wrap();
        let split = full.max.x - 270.0;
        let left_rect = egui::Rect::from_min_max(full.min, egui::pos2(split - 12.0, full.max.y));
        let right_rect = egui::Rect::from_min_max(egui::pos2(split, full.min.y), full.max);
        ui.painter().vline(split - 6.0, full.y_range(), ui.visuals().widgets.noninteractive.bg_stroke);
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(right_rect), |ui| {
            ui.heading(format!("Игроки онлайн ({})", players.len()));
            ui.add_space(4.0);
            egui::ScrollArea::vertical().id_salt("players").max_height(ui.available_height() - 110.0).auto_shrink([false, false]).show(ui, |ui| {
                for name in &players {
                    let sel = self.selected_player.as_deref() == Some(name);
                    let r = ui.selectable_label(sel, RichText::new(format!("👤 {}", name)).size(15.0));
                    if r.clicked() {
                        self.selected_player = Some(name.clone());
                    }
                    r.context_menu(|ui| {
                        self.selected_player = Some(name.clone());
                        self.player_menu(ui, name);
                    });
                }
                if players.is_empty() {
                    ui.weak(if running { "Никого нет" } else { "Сервер не запущен" });
                }
            });
            ui.separator();
            let sel = self.selected_player.clone().filter(|n| players.contains(n));
            ui.add_enabled_ui(sel.is_some() && running, |ui| {
                let n = sel.clone().unwrap_or_default();
                egui::Grid::new("pl_btns").num_columns(3).spacing([4.0, 4.0]).show(ui, |ui| {
                    if ui.button("Кик").clicked() {
                        self.prompt("Кик", &format!("Причина кика {}:", n), "", None, PromptAction::Kick(n.clone()));
                    }
                    if ui.button("Бан").clicked() {
                        self.prompt("Бан", &format!("Причина бана {}:", n), "", None, PromptAction::Ban(n.clone()));
                    }
                    if ui.button("Сообщение").clicked() {
                        self.prompt("Сообщение", &format!("Сообщение для {}:", n), "", None, PromptAction::Tell(n.clone()));
                    }
                    ui.end_row();
                    if ui.button("OP").clicked() {
                        self.send_cmd(&format!("op {}", n));
                        self.pending_list_reload = Some(Instant::now() + Duration::from_millis(1500));
                    }
                    if ui.button("Снять OP").clicked() {
                        self.send_cmd(&format!("deop {}", n));
                        self.pending_list_reload = Some(Instant::now() + Duration::from_millis(1500));
                    }
                    ui.menu_button("Режим ▾", |ui| {
                        for (m, l) in [("survival", "Выживание"), ("creative", "Творческий"), ("adventure", "Приключение"), ("spectator", "Наблюдатель")] {
                            if ui.button(l).clicked() {
                                self.send_cmd(&format!("gamemode {} {}", m, n));
                                ui.close_menu();
                            }
                        }
                    });
                    ui.end_row();
                });
            });
            ui.weak("Правый клик по игроку — все действия");
        });

        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(left_rect), |ui| egui::ScrollArea::vertical().id_salt("stats").auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui::stat_card(ui, "Статус", p.status().ru(), Some(status_color(p.status())));
                ui::stat_card(ui, "Время работы", &p.uptime().map(ui::fmt_duration).unwrap_or("—".into()), None);
                let online = st.ping.as_ref().map(|x| format!("{} / {}", x.online, x.max)).unwrap_or(if running { players.len().to_string() } else { "—".into() });
                ui::stat_card(ui, "Игроки", &online, None);
                ui::stat_card(ui, "Версия", st.ping.as_ref().map(|x| x.version.as_str()).unwrap_or("—"), None);
                ui::stat_card(ui, "Пинг (локально)", &st.ping.as_ref().and_then(|x| x.latency_ms).map(|l| format!("{} мс", l)).unwrap_or("—".into()), None);
                ui::stat_card(ui, "CPU процесса", &st.cpu.filter(|_| running).map(|c| format!("{:.1} %", c)).unwrap_or("—".into()), None);
                ui::stat_card(ui, "RAM процесса", &st.rss.filter(|_| running).map(backup::human_size).unwrap_or("—".into()), None);
                let sys = if st.sys_total > 0 { format!("{} / {}", backup::human_size(st.sys_used), backup::human_size(st.sys_total)) } else { "—".into() };
                ui::stat_card(ui, "RAM системы", &sys, None);
                ui::stat_card(ui, "Размер мира", &self.world_size.map(backup::human_size).unwrap_or("—".into()), None);
            });
            if let Some(motd) = st.ping.as_ref().map(|x| x.motd.replace('\n', " ")) {
                ui.add_space(4.0);
                ui.label(RichText::new(format!("MOTD: {}", motd)).weak());
            }
            ui.add_space(8.0);
            let h: Vec<(Option<f32>, Option<f32>, Option<f32>)> = self.history.get(&s.id).map(|h| h.iter().copied().collect()).unwrap_or_default();
            let xmx_mb = parse_mem_mb(&s.xmx).unwrap_or(1024.0);
            let ch = ((ui.available_height() - 30.0) / 3.0).clamp(110.0, 220.0);
            ui::chart(ui, "CPU", &h.iter().map(|x| x.0).collect::<Vec<_>>(), 10.0, RED, "%", 1, ch);
            ui.add_space(6.0);
            ui::chart(ui, "RAM", &h.iter().map(|x| x.1).collect::<Vec<_>>(), xmx_mb, BLUE, "МБ", 0, ch);
            ui.add_space(6.0);
            ui::chart(ui, "Игроки", &h.iter().map(|x| x.2).collect::<Vec<_>>(), 5.0, GREEN, "", 0, ch);
            ui.weak("Графики за последние 5 минут, наведите мышь для значений");
        }));
        ui.advance_cursor_after_rect(full);
    }

    fn player_menu(&mut self, ui: &mut Ui, n: &str) {
        let n = n.to_string();
        if ui.button("✉ Написать сообщение…").clicked() {
            self.prompt("Сообщение", &format!("Сообщение для {}:", n), "", None, PromptAction::Tell(n.clone()));
            ui.close_menu();
        }
        if ui.button("👢 Кикнуть…").clicked() {
            self.prompt("Кик", &format!("Причина кика {}:", n), "", None, PromptAction::Kick(n.clone()));
            ui.close_menu();
        }
        if ui.button("⛔ Забанить…").clicked() {
            self.prompt("Бан", &format!("Причина бана {}:", n), "", None, PromptAction::Ban(n.clone()));
            ui.close_menu();
        }
        ui.separator();
        for (label, cmd) in [("⭐ Сделать оператором", "op {}"), ("Снять оператора", "deop {}"), ("📋 В белый список", "whitelist add {}")] {
            if ui.button(label).clicked() {
                self.send_cmd(&cmd.replace("{}", &n));
                self.pending_list_reload = Some(Instant::now() + Duration::from_millis(1500));
                ui.close_menu();
            }
        }
        ui.menu_button("🎮 Режим игры", |ui| {
            for (m, l) in [("survival", "Выживание"), ("creative", "Творческий"), ("adventure", "Приключение"), ("spectator", "Наблюдатель")] {
                if ui.button(l).clicked() {
                    self.send_cmd(&format!("gamemode {} {}", m, n));
                    ui.close_menu();
                }
            }
        });
    }

    // ------------------------------------------------------------ доступ
    fn tab_access(&mut self, ui: &mut Ui, s: &Server) {
        let live = self.live().is_some();
        ui.horizontal_wrapped(|ui| {
            let mut wl = self.props.as_ref().map(|p| p.get_bool("white-list", false)).unwrap_or(false);
            if ui.checkbox(&mut wl, "Белый список включён").changed() {
                if live {
                    self.send_cmd(if wl { "whitelist on" } else { "whitelist off" });
                }
                if let Some(p) = &mut self.props {
                    if p.exists {
                        p.set("white-list", if wl { "true" } else { "false" });
                        if !live {
                            let _ = p.save();
                        }
                    }
                }
            }
            ui.weak(if live { "Сервер запущен — изменения отправляются командами" } else { "Сервер выключен — изменения пишутся прямо в файлы" });
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            for k in lists::ALL {
                let n = self.lists.get(&k).map(|v| v.len()).unwrap_or(0);
                if ui.selectable_label(self.list_kind == k, format!("{} ({})", k.title(), n)).clicked() {
                    self.list_kind = k;
                    self.list_sel.clear();
                }
            }
        });
        ui.separator();
        let kind = self.list_kind;
        ui.horizontal(|ui| {
            let add_label = if kind == ListKind::BannedIps { "➕ Забанить IP…" } else if kind.is_ban() { "➕ Забанить…" } else { "➕ Добавить…" };
            if ui.button(add_label).clicked() {
                let label = if kind == ListKind::BannedIps { "IP-адрес:" } else { "Ник игрока:" };
                self.prompt(kind.title(), label, "", if kind.is_ban() { Some("Причина (можно пусто):") } else { None }, PromptAction::ListAdd(kind));
            }
            if ui.add_enabled(!self.list_sel.is_empty(), egui::Button::new("🗑 Удалить выбранные")).clicked() {
                let names: Vec<String> = self.list_sel.iter().cloned().collect();
                self.confirm("Удалить", format!("Удалить из списка «{}»:\n{}", kind.title(), names.join(", ")),
                             Action::RemoveListEntries(kind, names));
            }
            if ui.button("🔄 Обновить").clicked() {
                self.reload_lists();
            }
            if ui.button("📄 Открыть файл").clicked() {
                let p = lists::path_for(&s.path, kind);
                if !p.exists() {
                    let _ = lists::save(&s.path, kind, &[]);
                }
                open_path(&p);
            }
        });
        ui.add_space(4.0);
        let entries = self.lists.get(&kind).cloned().unwrap_or_default();
        let cols = kind.columns();
        let mut toggle = None;
        let mut table = TableBuilder::new(ui).striped(true).sense(egui::Sense::click()).cell_layout(egui::Layout::left_to_right(egui::Align::Center));
        for (i, _) in cols.iter().enumerate() {
            let w = match cols[i].0 { "uuid" => 300.0, "reason" => 260.0, "created" => 190.0, _ => 160.0 };
            table = table.column(if i + 1 == cols.len() { Column::remainder() } else { Column::initial(w).at_least(80.0).resizable(true) });
        }
        table
            .header(24.0, |mut h| {
                for (_, t) in cols {
                    h.col(|ui| {
                        ui.strong(*t);
                    });
                }
            })
            .body(|body| {
                body.rows(24.0, entries.len(), |mut row| {
                    let e = &entries[row.index()];
                    let key = lists::field(e, kind.key());
                    row.set_selected(self.list_sel.contains(&key));
                    for (f, _) in cols {
                        row.col(|ui| {
                            ui.add(egui::Label::new(lists::field(e, f)).truncate());
                        });
                    }
                    if row.response().clicked() {
                        toggle = Some(key);
                    }
                });
            });
        if let Some(k) = toggle {
            if !self.list_sel.remove(&k) {
                self.list_sel.insert(k);
            }
        }
        if entries.is_empty() {
            ui.weak("Список пуст");
        }
    }

    // ------------------------------------------------------------ server.properties
    fn tab_props(&mut self, ui: &mut Ui) {
        let Some(exists) = self.props.as_ref().map(|p| p.exists) else { return };
        if !exists {
            ui.add_space(20.0);
            ui.label(RichText::new("Файл server.properties ещё не создан — запустите сервер один раз.").size(16.0).weak());
            return;
        }
        let live = self.live().is_some();
        ui.horizontal_wrapped(|ui| {
            ui.label("🔍");
            ui.add(egui::TextEdit::singleline(&mut self.props_filter).hint_text("поиск").desired_width(180.0));
            ui.checkbox(&mut self.props_only_known, "Только основные");
            let save = egui::Button::new(RichText::new("💾 Сохранить").strong());
            if ui.add_enabled(self.props_dirty, save).clicked() {
                self.save_props();
            }
            if ui.button("Отменить изменения").clicked() {
                self.load_props();
            }
            if ui.button("➕ Параметр…").clicked() {
                self.prompt("Новый параметр", "Имя параметра:", "", Some("Значение:"), PromptAction::AddProp);
            }
            if ui.button("📝 Открыть в блокноте").clicked() {
                if let Some(p) = &self.props {
                    open_path(&p.path);
                }
            }
            if self.props_dirty {
                ui.label(RichText::new("● есть несохранённые изменения").color(AMBER));
            } else if live {
                ui.weak("Изменения применяются после перезапуска сервера");
            }
        });
        ui.add_space(4.0);
        let f = self.props_filter.to_lowercase();
        let items: Vec<(String, String)> = self
            .props
            .as_ref()
            .unwrap()
            .items()
            .filter(|(k, v)| {
                let desc = props::known(k).map(|x| x.0).unwrap_or("");
                (!self.props_only_known || props::known(k).is_some())
                    && (f.is_empty() || k.to_lowercase().contains(&f) || v.to_lowercase().contains(&f) || desc.to_lowercase().contains(&f))
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let mut changes: Vec<(String, String)> = vec![];
        TableBuilder::new(ui)
            .striped(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(230.0).resizable(true))
            .column(Column::exact(260.0).resizable(true))
            .column(Column::remainder())
            .header(24.0, |mut h| {
                h.col(|ui| { ui.strong("Параметр"); });
                h.col(|ui| { ui.strong("Значение"); });
                h.col(|ui| { ui.strong("Описание"); });
            })
            .body(|body| {
                body.rows(30.0, items.len(), |mut row| {
                    let (k, v) = &items[row.index()];
                    let (desc, kind) = props::known(k).unwrap_or(("", Kind::Str));
                    row.col(|ui| {
                        ui.add(egui::Label::new(RichText::new(k).monospace()).truncate());
                    });
                    row.col(|ui| {
                        let mut val = v.clone();
                        match kind {
                            Kind::Bool => {
                                let mut b = val.eq_ignore_ascii_case("true");
                                let label = if b { "да" } else { "нет" };
                                if ui.checkbox(&mut b, label).changed() {
                                    val = if b { "true".into() } else { "false".into() };
                                }
                            }
                            Kind::Choice(opts) => {
                                egui::ComboBox::from_id_salt(k).selected_text(&val).width(200.0).show_ui(ui, |ui| {
                                    for o in opts {
                                        ui.selectable_value(&mut val, o.to_string(), *o);
                                    }
                                });
                            }
                            Kind::Int => {
                                let r = ui.add(egui::TextEdit::singleline(&mut val).desired_width(120.0));
                                if !val.trim().is_empty() && val.trim().parse::<i64>().is_err() {
                                    r.on_hover_text("Нужно целое число");
                                    ui.colored_label(RED, "⚠");
                                }
                            }
                            Kind::Str => {
                                ui.add(egui::TextEdit::singleline(&mut val).desired_width(f32::INFINITY));
                            }
                        }
                        if &val != v {
                            changes.push((k.clone(), val));
                        }
                    });
                    row.col(|ui| {
                        ui.add(egui::Label::new(RichText::new(desc).weak()).truncate());
                    });
                });
            });
        if !changes.is_empty() {
            if let Some(p) = &mut self.props {
                for (k, v) in changes {
                    p.set(&k, &v);
                }
            }
            self.props_dirty = true;
        }
    }

    // ------------------------------------------------------------ мир
    fn tab_world(&mut self, ui: &mut Ui, s: &Server) {
        let level = props::level_name(&s.path);
        let dirs = backup::world_dirs(&s.path);
        ui::card_frame(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(format!("🌍 Мир: {}", level)).size(17.0).strong());
                ui.weak(if dirs.is_empty() { "ещё не создан".to_string() } else { dirs.join(", ") });
                if let Some(sz) = self.world_size.filter(|_| !dirs.is_empty()) {
                    ui.weak(format!("· {}", backup::human_size(sz)));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("📁 Папка мира").clicked() {
                        open_path(&Path::new(&s.path).join(&level));
                    }
                    if ui.button("Сменить мир (level-name)…").clicked() {
                        let found: Vec<String> = std::fs::read_dir(&s.path).map(|rd| rd.filter_map(|e| e.ok())
                            .filter(|e| e.path().join("level.dat").exists())
                            .map(|e| e.file_name().to_string_lossy().to_string()).collect()).unwrap_or_default();
                        self.prompt("Мир", &format!("Папка мира (level-name). Найдены: {}\nНовое имя — сервер создаст новый мир.",
                            if found.is_empty() { "нет".into() } else { found.join(", ") }), &level, None, PromptAction::LevelName);
                    }
                });
            });
        });
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            if ui.button("💾 Создать бэкап").clicked() {
                self.backup_now(&s.id, "");
            }
            if ui.button("🏷 Бэкап с пометкой…").clicked() {
                self.prompt("Бэкап", "Пометка (например, «перед_обновлением»).\nДобавьте слово keep, чтобы бэкап не удалялся автоматически:",
                            "", None, PromptAction::BackupLabel);
            }
            if ui.button("📦 Экспорт мира в zip…").clicked() {
                self.export_world();
            }
            if ui.button("📑 Копировать мир между серверами…").clicked() {
                if self.cfg.servers.len() < 2 {
                    self.info("Копирование", "Добавьте ещё хотя бы один сервер.");
                } else {
                    self.dialog = Some(Dialog::CopyWorld { other: 0, to_other: true });
                }
            }
            if ui.button("📥 Импорт мира из папки…").clicked() && self.require_stopped(&s.id, "импортировать мир") {
                let start = dirs::config_dir().map(|d| d.join(".minecraft").join("saves")).filter(|d| d.is_dir());
                let mut dlg = rfd::FileDialog::new().set_title("Папка мира (с файлом level.dat)");
                if let Some(st) = start {
                    dlg = dlg.set_directory(st);
                }
                if let Some(folder) = dlg.pick_folder() {
                    self.confirm("Импорт мира", format!("Заменить мир сервера «{}» миром из\n{}?\n\nТекущий мир будет сохранён в бэкап.",
                        s.name, folder.display()), Action::ImportWorld(folder));
                }
            }
        });
        ui.add_space(6.0);
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Бэкапы").size(16.0).strong());
            ui.label("· папка:");
            let dir = backup::backup_dir(s);
            if ui.link(dir.display().to_string()).on_hover_text("Открыть").clicked() {
                let _ = std::fs::create_dir_all(&dir);
                open_path(&dir);
            }
            if s.backup_dir.is_empty() {
                ui.weak("(по умолчанию)");
            }
        });
        ui.horizontal_wrapped(|ui| {
            if ui.button("📂 Выбрать папку…").clicked() {
                let dir = backup::backup_dir(s);
                let mut d = rfd::FileDialog::new().set_title("Папка для бэкапов");
                d = d.set_directory(if dir.is_dir() { dir } else { PathBuf::from(&s.path) });
                if let Some(p) = d.pick_folder() {
                    self.set_backup_dir(p.to_string_lossy().to_string());
                }
            }
            if ui.button("🆕 Создать новую папку…").clicked() {
                if let Some(parent) = rfd::FileDialog::new().set_title("Где создать папку для бэкапов?").pick_folder() {
                    let name = format!("Бэкапы {}", s.name.replace(['\\', '/', ':', '*', '?', '"', '<', '>', '|'], "_"));
                    self.prompt("Новая папка", "Имя новой папки:", &name, None, PromptAction::NewBackupDir(parent));
                }
            }
            if ui.add_enabled(!s.backup_dir.is_empty(), egui::Button::new("По умолчанию")).clicked() {
                self.set_backup_dir(String::new());
            }
            ui.separator();
            let one = self.backup_sel.len() == 1;
            if ui.add_enabled(one, egui::Button::new("♻ Восстановить")).clicked() && self.require_stopped(&s.id, "восстановить мир") {
                let zip = self.backup_sel.iter().next().cloned().unwrap();
                self.confirm("Восстановление", format!("Восстановить мир из «{}»?\n\nТекущий мир будет заменён. Перед этим автоматически создастся его бэкап.",
                    zip.file_name().unwrap_or_default().to_string_lossy()), Action::Restore(zip));
            }
            if ui.add_enabled(!self.backup_sel.is_empty(), egui::Button::new("🗑 Удалить")).clicked() {
                let v: Vec<PathBuf> = self.backup_sel.iter().cloned().collect();
                self.confirm("Удалить бэкапы", format!("Удалить выбранные бэкапы ({} шт.) с диска?", v.len()), Action::DeleteBackups(v));
            }
            if ui.button("🔄").on_hover_text("Обновить").clicked() {
                self.refresh_backups();
            }
        });
        ui.weak(format!("Хранится последних: {} (меняется в «✏ Изменить»), бэкапы со словом keep не удаляются. Автобэкап: {}.",
            if s.keep_backups == 0 { "все".to_string() } else { s.keep_backups.to_string() },
            if s.auto_backup_min == 0 { "выключен".to_string() } else { format!("каждые {} мин", s.auto_backup_min) }));
        ui.add_space(4.0);
        let list = self.backups.clone();
        let mut toggle = None;
        TableBuilder::new(ui)
            .striped(true)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::remainder().at_least(250.0))
            .column(Column::exact(160.0))
            .column(Column::exact(90.0))
            .header(24.0, |mut h| {
                h.col(|ui| { ui.strong("Файл"); });
                h.col(|ui| { ui.strong("Дата"); });
                h.col(|ui| { ui.strong("Размер"); });
            })
            .body(|body| {
                body.rows(24.0, list.len(), |mut row| {
                    let b = &list[row.index()];
                    row.set_selected(self.backup_sel.contains(&b.path));
                    row.col(|ui| { ui.add(egui::Label::new(&b.name).truncate()); });
                    row.col(|ui| {
                        let dt: chrono::DateTime<chrono::Local> = b.mtime.into();
                        ui.label(dt.format("%d.%m.%Y %H:%M:%S").to_string());
                    });
                    row.col(|ui| { ui.label(backup::human_size(b.size)); });
                    if row.response().clicked() {
                        toggle = Some(b.path.clone());
                    }
                });
            });
        if let Some(p) = toggle {
            if !self.backup_sel.remove(&p) {
                self.backup_sel.insert(p);
            }
        }
        if list.is_empty() {
            ui.weak("Бэкапов пока нет");
        }
    }

    // ------------------------------------------------------------ моды
    fn tab_mods(&mut self, ui: &mut Ui, s: &Server) {
        let dirs = mods::mod_dirs(&s.path);
        ui.horizontal_wrapped(|ui| {
            ui.label("🔍");
            ui.add(egui::TextEdit::singleline(&mut self.mods_filter).hint_text("поиск").desired_width(180.0));
            let on = self.mods.iter().filter(|m| m.enabled).count();
            ui.weak(format!("Всего: {}, включено: {}, выключено: {}", self.mods.len(), on, self.mods.len() - on));
            if self.mods_loading {
                ui.spinner();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("🔄 Обновить").clicked() {
                    self.refresh_mods();
                }
                if ui.button("📁 Открыть папку").clicked() {
                    let p = Path::new(&s.path).join(dirs.first().copied().unwrap_or("mods"));
                    let _ = std::fs::create_dir_all(&p);
                    open_path(&p);
                }
            });
        });
        ui.horizontal_wrapped(|ui| {
            let has_sel = !self.mods_sel.is_empty();
            if ui.add_enabled(has_sel, egui::Button::new("Вкл./выкл. выбранные")).clicked() {
                let sel = self.mods_sel.clone();
                self.set_mods_enabled(&sel, None);
            }
            if ui.button("✔ Включить все").clicked() {
                self.confirm("Моды", "Включить все моды?", Action::SetAllMods(true));
            }
            if ui.button("✖ Выключить все").clicked() {
                self.confirm("Моды", "Выключить все моды?", Action::SetAllMods(false));
            }
            if ui.button("➕ Добавить моды…").clicked() {
                if let Some(files) = rfd::FileDialog::new().add_filter("Моды", &["jar"]).set_title("Выберите моды (.jar)").pick_files() {
                    let folder = if dirs.contains(&"plugins") && !dirs.contains(&"mods") { "plugins" } else { "mods" }.to_string();
                    let path = s.path.clone();
                    let sid = s.id.clone();
                    self.run_bg(Some("Копирую моды…".into()), move || {
                        mods::add_mods(&path, &files, &folder).map_err(|e| e.to_string())?;
                        Ok(TaskResult::Mods { sid, mods: mods::list_mods(&path) })
                    });
                }
            }
            if ui.add_enabled(has_sel, egui::Button::new("🗑 Удалить")).clicked() {
                let v: Vec<PathBuf> = self.mods_sel.iter().cloned().collect();
                let names: Vec<String> = self.mods.iter().filter(|m| self.mods_sel.contains(&m.path)).map(|m| m.file.clone()).take(15).collect();
                self.confirm("Удалить моды", format!("Удалить с диска {} файл(ов)?\n{}", v.len(), names.join("\n")), Action::DeleteMods(v));
            }
        });
        if dirs.is_empty() {
            ui.add_space(20.0);
            ui.weak("У этого сервера нет папки mods или plugins. «➕ Добавить моды» создаст папку mods.");
            return;
        }
        ui.weak("Галочка или двойной клик — включить/выключить. Выключенный мод переименовывается в .jar.disabled, как в Prism Launcher. Изменения применяются после перезапуска сервера.");
        ui.add_space(4.0);
        let f = self.mods_filter.to_lowercase();
        let list: Vec<Mod> = self.mods.iter().filter(|m| f.is_empty() || m.name.to_lowercase().contains(&f) || m.file.to_lowercase().contains(&f)).cloned().collect();
        let mut toggle_sel = None;
        let mut toggle_en = None;
        TableBuilder::new(ui)
            .striped(true)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(36.0))
            .column(Column::initial(240.0).at_least(120.0).resizable(true))
            .column(Column::initial(130.0).resizable(true))
            .column(Column::initial(80.0))
            .column(Column::remainder().at_least(150.0))
            .column(Column::exact(80.0))
            .header(24.0, |mut h| {
                for t in ["Вкл.", "Мод", "Версия", "Тип", "Файл", "Размер"] {
                    h.col(|ui| { ui.strong(t); });
                }
            })
            .body(|body| {
                body.rows(26.0, list.len(), |mut row| {
                    let m = &list[row.index()];
                    row.set_selected(self.mods_sel.contains(&m.path));
                    row.col(|ui| {
                        let mut en = m.enabled;
                        if ui.checkbox(&mut en, "").on_hover_text("Включить / выключить").changed() {
                            toggle_en = Some(m.path.clone());
                        }
                    });
                    let weak = !m.enabled;
                    let txt = |t: &str| if weak { RichText::new(t).weak().strikethrough() } else { RichText::new(t) };
                    row.col(|ui| {
                        let r = ui.add(egui::Label::new(txt(&m.name).strong()).truncate());
                        if !m.desc.is_empty() {
                            r.on_hover_text(&m.desc);
                        }
                    });
                    row.col(|ui| { ui.add(egui::Label::new(txt(&m.version)).truncate()); });
                    row.col(|ui| { ui.label(txt(if m.loader.is_empty() { &m.folder } else { &m.loader })); });
                    row.col(|ui| { ui.add(egui::Label::new(txt(&format!("{}/{}", m.folder, m.file)).small()).truncate()); });
                    row.col(|ui| { ui.label(txt(&backup::human_size(m.size))); });
                    let r = row.response();
                    if r.double_clicked() {
                        toggle_en = Some(m.path.clone());
                    } else if r.clicked() {
                        toggle_sel = Some(m.path.clone());
                    }
                });
            });
        if let Some(p) = toggle_en {
            let one: BTreeSet<PathBuf> = [p].into_iter().collect();
            let keep_sel = self.mods_sel.clone();
            self.set_mods_enabled(&one, None);
            self.mods_sel = keep_sel.into_iter().filter(|x| x.exists()).collect();
        } else if let Some(p) = toggle_sel {
            if !self.mods_sel.remove(&p) {
                self.mods_sel.insert(p);
            }
        }
    }

    // ------------------------------------------------------------ настройки
    fn tab_settings(&mut self, ui: &mut Ui) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui::card_frame(ui).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("🌐 SSH-туннель").size(16.0).strong());
                ui.horizontal(|ui| {
                    ui.label("Команда:");
                    ui.add(egui::TextEdit::singleline(&mut self.cfg.tunnel.command).font(egui::TextStyle::Monospace).desired_width(ui.available_width() - 130.0));
                    if ui.button("По умолчанию").clicked() {
                        self.cfg.tunnel.command = DEFAULT_TUNNEL.into();
                    }
                });
                ui.checkbox(&mut self.cfg.tunnel.new_window, "Открывать туннель в отдельном окне (нужно, если ssh спрашивает пароль)");
                ui.checkbox(&mut self.cfg.tunnel.auto_reconnect, "Переподключаться автоматически при обрыве");
                ui.checkbox(&mut self.cfg.tunnel.start_with_server, "Открывать доступ автоматически при запуске сервера");
                ui.checkbox(&mut self.cfg.tunnel.stop_with_server, "Закрывать доступ при остановке сервера");
                ui.add_space(4.0);
                ui.weak("Чтобы туннель открывался без пароля, один раз выполните в консоли:\n  ssh-keygen -t ed25519\n  type %USERPROFILE%\\.ssh\\id_ed25519.pub | ssh minecraft@ex1.ru \"mkdir -p ~/.ssh && cat >> ~/.ssh/authorized_keys\"\nПосле этого можно снять галочку «в отдельном окне».");
            });
            ui.add_space(8.0);
            ui::card_frame(ui).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("🔧 Общие").size(16.0).strong());
                ui.horizontal(|ui| {
                    ui.label("Ждать корректной остановки сервера:");
                    ui.add(egui::DragValue::new(&mut self.cfg.stop_timeout).range(10..=600).suffix(" с"));
                    ui.weak("потом процесс завершается принудительно");
                });
                if ui.checkbox(&mut self.cfg.dark_theme, "Тёмная тема").changed() {
                    ui::apply_theme(ui.ctx(), self.cfg.dark_theme);
                }
            });
            ui.add_space(8.0);
            if ui.button(RichText::new("💾 Сохранить настройки").strong()).clicked() {
                self.tunnel.set_settings(self.cfg.tunnel.clone());
                self.save_cfg();
                self.status_msg = format!("Настройки сохранены{}",
                    if self.tunnel.state() == TunnelState::Open { " — перезапустите туннель, чтобы применить" } else { "" });
            }
            ui.add_space(8.0);
            ui.label(RichText::new("Журнал туннеля").strong());
            let (counter, lines) = self.tunnel.log_lines();
            egui::Frame::new().fill(ui.visuals().extreme_bg_color).corner_radius(6).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
                egui::ScrollArea::vertical().id_salt("tunlog").max_height(220.0).auto_shrink([false, true]).stick_to_bottom(true).show(ui, |ui| {
                    for l in &lines {
                        ui.label(RichText::new(l).monospace().small());
                    }
                    if lines.is_empty() {
                        ui.weak("пусто");
                    }
                });
            });
            self.tunnel_log_counter = counter;
            ui.weak(format!("Настройки хранятся в: {}", crate::config::config_path().display()));
        });
    }

    // ------------------------------------------------------------ диалоги
    fn dialogs(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.dialog.take() else { return };
        let mut keep = true;
        let mut confirmed = false;
        let mut dialog = dialog;
        let modal = egui::Modal::new(egui::Id::new("dialog")).show(ctx, |ui| {
            ui.set_max_width(520.0);
            match &mut dialog {
                Dialog::Info { title, text } => {
                    ui.heading(title.as_str());
                    ui.add_space(6.0);
                    ui.label(text.as_str());
                    ui.add_space(10.0);
                    if ui.button("  OK  ").clicked() {
                        keep = false;
                    }
                }
                Dialog::Confirm { title, text, .. } => {
                    ui.heading(title.as_str());
                    ui.add_space(6.0);
                    ui.label(text.as_str());
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new("  Да  ").strong()).clicked() {
                            keep = false;
                            confirmed = true;
                        }
                        if ui.button("Отмена").clicked() {
                            keep = false;
                        }
                    });
                }
                Dialog::Prompt { title, label, value, label2, value2, .. } => {
                    ui.heading(title.as_str());
                    ui.add_space(6.0);
                    ui.label(label.as_str());
                    let r = ui.add(egui::TextEdit::singleline(value).desired_width(460.0));
                    if !r.has_focus() && value.is_empty() {
                        r.request_focus();
                    }
                    if let Some(l2) = label2 {
                        ui.label(l2.as_str());
                        ui.add(egui::TextEdit::singleline(value2).desired_width(460.0));
                    }
                    ui.add_space(10.0);
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new("  OK  ").strong()).clicked() || enter {
                            keep = false;
                            confirmed = true;
                        }
                        if ui.button("Отмена").clicked() {
                            keep = false;
                        }
                    });
                }
                Dialog::CopyWorld { other, to_other } => {
                    ui.heading("Копирование мира");
                    let cur = self.cur();
                    let others: Vec<Server> = self.cfg.servers.iter().filter(|s| Some(&s.id) != self.current.as_ref()).cloned().collect();
                    let name = cur.as_ref().map(|s| s.name.clone()).unwrap_or_default();
                    ui.radio_value(to_other, true, format!("Из «{}» в другой сервер", name));
                    ui.radio_value(to_other, false, format!("Из другого сервера в «{}»", name));
                    ui.add_space(4.0);
                    egui::ComboBox::from_label("Другой сервер")
                        .selected_text(others.get(*other).map(|s| s.name.clone()).unwrap_or_default())
                        .show_ui(ui, |ui| {
                            for (i, s) in others.iter().enumerate() {
                                ui.selectable_value(other, i, &s.name);
                            }
                        });
                    ui.add_space(4.0);
                    ui.colored_label(AMBER, "Мир получателя будет заменён (перед этим создаётся его бэкап).\nМир новой версии нельзя открыть на старой версии сервера.");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new("Копировать").strong()).clicked() {
                            keep = false;
                            if let (Some(c), Some(o)) = (cur.clone(), others.get(*other).cloned()) {
                                let (src, dst) = if *to_other { (c, o) } else { (o, c) };
                                self.copy_world(src, dst);
                            }
                        }
                        if ui.button("Отмена").clicked() {
                            keep = false;
                        }
                    });
                }
                Dialog::CloseApp => {
                    let name = self.running_proc().and_then(|p| self.cfg.server(&p.sid).map(|s| s.name.clone())).unwrap_or_default();
                    ui.heading("Выход");
                    ui.label(format!("Сервер «{}» запущен.", name));
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new("Остановить сервер и выйти").strong()).clicked() {
                            keep = false;
                            if let Some(p) = self.running_proc() {
                                p.restart_after_stop.store(false, std::sync::atomic::Ordering::SeqCst);
                                p.stop(self.stop_timeout());
                            }
                            self.closing = Some(Instant::now());
                            self.status_msg = "Останавливаю сервер перед выходом…".into();
                        }
                        if ui.button("Выйти, оставив сервер работать").clicked() {
                            keep = false;
                            self.allow_close = true;
                            self.save_cfg();
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        if ui.button("Отмена").clicked() {
                            keep = false;
                        }
                    });
                }
            }
        });
        if modal.should_close() && !matches!(dialog, Dialog::CloseApp) {
            keep = false;
        }
        if keep {
            self.dialog = Some(dialog);
            return;
        }
        match dialog {
            Dialog::Confirm { action, .. } if confirmed => self.do_action(action),
            Dialog::Prompt { action, value, value2, .. } if confirmed => self.do_prompt(action, value, value2),
            _ => {}
        }
    }

    fn copy_world(&mut self, src: Server, dst: Server) {
        if !self.require_stopped(&dst.id, "заменить мир") {
            return;
        }
        if backup::world_dirs(&src.path).is_empty() {
            return self.info("Копирование", format!("В «{}» нет мира.", src.name));
        }
        let p = self.procs.get(&src.id).cloned();
        let label = format!("Копирую мир «{}» → «{}»…", src.name, dst.name);
        self.run_bg(Some(label), move || {
            if !backup::world_dirs(&dst.path).is_empty() {
                backup::make_backup(&dst, "before-copy", None).map_err(|e| e.to_string())?;
            }
            let flushed = p.as_ref().map(|p| p.running() && flush_world(p)).unwrap_or(false);
            let r = backup::copy_world(&src, &dst);
            if flushed {
                let _ = p.unwrap().send("save-on");
            }
            r.map_err(|e| e.to_string())?;
            Ok(TaskResult::WorldChanged { sid: dst.id.clone(), msg: format!("Мир скопирован в «{}»", dst.name) })
        });
    }

    // ------------------------------------------------------------ редактор сервера
    fn new_server(&mut self) {
        let mut srv = Server::default();
        if let Some(dir) = rfd::FileDialog::new().set_title("Папка сервера").pick_folder() {
            srv.path = dir.to_string_lossy().to_string();
            srv.name = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or(srv.name);
            let (mode, jar, args) = launch::detect_launch(&srv.path);
            srv.launch_mode = mode;
            srv.jar = jar;
            srv.custom_args = args;
        } else {
            return;
        }
        let jars = launch::list_jars(&srv.path);
        self.edit = Some(ServerEdit { srv, is_new: true, jars, javas: vec!["java".into()], error: None });
    }

    fn open_editor(&mut self, sid: &str) {
        if let Some(s) = self.cfg.server(sid).cloned() {
            let jars = launch::list_jars(&s.path);
            let javas = vec![s.java.clone()];
            self.edit = Some(ServerEdit { srv: s, is_new: false, jars, javas, error: None });
        }
    }

    fn server_editor(&mut self, ctx: &egui::Context) {
        let Some(mut ed) = self.edit.take() else { return };
        let mut keep = true;
        let mut save = false;
        let mut scan_java = false;
        let modal = egui::Modal::new(egui::Id::new("server_edit")).show(ctx, |ui| {
            ui.set_width(640.0);
            ui.heading(if ed.is_new { "Новый сервер" } else { "Настройки сервера" });
            ui.add_space(6.0);
            let s = &mut ed.srv;
            egui::Grid::new("srv_grid").num_columns(2).spacing([12.0, 8.0]).min_col_width(140.0).show(ui, |ui| {
                ui.label("Название:");
                ui.add(egui::TextEdit::singleline(&mut s.name).desired_width(460.0));
                ui.end_row();
                ui.label("Папка сервера:");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut s.path).desired_width(370.0));
                    if ui.button("Обзор…").clicked() {
                        if let Some(d) = rfd::FileDialog::new().set_directory(&s.path).pick_folder() {
                            s.path = d.to_string_lossy().to_string();
                            ed.jars = launch::list_jars(&s.path);
                        }
                    }
                });
                ui.end_row();
                ui.label("Запуск:");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut s.launch_mode, "jar".to_string(), "jar-файл");
                    ui.radio_value(&mut s.launch_mode, "args".to_string(), "свои аргументы (Forge 1.17+)");
                    if ui.button("Определить").on_hover_text("Определить автоматически").clicked() {
                        let (m, j, a) = launch::detect_launch(&s.path);
                        s.launch_mode = m;
                        if !j.is_empty() {
                            s.jar = j;
                        }
                        if !a.is_empty() {
                            s.custom_args = a;
                        }
                        ed.jars = launch::list_jars(&s.path);
                    }
                });
                ui.end_row();
                if s.launch_mode == "jar" {
                    ui.label("Jar-файл:");
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("jar").width(370.0).selected_text(&s.jar).show_ui(ui, |ui| {
                            for j in &ed.jars {
                                ui.selectable_value(&mut s.jar, j.clone(), j);
                            }
                        });
                        if ui.button("Обзор…").clicked() {
                            if let Some(f) = rfd::FileDialog::new().set_directory(&s.path).add_filter("jar", &["jar"]).pick_file() {
                                s.jar = f.strip_prefix(&s.path).map(|p| p.to_string_lossy().to_string()).unwrap_or(f.to_string_lossy().to_string());
                            }
                        }
                    });
                } else {
                    ui.label("Аргументы:");
                    ui.add(egui::TextEdit::singleline(&mut s.custom_args).desired_width(460.0)
                        .hint_text("@user_jvm_args.txt @libraries/net/minecraftforge/forge/…/win_args.txt"));
                }
                ui.end_row();
                ui.label("Java:");
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("java").width(300.0).selected_text(java_label(&s.java)).show_ui(ui, |ui| {
                        for j in &ed.javas {
                            ui.selectable_value(&mut s.java, j.clone(), j);
                        }
                    });
                    if ui.button("Найти").clicked() {
                        scan_java = true;
                    }
                    if ui.button("Обзор…").clicked() {
                        if let Some(f) = rfd::FileDialog::new().set_title("java.exe").pick_file() {
                            s.java = f.to_string_lossy().to_string();
                        }
                    }
                });
                ui.end_row();
                ui.label("");
                ui.weak("MC 1.20.5+ → Java 21 · 1.18–1.20.4 → Java 17 · 1.17 → Java 16 · ≤1.16 → Java 8");
                ui.end_row();
                ui.label("Память (RAM):");
                ui.horizontal(|ui| {
                    ui.label("мин.");
                    ui.add(egui::TextEdit::singleline(&mut s.xms).desired_width(60.0));
                    ui.label("макс.");
                    egui::ComboBox::from_id_salt("xmx").width(70.0).selected_text(&s.xmx).show_ui(ui, |ui| {
                        for m in ["1G", "2G", "3G", "4G", "6G", "8G", "10G", "12G", "16G"] {
                            ui.selectable_value(&mut s.xmx, m.to_string(), m);
                        }
                    });
                });
                ui.end_row();
                ui.label("Доп. аргументы JVM:");
                ui.add(egui::TextEdit::singleline(&mut s.jvm_args).desired_width(460.0));
                ui.end_row();
                ui.label("Аргументы сервера:");
                ui.add(egui::TextEdit::singleline(&mut s.server_args).desired_width(460.0).hint_text("nogui"));
                ui.end_row();
                ui.label("Папка бэкапов:");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut s.backup_dir).desired_width(370.0).hint_text("по умолчанию: <папка сервера>/backups"));
                    if ui.button("Обзор…").clicked() {
                        if let Some(d) = rfd::FileDialog::new().pick_folder() {
                            s.backup_dir = d.to_string_lossy().to_string();
                        }
                    }
                });
                ui.end_row();
                ui.label("Бэкапы:");
                ui.horizontal(|ui| {
                    ui.label("хранить");
                    ui.add(egui::DragValue::new(&mut s.keep_backups).range(0..=999));
                    ui.label("(0 = все), автобэкап каждые");
                    ui.add(egui::DragValue::new(&mut s.auto_backup_min).range(0..=1440));
                    ui.label("мин.");
                });
                ui.end_row();
                ui.label("");
                ui.checkbox(&mut s.backup_on_stop, "Бэкап после каждой остановки сервера");
                ui.end_row();
            });
            if let Some(e) = &ed.error {
                ui.colored_label(RED, e);
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button(RichText::new("💾 Сохранить").strong()).clicked() {
                    save = true;
                }
                if ui.button("Отмена").clicked() {
                    keep = false;
                }
            });
        });
        if modal.should_close() {
            keep = false;
        }
        if scan_java {
            ed.javas = launch::find_java();
            if ed.javas.len() == 1 {
                ed.error = Some("Отдельно установленные Java не найдены, будет использоваться «java» из PATH.".into());
            }
        }
        if save {
            let s = &ed.srv;
            if s.name.trim().is_empty() {
                ed.error = Some("Укажите название".into());
            } else if !Path::new(&s.path).is_dir() {
                ed.error = Some("Папка сервера не существует".into());
            } else if s.launch_mode == "jar" && s.jar.trim().is_empty() {
                ed.error = Some("Выберите jar-файл".into());
            } else {
                let srv = ed.srv.clone();
                let id = srv.id.clone();
                if let Some(existing) = self.cfg.server_mut(&id) {
                    *existing = srv;
                } else {
                    self.cfg.servers.push(srv);
                }
                self.select(Some(id));
                self.save_cfg();
                keep = false;
            }
        }
        if keep {
            self.edit = Some(ed);
        }
    }
}

fn status_color(st: Status) -> Color32 {
    match st {
        Status::Running => GREEN,
        Status::Starting | Status::Stopping => AMBER,
        Status::Stopped => GRAY,
    }
}

fn pill(ui: &mut Ui, text: &str, color: Color32) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.2))
        .stroke(egui::Stroke::new(1.0_f32, color))
        .corner_radius(12)
        .inner_margin(egui::Margin::symmetric(10, 3))
        .show(ui, |ui| {
            ui.label(RichText::new(text).color(color).strong());
        });
}

fn parse_mem_mb(s: &str) -> Option<f32> {
    let s = s.trim().to_uppercase();
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit())?);
    let n: f32 = num.parse().ok()?;
    match unit {
        "G" | "GB" => Some(n * 1024.0),
        "M" | "MB" => Some(n),
        _ => None,
    }
}
