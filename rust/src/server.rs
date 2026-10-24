//! Процесс сервера: запуск, остановка, чтение консоли, учёт игроков.

use crate::config::Server;
use crate::launch::build_command;
use regex::Regex;
use std::collections::{BTreeSet, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Stopped,
    Starting,
    Running,
    Stopping,
}

impl Status {
    pub fn ru(self) -> &'static str {
        match self {
            Status::Stopped => "Остановлен",
            Status::Starting => "Запускается…",
            Status::Running => "Работает",
            Status::Stopping => "Останавливается…",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Normal,
    Warn,
    Error,
    Info,
    Cmd,
}

#[derive(Clone)]
pub struct LogLine {
    pub text: String,
    pub level: Level,
}

pub const LOG_LIMIT: usize = 5000;

pub struct Shared {
    pub status: Status,
    pub log: VecDeque<LogLine>,
    /// растёт при каждой новой строке, чтобы GUI знал, что прокручивать
    pub log_counter: u64,
    pub players: BTreeSet<String>,
    listeners: Vec<(Regex, Sender<String>)>,
    pub exited: Option<Option<i32>>,
    pub started_at: Option<Instant>,
    pub pid: Option<u32>,
}

struct Patterns {
    join: Regex,
    left: Regex,
    list: Regex,
    done: Regex,
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| Patterns {
        join: Regex::new(r"(?:^|\]: )([A-Za-z0-9_.\-]{1,20}) joined the game").unwrap(),
        left: Regex::new(r"(?:^|\]: )([A-Za-z0-9_.\-]{1,20}) left the game").unwrap(),
        list: Regex::new(r"There are (\d+) (?:of a max of |/)\s*(\d+) players online:?\s*(.*)").unwrap(),
        done: Regex::new(r"Done \(\d+[.,]\d+s\)!").unwrap(),
    })
}

impl Shared {
    fn push(&mut self, text: String, level: Level) {
        if self.log.len() >= LOG_LIMIT {
            self.log.pop_front();
        }
        self.log.push_back(LogLine { text, level });
        self.log_counter += 1;
    }

    fn parse(&mut self, line: &str) {
        let p = patterns();
        if self.status == Status::Starting && p.done.is_match(line) {
            self.status = Status::Running;
        }
        if let Some(c) = p.join.captures(line) {
            self.players.insert(c[1].to_string());
        }
        if let Some(c) = p.left.captures(line) {
            self.players.remove(&c[1]);
        }
        if let Some(c) = p.list.captures(line) {
            let names: BTreeSet<String> =
                c[3].split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            if !names.is_empty() || &c[1] == "0" {
                self.players = names;
            }
        }
        self.listeners.retain(|(re, tx)| {
            if re.is_match(line) {
                let _ = tx.send(line.to_string());
                false
            } else {
                true
            }
        });
    }
}

pub struct ServerProcess {
    pub sid: String,
    pub shared: Arc<Mutex<Shared>>,
    child: Arc<Mutex<Option<Child>>>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    pub restart_after_stop: AtomicBool,
}

fn classify(line: &str) -> Level {
    if line.contains("ERROR") || line.contains("Exception") || line.contains("FATAL") {
        Level::Error
    } else if line.contains("WARN") {
        Level::Warn
    } else {
        Level::Normal
    }
}

impl ServerProcess {
    pub fn new(sid: &str) -> ServerProcess {
        ServerProcess {
            sid: sid.to_string(),
            shared: Arc::new(Mutex::new(Shared {
                status: Status::Stopped,
                log: VecDeque::new(),
                log_counter: 0,
                players: BTreeSet::new(),
                listeners: vec![],
                exited: None,
                started_at: None,
                pid: None,
            })),
            child: Arc::new(Mutex::new(None)),
            stdin: Arc::new(Mutex::new(None)),
            restart_after_stop: AtomicBool::new(false),
        }
    }

    pub fn status(&self) -> Status {
        self.shared.lock().unwrap().status
    }

    pub fn running(&self) -> bool {
        self.child.lock().unwrap().is_some()
    }

    pub fn info(&self, text: &str, level: Level) {
        self.shared.lock().unwrap().push(format!("[Менеджер] {}", text), level);
    }

    pub fn start(&self, srv: &Server) -> Result<(), String> {
        if self.running() {
            return Err("Сервер уже запущен".into());
        }
        if !std::path::Path::new(&srv.path).is_dir() {
            return Err(format!("Папка сервера не найдена:\n{}", srv.path));
        }
        let cmd = build_command(srv)?;
        self.info(&format!("Запуск: {}", cmd.join(" ")), Level::Info);
        let mut c = Command::new(&cmd[0]);
        c.args(&cmd[1..])
            .current_dir(&srv.path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        c.creation_flags(CREATE_NO_WINDOW);
        let mut child = c.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!("Не найдена Java: {}\nУкажите путь к java.exe в настройках сервера.", cmd[0])
            } else {
                format!("Не удалось запустить: {}", e)
            }
        })?;
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        *self.stdin.lock().unwrap() = child.stdin.take();
        {
            let mut sh = self.shared.lock().unwrap();
            sh.players.clear();
            sh.status = Status::Starting;
            sh.started_at = Some(Instant::now());
            sh.pid = Some(child.id());
            sh.exited = None;
        }
        *self.child.lock().unwrap() = Some(child);

        Self::spawn_reader(self.shared.clone(), stderr);
        let shared = self.shared.clone();
        let child_ref = self.child.clone();
        let stdin_ref = self.stdin.clone();
        thread::spawn(move || {
            read_lines(stdout, |line| {
                let mut sh = shared.lock().unwrap();
                let lvl = classify(&line);
                sh.parse(&line);
                sh.push(line, lvl);
            });
            // stdout закрыт: ждём завершения процесса
            let code = loop {
                let mut guard = child_ref.lock().unwrap();
                match guard.as_mut().map(|c| c.try_wait()) {
                    Some(Ok(Some(st))) => break st.code(),
                    Some(Ok(None)) => {}
                    _ => break None,
                }
                drop(guard);
                thread::sleep(Duration::from_millis(100));
            };
            *child_ref.lock().unwrap() = None;
            *stdin_ref.lock().unwrap() = None;
            let mut sh = shared.lock().unwrap();
            let msg = match code {
                Some(c) => c.to_string(),
                None => "—".into(),
            };
            if sh.status == Status::Stopping {
                sh.push(format!("[Менеджер] Сервер остановлен (код {})", msg), Level::Info);
            } else {
                sh.push(format!("[Менеджер] Сервер завершился неожиданно (код {})", msg), Level::Error);
            }
            sh.status = Status::Stopped;
            sh.players.clear();
            sh.started_at = None;
            sh.pid = None;
            sh.exited = Some(code);
        });
        Ok(())
    }

    fn spawn_reader<R: Read + Send + 'static>(shared: Arc<Mutex<Shared>>, r: R) {
        thread::spawn(move || {
            read_lines(r, |line| {
                let mut sh = shared.lock().unwrap();
                let lvl = classify(&line);
                sh.parse(&line);
                sh.push(line, lvl);
            })
        });
    }

    pub fn send(&self, command: &str) -> Result<(), String> {
        let command = command.trim().trim_start_matches('/');
        if command.is_empty() {
            return Ok(());
        }
        let mut guard = self.stdin.lock().unwrap();
        let stdin = guard.as_mut().ok_or("Сервер не запущен")?;
        self.shared.lock().unwrap().push(format!("> {}", command), Level::Cmd);
        stdin
            .write_all(format!("{}\n", command).as_bytes())
            .and_then(|_| stdin.flush())
            .map_err(|e| format!("Не удалось отправить команду: {}", e))
    }

    /// Подписка на строку консоли. Подписываться до отправки команды.
    pub fn listen(&self, regex: &str) -> Receiver<String> {
        let (tx, rx) = channel();
        if let Ok(re) = Regex::new(regex) {
            self.shared.lock().unwrap().listeners.push((re, tx));
        }
        rx
    }

    /// Корректная остановка: команда stop, по таймауту — kill.
    pub fn stop(&self, timeout: Duration) {
        if !self.running() {
            return;
        }
        self.shared.lock().unwrap().status = Status::Stopping;
        let _ = self.send("stop");
        let child = self.child.clone();
        let shared = self.shared.clone();
        thread::spawn(move || {
            let t0 = Instant::now();
            while t0.elapsed() < timeout {
                if child.lock().unwrap().is_none() {
                    return;
                }
                thread::sleep(Duration::from_millis(200));
            }
            if let Some(c) = child.lock().unwrap().as_mut() {
                shared.lock().unwrap().push(
                    format!("[Менеджер] Сервер не остановился за {} с — принудительное завершение", timeout.as_secs()),
                    Level::Error,
                );
                let _ = c.kill();
            }
        });
    }

    pub fn kill(&self) {
        if let Some(c) = self.child.lock().unwrap().as_mut() {
            self.shared.lock().unwrap().status = Status::Stopping;
            self.info("Принудительное завершение процесса", Level::Error);
            let _ = c.kill();
        }
    }

    pub fn uptime(&self) -> Option<Duration> {
        self.shared.lock().unwrap().started_at.map(|t| t.elapsed())
    }

    pub fn pid(&self) -> Option<u32> {
        self.shared.lock().unwrap().pid
    }

    pub fn players(&self) -> Vec<String> {
        self.shared.lock().unwrap().players.iter().cloned().collect()
    }

    /// Забирает событие "процесс завершился" (один раз).
    pub fn take_exit(&self) -> Option<Option<i32>> {
        self.shared.lock().unwrap().exited.take()
    }

    pub fn clear_log(&self) {
        let mut sh = self.shared.lock().unwrap();
        sh.log.clear();
        sh.log_counter += 1;
    }
}

fn read_lines<R: Read, F: FnMut(String)>(r: R, mut f: F) {
    let mut reader = BufReader::new(r);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let line = String::from_utf8_lossy(&buf).trim_end_matches(['\r', '\n']).to_string();
                if !line.is_empty() {
                    f(line);
                }
            }
        }
    }
}

/// Сохраняет мир на диск перед копированием работающего мира. Возвращает true, если нужно потом save-on.
pub fn flush_world(p: &ServerProcess) -> bool {
    if p.status() != Status::Running {
        return false;
    }
    let _ = p.send("save-off");
    let rx = p.listen(r"Saved the (game|world)|Saving is already|Usage:|Incorrect argument|Unknown or incomplete command|Unknown command");
    let _ = p.send("save-all flush");
    if let Ok(line) = rx.recv_timeout(Duration::from_secs(30)) {
        if !line.contains("Saved the") {
            let rx = p.listen(r"Saved the (game|world)");
            let _ = p.send("save-all");
            let _ = rx.recv_timeout(Duration::from_secs(30));
        }
    }
    thread::sleep(Duration::from_secs(1));
    true
}
