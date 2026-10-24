//! server.properties: чтение и запись с сохранением комментариев и порядка строк.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Str,
    Int,
    Bool,
    Choice(&'static [&'static str]),
}

/// Подсказки и типы для основных параметров.
pub const KNOWN: &[(&str, &str, Kind)] = &[
    ("motd", "Описание сервера в списке серверов", Kind::Str),
    ("server-port", "Порт сервера", Kind::Int),
    ("server-ip", "IP для привязки (обычно пусто)", Kind::Str),
    ("max-players", "Максимум игроков", Kind::Int),
    ("online-mode", "Проверка лицензии (false = пиратки могут заходить)", Kind::Bool),
    ("white-list", "Белый список включён", Kind::Bool),
    ("enforce-whitelist", "Кикать игроков не из белого списка при его перезагрузке", Kind::Bool),
    ("gamemode", "Режим игры по умолчанию", Kind::Choice(&["survival", "creative", "adventure", "spectator"])),
    ("force-gamemode", "Принудительно ставить режим по умолчанию при входе", Kind::Bool),
    ("difficulty", "Сложность", Kind::Choice(&["peaceful", "easy", "normal", "hard"])),
    ("hardcore", "Хардкор", Kind::Bool),
    ("pvp", "PvP между игроками", Kind::Bool),
    ("level-name", "Папка мира", Kind::Str),
    ("level-seed", "Сид мира", Kind::Str),
    ("level-type", "Тип мира", Kind::Str),
    ("generate-structures", "Генерировать структуры", Kind::Bool),
    ("allow-nether", "Разрешить Незер", Kind::Bool),
    ("allow-flight", "Разрешить полёт (иначе кик за «полёт»)", Kind::Bool),
    ("spawn-monsters", "Спавн монстров", Kind::Bool),
    ("spawn-animals", "Спавн животных", Kind::Bool),
    ("spawn-npcs", "Спавн жителей", Kind::Bool),
    ("spawn-protection", "Радиус защиты спавна (0 = выкл.)", Kind::Int),
    ("view-distance", "Дальность прорисовки (чанки)", Kind::Int),
    ("simulation-distance", "Дальность симуляции (чанки)", Kind::Int),
    ("max-world-size", "Максимальный радиус мира", Kind::Int),
    ("enable-command-block", "Командные блоки", Kind::Bool),
    ("op-permission-level", "Уровень прав операторов (1-4)", Kind::Choice(&["1", "2", "3", "4"])),
    ("player-idle-timeout", "Кик за AFK, минут (0 = выкл.)", Kind::Int),
    ("enable-rcon", "Включить RCON", Kind::Bool),
    ("rcon.port", "Порт RCON", Kind::Int),
    ("rcon.password", "Пароль RCON", Kind::Str),
    ("enable-query", "Включить Query", Kind::Bool),
    ("enable-status", "Показывать сервер в списке (статус)", Kind::Bool),
    ("hide-online-players", "Скрывать список игроков в статусе", Kind::Bool),
    ("resource-pack", "Ссылка на ресурспак", Kind::Str),
    ("require-resource-pack", "Обязательный ресурспак", Kind::Bool),
    ("network-compression-threshold", "Порог сжатия пакетов", Kind::Int),
    ("max-tick-time", "Watchdog: макс. время тика, мс (-1 = выкл.)", Kind::Int),
    ("enforce-secure-profile", "Требовать подписанные сообщения чата", Kind::Bool),
    ("sync-chunk-writes", "Синхронная запись чанков", Kind::Bool),
];

pub fn known(key: &str) -> Option<(&'static str, Kind)> {
    KNOWN.iter().find(|k| k.0 == key).map(|k| (k.1, k.2))
}

fn unescape(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            let n = chars[i + 1];
            if n == 'u' && i + 6 <= chars.len() {
                let hex: String = chars[i + 2..i + 6].iter().collect();
                if let Ok(v) = u32::from_str_radix(&hex, 16) {
                    if let Some(ch) = char::from_u32(v) {
                        out.push(ch);
                        i += 6;
                        continue;
                    }
                }
            }
            out.push(match n {
                'n' => '\n',
                't' => '\t',
                'r' => '\r',
                'f' => '\x0c',
                other => other,
            });
            i += 2;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

fn escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) > 126 && (c as u32) <= 0xFFFF => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[derive(Clone, Default)]
pub struct Properties {
    pub path: PathBuf,
    lines: Vec<String>,
    values: BTreeMap<String, String>,
    index: BTreeMap<String, usize>,
    pub exists: bool,
}

impl Properties {
    pub fn load(server_dir: &str) -> Properties {
        let path = Path::new(server_dir).join("server.properties");
        let mut p = Properties { path: path.clone(), ..Default::default() };
        if let Ok(text) = fs::read(&path) {
            p.exists = true;
            p.lines = String::from_utf8_lossy(&text).lines().map(|l| l.to_string()).collect();
            for (n, line) in p.lines.iter().enumerate() {
                let st = line.trim_start();
                if st.is_empty() || st.starts_with('#') || st.starts_with('!') {
                    continue;
                }
                // ключ до первого неэкранированного '=' или ':'
                let mut key = String::new();
                let mut rest = "";
                let mut esc = false;
                for (i, c) in st.char_indices() {
                    if esc {
                        key.push('\\');
                        key.push(c);
                        esc = false;
                    } else if c == '\\' {
                        esc = true;
                    } else if c == '=' || c == ':' || c.is_whitespace() {
                        rest = &st[i..];
                        break;
                    } else {
                        key.push(c);
                    }
                }
                let rest = rest.trim_start();
                let rest = rest.strip_prefix('=').or_else(|| rest.strip_prefix(':')).unwrap_or(rest).trim_start();
                let key = unescape(&key);
                p.values.insert(key.clone(), unescape(rest));
                p.index.insert(key, n);
            }
        }
        p
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(|s| s.as_str())
    }

    pub fn get_bool(&self, key: &str, default: bool) -> bool {
        self.get(key).map(|v| v.trim().eq_ignore_ascii_case("true")).unwrap_or(default)
    }

    pub fn set(&mut self, key: &str, value: &str) {
        let line = format!("{}={}", key, escape(value));
        if let Some(&n) = self.index.get(key) {
            self.lines[n] = line;
        } else {
            self.index.insert(key.to_string(), self.lines.len());
            self.lines.push(line);
        }
        self.values.insert(key.to_string(), value.to_string());
    }

    pub fn save(&self) -> std::io::Result<()> {
        fs::write(&self.path, self.lines.join("\n") + "\n")
    }

    pub fn items(&self) -> impl Iterator<Item = (&String, &String)> {
        self.values.iter()
    }
}

pub fn level_name(server_dir: &str) -> String {
    Properties::load(server_dir)
        .get("level-name")
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("world")
        .to_string()
}

pub fn server_port(server_dir: &str) -> u16 {
    Properties::load(server_dir).get("server-port").and_then(|s| s.trim().parse().ok()).unwrap_or(25565)
}
