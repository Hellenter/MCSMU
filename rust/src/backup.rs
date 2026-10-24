//! Бэкапы, восстановление, копирование и импорт миров.

use crate::config::Server;
use crate::props::level_name;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use walkdir::WalkDir;
use zip::write::SimpleFileOptions;

const SKIP: &[&str] = &["session.lock"];

/// Папки мира: основной + _nether/_the_end (Spigot/Paper хранят измерения отдельно).
pub fn world_dirs(server_dir: &str) -> Vec<String> {
    let name = level_name(server_dir);
    [name.clone(), format!("{}_nether", name), format!("{}_the_end", name)]
        .into_iter()
        .filter(|d| Path::new(server_dir).join(d).is_dir())
        .collect()
}

pub fn dir_size(path: &Path) -> u64 {
    WalkDir::new(path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

pub fn human_size(n: u64) -> String {
    let mut v = n as f64;
    for unit in ["Б", "КБ", "МБ", "ГБ"] {
        if v < 1024.0 || unit == "ГБ" {
            return if unit == "Б" { format!("{:.0} {}", v, unit) } else { format!("{:.1} {}", v, unit) };
        }
        v /= 1024.0;
    }
    unreachable!()
}

pub fn backup_dir(s: &Server) -> PathBuf {
    if s.backup_dir.trim().is_empty() {
        Path::new(&s.path).join("backups")
    } else {
        PathBuf::from(s.backup_dir.trim())
    }
}

fn safe_name(t: &str) -> String {
    let re = regex::Regex::new(r#"[\\/:*?"<>|\s]+"#).unwrap();
    let r = re.replace_all(t, "_").trim_matches('_').to_string();
    if r.is_empty() { "server".into() } else { r }
}

pub struct BackupResult {
    pub path: PathBuf,
    pub skipped: Vec<String>,
}

/// Архивирует мир сервера в zip.
pub fn make_backup(s: &Server, label: &str, dest: Option<PathBuf>) -> io::Result<BackupResult> {
    let dirs = world_dirs(&s.path);
    if dirs.is_empty() {
        return Err(io::Error::other(format!("Мир не найден (папка «{}» отсутствует)", level_name(&s.path))));
    }
    let dest = match dest {
        Some(d) => d,
        None => {
            let out = backup_dir(s);
            fs::create_dir_all(&out)?;
            let stamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
            let lbl = if label.is_empty() { String::new() } else { format!("_{}", safe_name(label)) };
            out.join(format!("{}_{}{}.zip", safe_name(&s.name), stamp, lbl))
        }
    };
    let tmp = dest.with_extension("zip.part");
    let mut zw = zip::ZipWriter::new(File::create(&tmp)?);
    let opts = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(6))
        .large_file(true);
    let root = Path::new(&s.path);
    let mut skipped = vec![];
    for d in &dirs {
        for e in WalkDir::new(root.join(d)).into_iter().filter_map(|e| e.ok()) {
            let rel = e.path().strip_prefix(root).unwrap_or(e.path()).to_string_lossy().replace('\\', "/");
            if e.file_type().is_dir() {
                zw.add_directory(rel + "/", opts)?;
                continue;
            }
            if SKIP.contains(&e.file_name().to_string_lossy().as_ref()) {
                continue;
            }
            match File::open(e.path()) {
                Ok(mut f) => {
                    zw.start_file(rel.clone(), opts)?;
                    if io::copy(&mut f, &mut zw).is_err() {
                        skipped.push(rel);
                    }
                }
                Err(_) => skipped.push(rel),
            }
        }
    }
    zw.finish()?;
    fs::rename(&tmp, &dest)?;
    Ok(BackupResult { path: dest, skipped })
}

#[derive(Clone)]
pub struct BackupInfo {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub mtime: SystemTime,
}

pub fn list_backups(s: &Server) -> Vec<BackupInfo> {
    let mut out: Vec<BackupInfo> = fs::read_dir(backup_dir(s))
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().to_lowercase().ends_with(".zip"))
                .filter_map(|e| {
                    let m = e.metadata().ok()?;
                    Some(BackupInfo {
                        path: e.path(),
                        name: e.file_name().to_string_lossy().to_string(),
                        size: m.len(),
                        mtime: m.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    out
}

/// Удаляет старые бэкапы сверх лимита; бэкапы с "keep" в имени не трогает.
pub fn rotate(s: &Server) -> usize {
    if s.keep_backups == 0 {
        return 0;
    }
    let mut removed = 0;
    let auto: Vec<_> = list_backups(s).into_iter().filter(|b| !b.name.contains("keep")).collect();
    for b in auto.iter().skip(s.keep_backups as usize) {
        if fs::remove_file(&b.path).is_ok() {
            removed += 1;
        }
    }
    removed
}

fn remove_world(server_dir: &str) -> io::Result<()> {
    for d in world_dirs(server_dir) {
        fs::remove_dir_all(Path::new(server_dir).join(d))?;
    }
    Ok(())
}

/// Заменяет мир содержимым архива. Возвращает (папки в архиве, имя основного мира, если оно одно).
pub fn restore_backup(s: &Server, zip_path: &Path) -> io::Result<(Vec<String>, Option<String>)> {
    let mut z = zip::ZipArchive::new(File::open(zip_path)?)?;
    let mut top = std::collections::BTreeSet::new();
    for i in 0..z.len() {
        let f = z.by_index(i)?;
        if f.enclosed_name().is_none() {
            return Err(io::Error::other(format!("Подозрительный путь в архиве: {}", f.name())));
        }
        if let Some(t) = f.name().replace('\\', "/").split('/').next() {
            top.insert(t.to_string());
        }
    }
    let main: Vec<&String> = top.iter().filter(|t| !t.ends_with("_nether") && !t.ends_with("_the_end")).collect();
    let main = if main.len() == 1 { Some(main[0].clone()) } else { None };
    remove_world(&s.path)?;
    z.extract(&s.path).map_err(io::Error::other)?;
    Ok((top.into_iter().collect(), main))
}

fn copy_dir(src: &Path, dst: &Path) -> io::Result<()> {
    for e in WalkDir::new(src).into_iter().filter_map(|e| e.ok()) {
        let rel = e.path().strip_prefix(src).unwrap();
        let target = dst.join(rel);
        if e.file_type().is_dir() {
            fs::create_dir_all(&target)?;
        } else if !SKIP.contains(&e.file_name().to_string_lossy().as_ref()) {
            fs::copy(e.path(), &target)?;
        }
    }
    Ok(())
}

/// Копирует мир из одного сервера в другой (под level-name получателя).
pub fn copy_world(src: &Server, dst: &Server) -> io::Result<()> {
    let src_name = level_name(&src.path);
    let dst_name = level_name(&dst.path);
    let dirs = world_dirs(&src.path);
    if dirs.is_empty() {
        return Err(io::Error::other("В исходном сервере нет мира"));
    }
    remove_world(&dst.path)?;
    for d in dirs {
        let new = format!("{}{}", dst_name, &d[src_name.len()..]);
        copy_dir(&Path::new(&src.path).join(&d), &Path::new(&dst.path).join(new))?;
    }
    Ok(())
}

/// Импорт мира из произвольной папки (например, .minecraft/saves).
pub fn import_world(s: &Server, folder: &Path) -> io::Result<()> {
    if !folder.join("level.dat").exists() {
        return Err(io::Error::other("В выбранной папке нет level.dat — это не мир Minecraft"));
    }
    let name = level_name(&s.path);
    remove_world(&s.path)?;
    copy_dir(folder, &Path::new(&s.path).join(name))
}
