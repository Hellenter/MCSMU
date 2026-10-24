//! Как запускать сервер: jar, аргументы Forge, поиск Java.

use crate::config::Server;
use regex::Regex;
use std::fs;
use std::path::{Path, PathBuf};

/// Разбиение строки аргументов с поддержкой кавычек.
pub fn split_args(text: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has = false;
    for c in text.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                has = true;
            }
            None if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            None => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub fn list_jars(dir: &str) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n.to_lowercase().ends_with(".jar"))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn glob_dirs(base: &Path) -> Vec<PathBuf> {
    fs::read_dir(base)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_dir()).collect())
        .unwrap_or_default()
}

/// Угадывает способ запуска: (mode, jar, custom_args).
pub fn detect_launch(dir: &str) -> (String, String, String) {
    let root = Path::new(dir);
    let args_file = if cfg!(windows) { "win_args.txt" } else { "unix_args.txt" };
    for base in ["libraries/net/minecraftforge/forge", "libraries/net/neoforged/neoforge", "libraries/net/neoforged/forge"] {
        let mut hits: Vec<PathBuf> = glob_dirs(&root.join(base))
            .into_iter()
            .map(|d| d.join(args_file))
            .filter(|p| p.exists())
            .collect();
        hits.sort();
        if let Some(h) = hits.last() {
            let rel = h.strip_prefix(root).unwrap_or(h).to_string_lossy().replace('\\', "/");
            let extra = if root.join("user_jvm_args.txt").exists() { "@user_jvm_args.txt " } else { "" };
            return ("args".into(), String::new(), format!("{}@{}", extra, rel));
        }
    }
    let jars = list_jars(dir);
    let re = Regex::new(r"(?i)(paper|purpur|spigot|fabric|forge|server|minecraft|mohist|arclight|velocity)").unwrap();
    let pick = jars
        .iter()
        .find(|j| re.is_match(j) && !j.to_lowercase().contains("installer"))
        .or(jars.first())
        .cloned()
        .unwrap_or_default();
    ("jar".into(), pick, String::new())
}

pub fn build_command(s: &Server) -> Result<Vec<String>, String> {
    let mut cmd = vec![if s.java.trim().is_empty() { "java".to_string() } else { s.java.trim().to_string() }];
    if !s.xms.trim().is_empty() {
        cmd.push(format!("-Xms{}", s.xms.trim()));
    }
    if !s.xmx.trim().is_empty() {
        cmd.push(format!("-Xmx{}", s.xmx.trim()));
    }
    cmd.extend(["-Dfile.encoding=UTF-8", "-Dstdout.encoding=UTF-8", "-Dstderr.encoding=UTF-8"].map(String::from));
    cmd.extend(split_args(&s.jvm_args));
    if s.launch_mode == "args" {
        cmd.extend(split_args(&s.custom_args));
    } else {
        if s.jar.trim().is_empty() {
            return Err("Не выбран jar-файл сервера".into());
        }
        cmd.push("-jar".into());
        cmd.push(s.jar.trim().into());
    }
    cmd.extend(split_args(&s.server_args));
    Ok(cmd)
}

/// Ищет установленные Java: разные версии MC требуют разные Java.
pub fn find_java() -> Vec<String> {
    let mut found: Vec<PathBuf> = vec![];
    let exe = if cfg!(windows) { "java.exe" } else { "java" };
    let mut roots: Vec<PathBuf> = vec![];
    if cfg!(windows) {
        let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
        let pf86 = std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| r"C:\Program Files (x86)".into());
        for r in [&pf, &pf86] {
            for v in ["Java", "Eclipse Adoptium", "Eclipse Foundation", "Microsoft", "Zulu", "BellSoft", "Amazon Corretto",
                      "AdoptOpenJDK", "Semeru", "Liberica"] {
                roots.push(Path::new(r).join(v));
            }
        }
        // Java из лаунчера Minecraft
        let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
        for base in [
            Path::new(&pf86).join("Minecraft Launcher").join("runtime"),
            Path::new(&local).join("Packages").join("Microsoft.4297127D64EC6_8wekyb3d8bbwe").join("LocalCache").join("Local").join("runtime"),
        ] {
            for walk in walkdir::WalkDir::new(base).max_depth(5).into_iter().filter_map(|e| e.ok()) {
                if walk.file_name().to_string_lossy().eq_ignore_ascii_case(exe)
                    && walk.path().parent().map(|p| p.ends_with("bin")).unwrap_or(false)
                {
                    found.push(walk.path().to_path_buf());
                }
            }
        }
    } else {
        roots.push("/usr/lib/jvm".into());
        roots.push("/Library/Java/JavaVirtualMachines".into());
    }
    for r in roots {
        for d in glob_dirs(&r) {
            for cand in [d.join("bin").join(exe), d.join("Contents/Home/bin").join(exe)] {
                if cand.exists() {
                    found.push(cand);
                }
            }
            for d2 in glob_dirs(&d) {
                let cand = d2.join("bin").join(exe);
                if cand.exists() {
                    found.push(cand);
                }
            }
        }
    }
    let mut out = vec!["java".to_string()];
    let mut seen = std::collections::HashSet::new();
    for f in found {
        let key = fs::canonicalize(&f).unwrap_or(f.clone()).to_string_lossy().to_lowercase();
        if seen.insert(key) {
            out.push(f.to_string_lossy().to_string());
        }
    }
    out[1..].sort();
    out
}

/// Короткое имя Java для отображения: папка JDK вместо полного пути.
pub fn java_label(java: &str) -> String {
    if java.trim().is_empty() || java == "java" {
        return "java (PATH)".into();
    }
    let p = Path::new(java);
    p.parent()
        .and_then(|b| b.parent())
        .and_then(|d| d.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| java.to_string())
}

/// "ssh -R 25565:localhost:25565 user@host" -> "host" (или host:port, если порт не 25565).
pub fn tunnel_address(command: &str) -> Option<String> {
    let args = split_args(command);
    let mut host = None;
    let mut port = "25565".to_string();
    for (i, a) in args.iter().enumerate() {
        if a == "-R" {
            if let Some(spec) = args.get(i + 1) {
                let parts: Vec<&str> = spec.split(':').collect();
                if parts.len() >= 3 {
                    port = parts[parts.len() - 3].to_string();
                }
            }
        }
        if a.contains('@') && !a.starts_with('-') {
            host = a.split_once('@').map(|x| x.1.to_string());
        }
    }
    host.map(|h| if port == "25565" { h } else { format!("{}:{}", h, port) })
}
