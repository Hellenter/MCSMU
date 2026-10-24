//! Данные панели (пользователи, серверы, сессии) и всё, что работает в фоне:
//! процессы серверов, туннели, статистика, автобэкапы.

use crate::core::backup;
use crate::core::config::{Server, TunnelSettings};
use crate::core::ping;
use crate::core::props::{self, Properties};
use crate::core::server::{flush_world, ServerProcess, Status};
use crate::core::tunnel::Tunnel;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub const HISTORY_LEN: usize = 150;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct User {
    pub id: String,
    pub username: String,
    pub pass_hash: String,
    pub admin: bool,
    /// новые аккаунты ждут одобрения администратора (если регистрация не открыта)
    pub approved: bool,
    pub created: String,
    /// у каждого свой туннель для своих серверов
    pub tunnel: TunnelSettings,
}

impl Default for User {
    fn default() -> Self {
        let mut tunnel = TunnelSettings::default();
        tunnel.new_window = false;
        User {
            id: uuid::Uuid::new_v4().simple().to_string(),
            username: String::new(),
            pass_hash: String::new(),
            admin: false,
            approved: false,
            created: chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
            tunnel,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OwnedServer {
    pub owner: String,
    #[serde(flatten)]
    pub server: Server,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// true — регистрация без одобрения администратора
    pub open_registration: bool,
    /// папки, внутри которых обычные пользователи могут держать серверы; пусто — без ограничений
    pub allowed_roots: Vec<String>,
    pub stop_timeout: u64,
    /// туннель для доступа к самой панели из интернета
    pub panel_tunnel: TunnelSettings,
    pub panel_tunnel_autostart: bool,
}

impl Default for Settings {
    fn default() -> Self {
        let mut panel_tunnel = TunnelSettings::default();
        panel_tunnel.command = "ssh -R 127.0.0.1:8080:localhost:8080 minecraft@ex1.ru".into();
        panel_tunnel.new_window = false;
        Settings {
            open_registration: false,
            allowed_roots: vec![],
            stop_timeout: 60,
            panel_tunnel,
            panel_tunnel_autostart: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredSession {
    /// sha256 от токена: сам токен хранится только в cookie браузера
    pub hash: String,
    pub user: String,
    pub expires: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Data {
    pub users: Vec<User>,
    pub servers: Vec<OwnedServer>,
    pub settings: Settings,
    pub sessions: Vec<StoredSession>,
}

#[derive(Clone, Default, Serialize)]
pub struct PingJson {
    pub online: i64,
    pub max: i64,
    pub players: Vec<String>,
    pub version: String,
    pub motd: String,
    pub latency_ms: Option<u128>,
}

#[derive(Clone, Default, Serialize)]
pub struct Stats {
    pub cpu: Option<f32>,
    pub rss: Option<u64>,
    pub sys_used: u64,
    pub sys_total: u64,
    pub ping: Option<PingJson>,
}

/// (время, CPU %, RAM МБ, игроки)
pub type HistPoint = (i64, Option<f32>, Option<f32>, Option<f32>);

pub struct App {
    pub data_dir: PathBuf,
    pub port: u16,
    pub data: Mutex<Data>,
    pub procs: Mutex<HashMap<String, Arc<ServerProcess>>>,
    pub tunnels: Mutex<HashMap<String, Tunnel>>,
    pub panel_tunnel: Tunnel,
    pub stats: Mutex<HashMap<String, (Stats, VecDeque<HistPoint>)>>,
    /// что сейчас делается с сервером (бэкап, восстановление…)
    pub busy: Mutex<HashMap<String, String>>,
    /// последнее сообщение о фоновой операции: (счётчик, текст, ошибка?)
    pub notices: Mutex<HashMap<String, (u64, String, bool)>>,
    pub last_auto_backup: Mutex<HashMap<String, Instant>>,
    pub pending_start: Mutex<Vec<(String, Instant)>>,
    /// остановленный сервер -> какой запустить следом
    pub start_after: Mutex<HashMap<String, String>>,
    pub login_fails: Mutex<HashMap<String, (u32, Instant)>>,
}

impl App {
    pub fn new(data_dir: PathBuf, port: u16) -> Arc<App> {
        let _ = fs::create_dir_all(&data_dir);
        let data: Data = fs::read_to_string(data_dir.join("web.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let panel_tunnel = Tunnel::new(data.settings.panel_tunnel.clone());
        Arc::new(App {
            data_dir,
            port,
            data: Mutex::new(data),
            procs: Mutex::new(HashMap::new()),
            tunnels: Mutex::new(HashMap::new()),
            panel_tunnel,
            stats: Mutex::new(HashMap::new()),
            busy: Mutex::new(HashMap::new()),
            notices: Mutex::new(HashMap::new()),
            last_auto_backup: Mutex::new(HashMap::new()),
            pending_start: Mutex::new(vec![]),
            start_after: Mutex::new(HashMap::new()),
            login_fails: Mutex::new(HashMap::new()),
        })
    }

    pub fn server(&self, sid: &str) -> Option<OwnedServer> {
        self.data.lock().unwrap().servers.iter().find(|s| s.server.id == sid).cloned()
    }

    pub fn user(&self, uid: &str) -> Option<User> {
        self.data.lock().unwrap().users.iter().find(|u| u.id == uid).cloned()
    }

    pub fn proc(&self, sid: &str) -> Arc<ServerProcess> {
        self.procs.lock().unwrap().entry(sid.to_string()).or_insert_with(|| Arc::new(ServerProcess::new(sid))).clone()
    }

    pub fn status_of(&self, sid: &str) -> Status {
        self.procs.lock().unwrap().get(sid).map(|p| p.status()).unwrap_or(Status::Stopped)
    }

    pub fn tunnel(&self, uid: &str) -> Tunnel {
        let mut t = self.tunnels.lock().unwrap();
        if let Some(t) = t.get(uid) {
            return t.clone();
        }
        let settings = self.user(uid).map(|u| u.tunnel).unwrap_or_default();
        let tun = Tunnel::new(settings);
        t.insert(uid.to_string(), tun.clone());
        tun
    }

    pub fn stop_timeout(&self) -> Duration {
        Duration::from_secs(self.data.lock().unwrap().settings.stop_timeout.max(10))
    }

    pub fn notice(&self, sid: &str, text: impl Into<String>, error: bool) {
        let mut n = self.notices.lock().unwrap();
        let c = n.get(sid).map(|x| x.0 + 1).unwrap_or(1);
        n.insert(sid.to_string(), (c, text.into(), error));
    }

    pub fn is_busy(&self, sid: &str) -> Option<String> {
        self.busy.lock().unwrap().get(sid).cloned()
    }

    /// Выполняет долгую операцию в фоне, помечая сервер занятым.
    pub fn run_bg<F>(self: &Arc<Self>, sid: &str, label: &str, job: F) -> Result<(), String>
    where
        F: FnOnce(&Arc<App>) -> Result<String, String> + Send + 'static,
    {
        {
            let mut b = self.busy.lock().unwrap();
            if let Some(cur) = b.get(sid) {
                return Err(format!("Подождите: {}", cur));
            }
            b.insert(sid.to_string(), label.to_string());
        }
        let app = self.clone();
        let sid = sid.to_string();
        thread::spawn(move || {
            let r = job(&app);
            app.busy.lock().unwrap().remove(&sid);
            match r {
                Ok(msg) => app.notice(&sid, msg, false),
                Err(e) => app.notice(&sid, e, true),
            }
        });
        Ok(())
    }
}

pub fn save_data(dir: &Path, d: &Data) {
    let path = dir.join("web.json");
    let tmp = path.with_extension("json.tmp");
    if fs::write(&tmp, serde_json::to_string_pretty(d).unwrap_or_default()).is_ok() {
        let _ = fs::rename(tmp, path);
    }
}

pub fn eula_accepted(path: &str) -> bool {
    fs::read_to_string(Path::new(path).join("eula.txt"))
        .map(|t| t.lines().any(|l| l.trim().replace(' ', "").eq_ignore_ascii_case("eula=true")))
        .unwrap_or(false)
}

pub fn accept_eula(path: &str) -> std::io::Result<()> {
    let text = format!("# Accepted via MC Server Manager {}\neula=true\n", chrono::Local::now().format("%Y-%m-%dT%H:%M:%S"));
    fs::write(Path::new(path).join("eula.txt"), text)
}

/// Запуск без проверок EULA/конфликтов (они делаются в API).
pub fn start_now(app: &Arc<App>, sid: &str) -> Result<(), String> {
    let s = app.server(sid).ok_or("Сервер не найден")?;
    let p = app.proc(sid);
    p.start(&s.server)?;
    app.last_auto_backup.lock().unwrap().insert(sid.to_string(), Instant::now());
    app.stats.lock().unwrap().remove(sid);
    let user = app.user(&s.owner);
    if let Some(u) = user {
        if u.tunnel.start_with_server {
            let t = app.tunnel(&u.id);
            if !t.wanted() {
                let mut ts = u.tunnel.clone();
                ts.new_window = false;
                t.set_settings(ts);
                if let Err(e) = t.start() {
                    p.info(&format!("Туннель не открыт: {}", e), crate::core::server::Level::Error);
                }
            }
        }
    }
    Ok(())
}

/// Бэкап мира (с сохранением мира на диск, если сервер работает) и ротация старых.
pub fn backup_now(app: &Arc<App>, sid: &str, label: &str) -> Result<String, String> {
    let s = app.server(sid).ok_or("Сервер не найден")?.server;
    let p = app.proc(sid);
    let flushed = flush_world(&p);
    let r = backup::make_backup(&s, label, None);
    if flushed {
        let _ = p.send("save-on");
    }
    let r = r.map_err(|e| format!("Бэкап не создан: {}", e))?;
    let removed = backup::rotate(&s);
    let mut msg = format!("Бэкап создан: {}", r.path.file_name().unwrap_or_default().to_string_lossy());
    if !r.skipped.is_empty() {
        msg += &format!(" (пропущено файлов: {})", r.skipped.len());
    }
    if removed > 0 {
        msg += &format!(", удалено старых: {}", removed);
    }
    app.last_auto_backup.lock().unwrap().insert(sid.to_string(), Instant::now());
    Ok(msg)
}

fn on_exited(app: &Arc<App>, sid: &str) {
    let p = app.proc(sid);
    let restart = p.restart_after_stop.swap(false, Ordering::SeqCst);
    let next = app.start_after.lock().unwrap().remove(sid);
    let owner = app.server(sid).map(|s| s.owner);
    if restart {
        app.pending_start.lock().unwrap().push((sid.to_string(), Instant::now() + Duration::from_millis(1500)));
    } else if let Some(n) = next {
        app.pending_start.lock().unwrap().push((n, Instant::now() + Duration::from_millis(1000)));
    } else if let Some(u) = owner.as_deref().and_then(|o| app.user(o)) {
        if u.tunnel.stop_with_server {
            app.tunnel(&u.id).stop();
        }
    }
    if app.server(sid).map(|s| s.server.backup_on_stop).unwrap_or(false) {
        let _ = app.run_bg(sid, "Бэкап после остановки…", {
            let sid = sid.to_string();
            move |app| backup_now(app, &sid, "stop")
        });
    }
}

fn collect_stats(app: &Arc<App>, sys: &mut sysinfo::System) {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate};
    let targets: Vec<(String, u32, Status, String, usize)> = {
        let procs = app.procs.lock().unwrap();
        procs
            .values()
            .filter_map(|p| {
                let pid = p.pid()?;
                let path = app.server(&p.sid)?.server.path;
                Some((p.sid.clone(), pid, p.status(), path, p.players().len()))
            })
            .collect()
    };
    if targets.is_empty() {
        return;
    }
    let pids: Vec<Pid> = targets.iter().map(|t| Pid::from_u32(t.1)).collect();
    sys.refresh_processes_specifics(ProcessesToUpdate::Some(&pids), true, ProcessRefreshKind::nothing().with_cpu().with_memory());
    sys.refresh_memory();
    let cores = thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f32;
    for (sid, pid, status, path, log_players) in targets {
        let pr = sys.process(Pid::from_u32(pid));
        let ping = if status == Status::Running {
            ping::ping("127.0.0.1", props::server_port(&path), Duration::from_millis(1500)).ok().map(|x| PingJson {
                online: x.online,
                max: x.max,
                players: x.players,
                version: x.version,
                motd: x.motd,
                latency_ms: x.latency_ms,
            })
        } else {
            None
        };
        let st = Stats {
            cpu: pr.map(|p| p.cpu_usage() / cores),
            rss: pr.map(|p| p.memory()),
            sys_used: sys.used_memory(),
            sys_total: sys.total_memory(),
            ping,
        };
        let players = st.ping.as_ref().map(|p| p.online as f32).or(Some(log_players as f32));
        let point = (chrono::Local::now().timestamp(), st.cpu, st.rss.map(|r| r as f32 / 1048576.0), players);
        let mut all = app.stats.lock().unwrap();
        let e = all.entry(sid).or_default();
        if e.1.len() >= HISTORY_LEN {
            e.1.pop_front();
        }
        e.1.push_back(point);
        e.0 = st;
    }
}

/// Фоновый цикл: завершения процессов, отложенные запуски, статистика, автобэкапы.
pub fn background(app: Arc<App>) {
    let mut sys = sysinfo::System::new();
    let mut tick: u64 = 0;
    loop {
        thread::sleep(Duration::from_millis(500));
        tick += 1;
        let exited: Vec<String> = {
            let procs = app.procs.lock().unwrap();
            procs.values().filter_map(|p| p.take_exit().map(|_| p.sid.clone())).collect()
        };
        for sid in exited {
            on_exited(&app, &sid);
        }
        let due: Vec<String> = {
            let mut ps = app.pending_start.lock().unwrap();
            let now = Instant::now();
            let due = ps.iter().filter(|x| x.1 <= now).map(|x| x.0.clone()).collect();
            ps.retain(|x| x.1 > now);
            due
        };
        for sid in due {
            if let Err(e) = start_now(&app, &sid) {
                app.notice(&sid, format!("Не удалось запустить: {}", e), true);
            }
        }
        if tick % 4 == 0 {
            collect_stats(&app, &mut sys);
        }
        if tick % 60 == 0 {
            let servers: Vec<Server> = app.data.lock().unwrap().servers.iter().map(|s| s.server.clone()).collect();
            for s in servers.into_iter().filter(|s| s.auto_backup_min > 0) {
                if app.status_of(&s.id) != Status::Running {
                    continue;
                }
                let last = app.last_auto_backup.lock().unwrap().get(&s.id).copied();
                if last.map(|t| t.elapsed() >= Duration::from_secs(s.auto_backup_min as u64 * 60)).unwrap_or(true) {
                    app.last_auto_backup.lock().unwrap().insert(s.id.clone(), Instant::now());
                    let sid = s.id.clone();
                    let _ = app.run_bg(&s.id, "Автобэкап…", move |app| backup_now(app, &sid, "auto"));
                }
            }
            // протухшие сессии
            let now = chrono::Local::now().timestamp();
            let mut d = app.data.lock().unwrap();
            let before = d.sessions.len();
            d.sessions.retain(|s| s.expires > now);
            if d.sessions.len() != before {
                save_data(&app.data_dir, &d);
            }
        }
    }
}

/// Корректно останавливает все серверы (при выходе из программы).
pub fn stop_all(app: &Arc<App>) {
    let procs: Vec<Arc<ServerProcess>> = app.procs.lock().unwrap().values().filter(|p| p.running()).cloned().collect();
    let timeout = app.stop_timeout();
    for p in &procs {
        p.restart_after_stop.store(false, Ordering::SeqCst);
        p.stop(timeout);
    }
    let t0 = Instant::now();
    while procs.iter().any(|p| p.running()) && t0.elapsed() < timeout + Duration::from_secs(5) {
        thread::sleep(Duration::from_millis(200));
    }
    for t in app.tunnels.lock().unwrap().values() {
        t.stop();
    }
    app.panel_tunnel.stop();
}

/// Свойства, которые удобно показать в списке серверов.
pub fn level_and_port(path: &str) -> (String, u16) {
    let p = Properties::load(path);
    let lvl = p.get("level-name").filter(|s| !s.trim().is_empty()).unwrap_or("world").to_string();
    let port = p.get("server-port").and_then(|s| s.trim().parse().ok()).unwrap_or(25565);
    (lvl, port)
}
