//! Моды и плагины: список и включение/выключение (.disabled, как в Prism Launcher).

use regex::Regex;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

pub const DISABLED: &str = ".disabled";

#[derive(Clone)]
pub struct Mod {
    pub path: PathBuf,
    pub folder: String,
    pub file: String,
    pub enabled: bool,
    pub name: String,
    pub version: String,
    pub loader: String,
    pub desc: String,
    pub size: u64,
}

pub fn mod_dirs(server_dir: &str) -> Vec<&'static str> {
    ["mods", "plugins"].into_iter().filter(|d| Path::new(server_dir).join(d).is_dir()).collect()
}

fn toml_value(text: &str, key: &str) -> Option<String> {
    let re = Regex::new(&format!(r#"(?m)^\s*{}\s*=\s*["']([^"']*)["']"#, regex::escape(key))).ok()?;
    re.captures(text).map(|c| c[1].to_string())
}

fn yaml_value(text: &str, key: &str) -> Option<String> {
    let re = Regex::new(&format!(r#"(?m)^{}:\s*['"]?([^'"\n]+)"#, regex::escape(key))).ok()?;
    re.captures(text).map(|c| c[1].trim().to_string())
}

/// (название, версия, загрузчик, описание) из jar. Ошибки игнорируются.
pub fn read_meta(path: &Path) -> (Option<String>, Option<String>, Option<String>, String) {
    let Ok(f) = File::open(path) else { return (None, None, None, String::new()) };
    let Ok(mut z) = zip::ZipArchive::new(f) else { return (None, None, None, String::new()) };
    let mut read = |n: &str| -> Option<String> {
        let mut f = z.by_name(n).ok()?;
        let mut s = String::new();
        let mut buf = vec![];
        f.read_to_end(&mut buf).ok()?;
        s.push_str(&String::from_utf8_lossy(&buf));
        Some(s)
    };
    let s = |v: &serde_json::Value| v.as_str().map(String::from);
    if let Some(t) = read("fabric.mod.json") {
        if let Ok(d) = serde_json::from_str::<serde_json::Value>(&t.replace(['\n', '\r', '\t'], " ")) {
            return (s(&d["name"]).or(s(&d["id"])), s(&d["version"]), Some("Fabric".into()),
                    s(&d["description"]).unwrap_or_default());
        }
    }
    if let Some(t) = read("quilt.mod.json") {
        if let Ok(d) = serde_json::from_str::<serde_json::Value>(&t) {
            let q = &d["quilt_loader"];
            return (s(&q["metadata"]["name"]).or(s(&q["id"])), s(&q["version"]), Some("Quilt".into()),
                    s(&q["metadata"]["description"]).unwrap_or_default());
        }
    }
    for (file, loader) in [("META-INF/neoforge.mods.toml", "NeoForge"), ("META-INF/mods.toml", "Forge")] {
        if let Some(t) = read(file) {
            let mut ver = toml_value(&t, "version");
            if ver.as_deref().map(|v| v.contains("${")).unwrap_or(false) {
                ver = read("META-INF/MANIFEST.MF").and_then(|m| {
                    Regex::new(r"(?m)^Implementation-Version:\s*(.+)$").unwrap().captures(&m).map(|c| c[1].trim().to_string())
                });
            }
            return (toml_value(&t, "displayName").or(toml_value(&t, "modId")), ver, Some(loader.into()),
                    toml_value(&t, "description").unwrap_or_default().trim().to_string());
        }
    }
    if let Some(t) = read("mcmod.info") {
        if let Ok(mut d) = serde_json::from_str::<serde_json::Value>(&t) {
            if d.is_object() {
                d = d["modList"].clone();
            }
            let m = &d[0];
            return (s(&m["name"]), s(&m["version"]), Some("Forge".into()), s(&m["description"]).unwrap_or_default());
        }
    }
    for file in ["paper-plugin.yml", "plugin.yml"] {
        if let Some(t) = read(file) {
            return (yaml_value(&t, "name"), yaml_value(&t, "version"), Some("Плагин".into()),
                    yaml_value(&t, "description").unwrap_or_default());
        }
    }
    (None, None, None, String::new())
}

pub fn list_mods(server_dir: &str) -> Vec<Mod> {
    let mut out = vec![];
    for d in mod_dirs(server_dir) {
        let Ok(rd) = fs::read_dir(Path::new(server_dir).join(d)) else { continue };
        for e in rd.filter_map(|e| e.ok()) {
            let f = e.file_name().to_string_lossy().to_string();
            let low = f.to_lowercase();
            if !(low.ends_with(".jar") || low.ends_with(".jar.disabled")) {
                continue;
            }
            let enabled = !low.ends_with(DISABLED);
            let base = if enabled { f.clone() } else { f[..f.len() - DISABLED.len()].to_string() };
            let (name, version, loader, desc) = read_meta(&e.path());
            let version = version.filter(|v| !v.contains("${")).unwrap_or_default();
            out.push(Mod {
                path: e.path(),
                folder: d.to_string(),
                name: name.unwrap_or_else(|| base.trim_end_matches(".jar").to_string()),
                file: base,
                enabled,
                version,
                loader: loader.unwrap_or_default(),
                desc,
                size: e.metadata().map(|m| m.len()).unwrap_or(0),
            });
        }
    }
    out.sort_by(|a, b| (a.folder.clone(), a.name.to_lowercase()).cmp(&(b.folder.clone(), b.name.to_lowercase())));
    out
}

/// x.jar <-> x.jar.disabled. Возвращает новый путь.
pub fn set_enabled(path: &Path, enabled: bool) -> Result<PathBuf, String> {
    let s = path.to_string_lossy().to_string();
    let is_disabled = s.to_lowercase().ends_with(DISABLED);
    let new = if enabled && is_disabled {
        PathBuf::from(&s[..s.len() - DISABLED.len()])
    } else if !enabled && !is_disabled {
        PathBuf::from(format!("{}{}", s, DISABLED))
    } else {
        return Ok(path.to_path_buf());
    };
    if new.exists() {
        return Err(format!("Файл уже существует: {}", new.file_name().unwrap_or_default().to_string_lossy()));
    }
    fs::rename(path, &new).map_err(|e| e.to_string())?;
    Ok(new)
}

pub fn add_mods(server_dir: &str, files: &[PathBuf], folder: &str) -> std::io::Result<()> {
    let dest = Path::new(server_dir).join(folder);
    fs::create_dir_all(&dest)?;
    for f in files {
        if let Some(n) = f.file_name() {
            fs::copy(f, dest.join(n))?;
        }
    }
    Ok(())
}
