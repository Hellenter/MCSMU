//! SSH-туннель (ssh -R ...) для доступа к серверу из интернета.

use crate::config::TunnelSettings;
use crate::launch::split_args;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TunnelState {
    Closed,
    Open,
    Reconnecting,
}

struct Inner {
    child: Option<Child>,
    wanted: bool,
    state: TunnelState,
    log: VecDeque<String>,
    log_counter: u64,
    settings: TunnelSettings,
}

#[derive(Clone)]
pub struct Tunnel {
    inner: Arc<Mutex<Inner>>,
}

const KEEPALIVE: [&str; 6] =
    ["-o", "ServerAliveInterval=30", "-o", "ServerAliveCountMax=3", "-o", "ExitOnForwardFailure=yes"];

fn log(inner: &mut Inner, text: String) {
    let stamp = chrono::Local::now().format("%H:%M:%S");
    if inner.log.len() > 500 {
        inner.log.pop_front();
    }
    inner.log.push_back(format!("{} {}", stamp, text));
    inner.log_counter += 1;
}

pub fn build_command(settings: &TunnelSettings) -> Result<Vec<String>, String> {
    let cmd = split_args(&settings.command);
    if cmd.is_empty() {
        return Err("Команда туннеля не задана (Настройки)".into());
    }
    let prog = cmd[0].replace('\\', "/").rsplit('/').next().unwrap_or("").to_lowercase();
    if prog == "ssh" || prog == "ssh.exe" {
        let mut out = vec![cmd[0].clone()];
        if !cmd.iter().any(|a| a.contains("ServerAliveInterval")) {
            out.extend(KEEPALIVE.iter().map(|s| s.to_string()));
        }
        // -N: только перенаправление порта, без удалённой оболочки
        if !cmd.iter().any(|a| a == "-N") && !settings.new_window {
            out.push("-N".into());
        }
        out.extend(cmd[1..].iter().cloned());
        return Ok(out);
    }
    Ok(cmd)
}

impl Tunnel {
    pub fn new(settings: TunnelSettings) -> Tunnel {
        Tunnel {
            inner: Arc::new(Mutex::new(Inner {
                child: None,
                wanted: false,
                state: TunnelState::Closed,
                log: VecDeque::new(),
                log_counter: 0,
                settings,
            })),
        }
    }

    pub fn set_settings(&self, s: TunnelSettings) {
        self.inner.lock().unwrap().settings = s;
    }

    pub fn state(&self) -> TunnelState {
        self.inner.lock().unwrap().state
    }

    pub fn wanted(&self) -> bool {
        self.inner.lock().unwrap().wanted
    }

    pub fn log_lines(&self) -> (u64, Vec<String>) {
        let i = self.inner.lock().unwrap();
        (i.log_counter, i.log.iter().cloned().collect())
    }

    fn spawn(inner: &mut Inner) -> Result<(), String> {
        let cmd = build_command(&inner.settings)?;
        log(inner, format!("Запуск туннеля: {}", cmd.join(" ")));
        let mut c = Command::new(&cmd[0]);
        c.args(&cmd[1..]);
        let new_window = inner.settings.new_window;
        if new_window {
            // отдельное окно: можно ввести пароль / подтвердить ключ хоста
            #[cfg(windows)]
            c.creation_flags(CREATE_NEW_CONSOLE);
        } else {
            c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
            #[cfg(windows)]
            c.creation_flags(crate::server::CREATE_NO_WINDOW);
        }
        match c.spawn() {
            Ok(child) => {
                inner.child = Some(child);
                inner.state = TunnelState::Open;
                Ok(())
            }
            Err(e) => {
                inner.wanted = false;
                inner.state = TunnelState::Closed;
                if e.kind() == std::io::ErrorKind::NotFound {
                    log(inner, "Не найден ssh. В Windows 10/11: Параметры → Приложения → Дополнительные компоненты → «Клиент OpenSSH».".into());
                    Err("Не найдена программа ssh".into())
                } else {
                    log(inner, format!("Ошибка запуска: {}", e));
                    Err(e.to_string())
                }
            }
        }
    }

    fn attach_readers(&self) {
        let mut i = self.inner.lock().unwrap();
        let Some(child) = i.child.as_mut() else { return };
        let streams: Vec<Box<dyn std::io::Read + Send>> = [
            child.stdout.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
            child.stderr.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
        ]
        .into_iter()
        .flatten()
        .collect();
        drop(i);
        for s in streams {
            let inner = self.inner.clone();
            thread::spawn(move || {
                for line in BufReader::new(s).lines().map_while(Result::ok) {
                    if !line.trim().is_empty() {
                        log(&mut inner.lock().unwrap(), line);
                    }
                }
            });
        }
    }

    pub fn start(&self) -> Result<(), String> {
        {
            let mut i = self.inner.lock().unwrap();
            if i.child.is_some() {
                return Ok(());
            }
            i.wanted = true;
            Self::spawn(&mut i)?;
        }
        self.attach_readers();
        let me = self.clone();
        thread::spawn(move || me.watch());
        Ok(())
    }

    fn watch(&self) {
        let mut delay = 5u64;
        let mut started = Instant::now();
        loop {
            // ждём завершения процесса
            let code = loop {
                thread::sleep(Duration::from_millis(300));
                let mut i = self.inner.lock().unwrap();
                match i.child.as_mut().map(|c| c.try_wait()) {
                    Some(Ok(Some(st))) => break st.code(),
                    Some(Ok(None)) => continue,
                    _ => break None,
                }
            };
            let mut i = self.inner.lock().unwrap();
            i.child = None;
            if !i.wanted {
                i.state = TunnelState::Closed;
                log(&mut i, "Туннель закрыт".into());
                return;
            }
            log(&mut i, format!("Туннель оборвался (код {})", code.map(|c| c.to_string()).unwrap_or("—".into())));
            if !i.settings.auto_reconnect {
                i.wanted = false;
                i.state = TunnelState::Closed;
                return;
            }
            delay = if started.elapsed() < Duration::from_secs(30) { (delay * 2).min(60) } else { 5 };
            i.state = TunnelState::Reconnecting;
            log(&mut i, format!("Переподключение через {} с…", delay));
            drop(i);
            for _ in 0..delay * 10 {
                if !self.inner.lock().unwrap().wanted {
                    let mut i = self.inner.lock().unwrap();
                    i.state = TunnelState::Closed;
                    return;
                }
                thread::sleep(Duration::from_millis(100));
            }
            let res = Self::spawn(&mut self.inner.lock().unwrap());
            if res.is_err() {
                return;
            }
            self.attach_readers();
            started = Instant::now();
        }
    }

    pub fn stop(&self) {
        let mut i = self.inner.lock().unwrap();
        i.wanted = false;
        if let Some(c) = i.child.as_mut() {
            let _ = c.kill();
        } else {
            i.state = TunnelState::Closed;
        }
    }
}
