//! Настройки приложения и список серверов. Формат совместим с Python-версией.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

pub const DEFAULT_TUNNEL: &str = "ssh -R 25565:localhost:25565 minecraft@ex1.ru";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Server {
    pub id: String,
    pub name: String,
    pub path: String,
    /// "jar" или "args"
    pub launch_mode: String,
    pub jar: String,
    /// для Forge 1.17+: @user_jvm_args.txt @libraries/.../win_args.txt
    pub custom_args: String,
    pub java: String,
    pub xms: String,
    pub xmx: String,
    pub jvm_args: String,
    pub server_args: String,
    /// пусто = <папка сервера>/backups
    pub backup_dir: String,
    /// 0 = хранить все
    pub keep_backups: u32,
    /// 0 = выключено
    pub auto_backup_min: u32,
    pub backup_on_stop: bool,
}

impl Default for Server {
    fn default() -> Self {
        Server {
            id: uuid::Uuid::new_v4().simple().to_string(),
            name: "Новый сервер".into(),
            path: String::new(),
            launch_mode: "jar".into(),
            jar: String::new(),
            custom_args: String::new(),
            java: "java".into(),
            xms: "1G".into(),
            xmx: "4G".into(),
            jvm_args: String::new(),
            server_args: "nogui".into(),
            backup_dir: String::new(),
            keep_backups: 10,
            auto_backup_min: 0,
            backup_on_stop: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct TunnelSettings {
    pub command: String,
    pub new_window: bool,
    pub auto_reconnect: bool,
    pub start_with_server: bool,
    pub stop_with_server: bool,
}

impl Default for TunnelSettings {
    fn default() -> Self {
        TunnelSettings {
            command: DEFAULT_TUNNEL.into(),
            new_window: cfg!(windows),
            auto_reconnect: true,
            start_with_server: false,
            stop_with_server: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub servers: Vec<Server>,
    pub last_server: Option<String>,
    pub tunnel: TunnelSettings,
    pub stop_timeout: u64,
    pub dark_theme: bool,
    /// прочие поля Python-версии сохраняем как есть
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            servers: vec![],
            last_server: None,
            tunnel: TunnelSettings::default(),
            stop_timeout: 60,
            dark_theme: true,
            extra: Default::default(),
        }
    }
}

pub fn app_dir() -> PathBuf {
    let base = if cfg!(windows) { dirs::config_dir() } else { dirs::config_dir() };
    let dir = base.unwrap_or_else(|| PathBuf::from(".")).join("MCServerManager");
    let _ = fs::create_dir_all(&dir);
    dir
}

pub fn config_path() -> PathBuf {
    app_dir().join("config.json")
}

impl Config {
    pub fn load() -> Config {
        fs::read_to_string(config_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = config_path();
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(self).unwrap_or_default())?;
        fs::rename(tmp, path)
    }

    pub fn server(&self, id: &str) -> Option<&Server> {
        self.servers.iter().find(|s| s.id == id)
    }

    pub fn server_mut(&mut self, id: &str) -> Option<&mut Server> {
        self.servers.iter_mut().find(|s| s.id == id)
    }
}
