"""Бэкапы, восстановление, копирование и импорт миров."""
import datetime
import os
import re
import shutil
import zipfile

from .props import level_name

SKIP_FILES = {"session.lock"}


def world_dirs(server_dir):
    """Папки мира: основной + _nether/_the_end (Spigot/Paper хранят измерения отдельно)."""
    name = level_name(server_dir)
    dirs = []
    for d in (name, name + "_nether", name + "_the_end"):
        if os.path.isdir(os.path.join(server_dir, d)):
            dirs.append(d)
    return dirs


def dir_size(path):
    total = 0
    for root, _, files in os.walk(path):
        for f in files:
            try:
                total += os.path.getsize(os.path.join(root, f))
            except OSError:
                pass
    return total


def human_size(n):
    for unit in ("Б", "КБ", "МБ", "ГБ"):
        if n < 1024 or unit == "ГБ":
            return ("%.0f %s" if unit == "Б" else "%.1f %s") % (n, unit)
        n /= 1024.0


def backup_dir(srv):
    return srv.get("backup_dir") or os.path.join(srv["path"], "backups")


def _safe_name(text):
    return re.sub(r'[\\/:*?"<>|\s]+', "_", text).strip("_") or "server"


def _iter_files(server_dir, dirs):
    for d in dirs:
        for root, _, files in os.walk(os.path.join(server_dir, d)):
            for f in files:
                if f in SKIP_FILES:
                    continue
                full = os.path.join(root, f)
                yield full, os.path.relpath(full, server_dir)


def make_backup(srv, label="", dest=None, progress=None):
    """Архивирует мир сервера в zip. Возвращает (путь, пропущенные_файлы)."""
    server_dir = srv["path"]
    dirs = world_dirs(server_dir)
    if not dirs:
        raise RuntimeError("Мир не найден (папка «%s» отсутствует)" % level_name(server_dir))
    if dest is None:
        out_dir = backup_dir(srv)
        os.makedirs(out_dir, exist_ok=True)
        stamp = datetime.datetime.now().strftime("%Y-%m-%d_%H-%M-%S")
        fname = "%s_%s%s.zip" % (_safe_name(srv["name"]), stamp, ("_" + _safe_name(label)) if label else "")
        dest = os.path.join(out_dir, fname)
    files = list(_iter_files(server_dir, dirs))
    skipped = []
    tmp = dest + ".part"
    with zipfile.ZipFile(tmp, "w", zipfile.ZIP_DEFLATED, compresslevel=6) as z:
        for i, (full, arc) in enumerate(files):
            try:
                z.write(full, arc)
            except (OSError, PermissionError):
                skipped.append(arc)
            if progress and i % 50 == 0:
                progress(i, len(files))
    os.replace(tmp, dest)
    if progress:
        progress(len(files), len(files))
    return dest, skipped


def list_backups(srv):
    d = backup_dir(srv)
    if not os.path.isdir(d):
        return []
    out = []
    for f in os.listdir(d):
        if f.lower().endswith(".zip"):
            p = os.path.join(d, f)
            st = os.stat(p)
            out.append({"path": p, "name": f, "size": st.st_size, "mtime": st.st_mtime})
    out.sort(key=lambda b: b["mtime"], reverse=True)
    return out


def rotate(srv):
    keep = int(srv.get("keep_backups") or 0)
    if keep <= 0:
        return []
    removed = []
    # ручные бэкапы с пометкой "keep" не удаляем
    auto = [b for b in list_backups(srv) if "_keep" not in b["name"]]
    for b in auto[keep:]:
        try:
            os.remove(b["path"])
            removed.append(b["name"])
        except OSError:
            pass
    return removed


def _remove_world(server_dir):
    for d in world_dirs(server_dir):
        shutil.rmtree(os.path.join(server_dir, d))


def restore_backup(srv, zip_path):
    """Заменяет текущий мир содержимым архива. Сервер должен быть остановлен."""
    server_dir = srv["path"]
    with zipfile.ZipFile(zip_path) as z:
        names = z.namelist()
        for n in names:
            target = os.path.normpath(os.path.join(server_dir, n))
            if not target.startswith(os.path.normpath(server_dir) + os.sep):
                raise RuntimeError("Подозрительный путь в архиве: " + n)
        top = {n.replace("\\", "/").split("/")[0] for n in names}
        # если в архиве мир с другим именем — переключаем level-name
        main = [t for t in top if not t.endswith(("_nether", "_the_end"))]
        _remove_world(server_dir)
        z.extractall(server_dir)
    return sorted(top), (main[0] if len(main) == 1 else None)


def copy_world(src_srv, dst_srv):
    """Копирует мир из одного сервера в другой (под именем level-name получателя)."""
    src_dir, dst_dir = src_srv["path"], dst_srv["path"]
    src_name, dst_name = level_name(src_dir), level_name(dst_dir)
    dirs = world_dirs(src_dir)
    if not dirs:
        raise RuntimeError("В исходном сервере нет мира")
    _remove_world(dst_dir)
    for d in dirs:
        new = dst_name + d[len(src_name):]
        shutil.copytree(os.path.join(src_dir, d), os.path.join(dst_dir, new),
                        ignore=shutil.ignore_patterns(*SKIP_FILES))


def import_world(srv, folder):
    """Импорт мира из произвольной папки (например, из .minecraft/saves)."""
    if not os.path.exists(os.path.join(folder, "level.dat")):
        raise RuntimeError("В выбранной папке нет level.dat — это не мир Minecraft")
    dst_dir = srv["path"]
    name = level_name(dst_dir)
    _remove_world(dst_dir)
    shutil.copytree(folder, os.path.join(dst_dir, name), ignore=shutil.ignore_patterns(*SKIP_FILES))
