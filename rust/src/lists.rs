//! Операторы, белый список и баны (ops.json, whitelist.json, banned-*.json).

use md5::{Digest, Md5};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ListKind {
    Ops,
    Whitelist,
    BannedPlayers,
    BannedIps,
}

pub const ALL: [ListKind; 4] = [ListKind::Ops, ListKind::Whitelist, ListKind::BannedPlayers, ListKind::BannedIps];

impl ListKind {
    pub fn file(self) -> &'static str {
        match self {
            ListKind::Ops => "ops.json",
            ListKind::Whitelist => "whitelist.json",
            ListKind::BannedPlayers => "banned-players.json",
            ListKind::BannedIps => "banned-ips.json",
        }
    }
    pub fn title(self) -> &'static str {
        match self {
            ListKind::Ops => "Операторы",
            ListKind::Whitelist => "Белый список",
            ListKind::BannedPlayers => "Баны игроков",
            ListKind::BannedIps => "Баны IP",
        }
    }
    pub fn key(self) -> &'static str {
        if self == ListKind::BannedIps { "ip" } else { "name" }
    }
    /// (поле, заголовок)
    pub fn columns(self) -> &'static [(&'static str, &'static str)] {
        match self {
            ListKind::Ops => &[("name", "Ник"), ("uuid", "UUID"), ("level", "Уровень")],
            ListKind::Whitelist => &[("name", "Ник"), ("uuid", "UUID")],
            ListKind::BannedPlayers => &[("name", "Ник"), ("reason", "Причина"), ("created", "Когда"), ("expires", "До")],
            ListKind::BannedIps => &[("ip", "IP"), ("reason", "Причина"), ("created", "Когда"), ("expires", "До")],
        }
    }
    pub fn is_ban(self) -> bool {
        matches!(self, ListKind::BannedPlayers | ListKind::BannedIps)
    }
    pub fn command(self, add: bool, name: &str, reason: &str) -> String {
        let c = match (self, add) {
            (ListKind::Ops, true) => format!("op {}", name),
            (ListKind::Ops, false) => format!("deop {}", name),
            (ListKind::Whitelist, true) => format!("whitelist add {}", name),
            (ListKind::Whitelist, false) => format!("whitelist remove {}", name),
            (ListKind::BannedPlayers, true) => format!("ban {} {}", name, reason),
            (ListKind::BannedPlayers, false) => format!("pardon {}", name),
            (ListKind::BannedIps, true) => format!("ban-ip {} {}", name, reason),
            (ListKind::BannedIps, false) => format!("pardon-ip {}", name),
        };
        c.trim().to_string()
    }
}

pub fn path_for(server_dir: &str, kind: ListKind) -> PathBuf {
    Path::new(server_dir).join(kind.file())
}

pub fn load(server_dir: &str, kind: ListKind) -> Vec<Value> {
    fs::read_to_string(path_for(server_dir, kind))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
}

pub fn save(server_dir: &str, kind: ListKind, entries: &[Value]) -> std::io::Result<()> {
    fs::write(path_for(server_dir, kind), serde_json::to_string_pretty(entries).unwrap_or("[]".into()))
}

pub fn field(v: &Value, key: &str) -> String {
    match &v[key] {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn fmt_uuid(b: &[u8]) -> String {
    let h: String = b.iter().map(|x| format!("{:02x}", x)).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

/// UUID как в Java UUID.nameUUIDFromBytes("OfflinePlayer:" + name).
pub fn offline_uuid(name: &str) -> String {
    let mut h: [u8; 16] = Md5::digest(format!("OfflinePlayer:{}", name).as_bytes()).into();
    h[6] = (h[6] & 0x0F) | 0x30;
    h[8] = (h[8] & 0x3F) | 0x80;
    fmt_uuid(&h)
}

fn online_uuid(name: &str) -> Result<(String, String), String> {
    let resp: Value = ureq::get(&format!("https://api.mojang.com/users/profiles/minecraft/{}", name))
        .set("User-Agent", "MCServerManager")
        .timeout(std::time::Duration::from_secs(6))
        .call()
        .map_err(|e| e.to_string())?
        .into_json()
        .map_err(|e| e.to_string())?;
    let id = resp["id"].as_str().ok_or("игрок не найден")?;
    let bytes: Vec<u8> = (0..16).filter_map(|i| u8::from_str_radix(id.get(i * 2..i * 2 + 2)?, 16).ok()).collect();
    if bytes.len() != 16 {
        return Err("неверный ответ Mojang".into());
    }
    Ok((fmt_uuid(&bytes), resp["name"].as_str().unwrap_or(name).to_string()))
}

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S %z").to_string()
}

pub fn make_entry(kind: ListKind, name: &str, online_mode: bool, reason: &str) -> Result<Value, String> {
    let reason = if reason.trim().is_empty() { "Banned by an operator." } else { reason.trim() };
    if kind == ListKind::BannedIps {
        return Ok(json!({"ip": name, "created": now(), "source": "MCServerManager", "expires": "forever", "reason": reason}));
    }
    let (uuid, real) = if online_mode {
        online_uuid(name).map_err(|e| format!("Не удалось получить UUID игрока «{}» у Mojang: {}", name, e))?
    } else {
        (offline_uuid(name), name.to_string())
    };
    Ok(match kind {
        ListKind::Ops => json!({"uuid": uuid, "name": real, "level": 4, "bypassesPlayerLimit": false}),
        ListKind::Whitelist => json!({"uuid": uuid, "name": real}),
        _ => json!({"uuid": uuid, "name": real, "created": now(), "source": "MCServerManager", "expires": "forever", "reason": reason}),
    })
}

/// Добавление прямо в файл (когда сервер выключен).
pub fn add_offline(server_dir: &str, kind: ListKind, name: &str, online_mode: bool, reason: &str) -> Result<(), String> {
    let entry = make_entry(kind, name, online_mode, reason)?;
    let mut entries: Vec<Value> =
        load(server_dir, kind).into_iter().filter(|e| !field(e, kind.key()).eq_ignore_ascii_case(name)).collect();
    entries.push(entry);
    save(server_dir, kind, &entries).map_err(|e| e.to_string())
}

pub fn remove_offline(server_dir: &str, kind: ListKind, name: &str) -> std::io::Result<()> {
    let entries: Vec<Value> =
        load(server_dir, kind).into_iter().filter(|e| !field(e, kind.key()).eq_ignore_ascii_case(name)).collect();
    save(server_dir, kind, &entries)
}
