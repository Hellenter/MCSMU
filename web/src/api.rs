//! HTTP API панели. Каждый пользователь видит и меняет только свои серверы.

use crate::auth::{self, bad, Admin, ApiErr, CurrentUser};
use crate::core::backup;
use crate::core::config::{Config, Server, TunnelSettings};
use crate::core::launch::{self, detect_launch, find_java, list_jars, tunnel_address};
use crate::core::lists::{self, ListKind};
use crate::core::mods;
use crate::core::props::{self, Kind, Properties, KNOWN};
use crate::core::server::{Level, Status};
use crate::core::tunnel::TunnelState;
use crate::state::{self, accept_eula, eula_accepted, level_and_port, start_now, App, OwnedServer, User};
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Multipart, Path as UrlPath, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;

type Res = Result<Json<Value>, ApiErr>;
type St = State<Arc<App>>;

fn ok() -> Res {
    Ok(Json(json!({ "ok": true })))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, ApiErr> {
    tokio::task::spawn_blocking(f).await.map_err(|e| ApiErr(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/api/register", post(register))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/me", get(me))
        .route("/api/me/password", post(change_password))
        .route("/api/servers", get(servers_list).post(server_create))
        .route("/api/servers/:id", get(server_get).put(server_update).delete(server_delete))
        .route("/api/servers/:id/start", post(server_start))
        .route("/api/servers/:id/stop", post(server_stop))
        .route("/api/servers/:id/restart", post(server_restart))
        .route("/api/servers/:id/kill", post(server_kill))
        .route("/api/servers/:id/console", get(console))
        .route("/api/servers/:id/console/clear", post(console_clear))
        .route("/api/servers/:id/command", post(command))
        .route("/api/servers/:id/stats", get(stats))
        .route("/api/servers/:id/lists", get(lists_get).post(lists_add))
        .route("/api/servers/:id/lists/remove", post(lists_remove))
        .route("/api/servers/:id/properties", get(props_get).put(props_put))
        .route("/api/servers/:id/backups", get(backups_list).post(backups_create))
        .route("/api/servers/:id/backups/restore", post(backups_restore))
        .route("/api/servers/:id/backups/delete", post(backups_delete))
        .route("/api/servers/:id/backups/download/:name", get(backups_download))
        .route("/api/servers/:id/world/copy", post(world_copy))
        .route("/api/servers/:id/world/import", post(world_import))
        .route("/api/servers/:id/mods", get(mods_list))
        .route("/api/servers/:id/mods/toggle", post(mods_toggle))
        .route("/api/servers/:id/mods/delete", post(mods_delete))
        .route(
            "/api/servers/:id/mods/upload",
            post(mods_upload).layer(DefaultBodyLimit::max(1024 * 1024 * 1024)),
        )
        .route("/api/detect", get(detect))
        .route("/api/java", get(java))
        .route("/api/fs", get(fs_list))
        .route("/api/tunnel", get(tunnel_get).put(tunnel_put))
        .route("/api/tunnel/start", post(tunnel_start))
        .route("/api/tunnel/stop", post(tunnel_stop))
        .route("/api/admin/users", get(admin_users))
        .route("/api/admin/users/:uid", post(admin_user_update).delete(admin_user_delete))
        .route("/api/admin/settings", get(admin_settings).put(admin_settings_put))
        .route("/api/admin/panel-tunnel/start", post(panel_tunnel_start))
        .route("/api/admin/panel-tunnel/stop", post(panel_tunnel_stop))
        .route("/api/admin/import-desktop", post(import_desktop))
        .with_state(app)
}

// ================================================================ доступ

/// Сервер текущего пользователя; чужие серверы для него «не существуют».
fn my_server(app: &App, u: &User, sid: &str) -> Result<Server, ApiErr> {
    app.server(sid)
        .filter(|s| s.owner == u.id)
        .map(|s| s.server)
        .ok_or_else(|| ApiErr(StatusCode::NOT_FOUND, "Сервер не найден".into()))
}

/// Канонический путь даже для ещё не существующей папки.
fn canon(p: &Path) -> PathBuf {
    let mut existing = p.to_path_buf();
    let mut rest = vec![];
    while !existing.exists() {
        match (existing.file_name().map(|n| n.to_os_string()), existing.parent()) {
            (Some(n), Some(parent)) => {
                rest.push(n);
                existing = parent.to_path_buf();
            }
            _ => return p.to_path_buf(),
        }
    }
    let mut out = fs::canonicalize(&existing).unwrap_or(existing);
    for n in rest.into_iter().rev() {
        out.push(n);
    }
    out
}

fn same_path(a: &str, b: &str) -> bool {
    let (a, b) = (canon(Path::new(a)), canon(Path::new(b)));
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

fn inside(path: &Path, root: &Path) -> bool {
    let (p, r) = (canon(path), canon(root));
    if cfg!(windows) {
        let (p, r) = (p.to_string_lossy().to_lowercase(), r.to_string_lossy().to_lowercase());
        p == r || p.starts_with(&(r.trim_end_matches('\\').to_string() + "\\"))
    } else {
        p.starts_with(&r)
    }
}

/// Обычным пользователям можно работать только внутри разрешённых администратором папок.
fn check_path(app: &App, u: &User, path: &str) -> Result<(), ApiErr> {
    if u.admin || path.trim().is_empty() {
        return Ok(());
    }
    let roots = app.data.lock().unwrap().settings.allowed_roots.clone();
    if roots.is_empty() || roots.iter().any(|r| inside(Path::new(path), Path::new(r))) {
        Ok(())
    } else {
        Err(ApiErr(StatusCode::FORBIDDEN, format!("Папка вне разрешённых администратором: {}", path)))
    }
}

/// Простое имя файла без переходов по папкам.
fn plain_name(name: &str) -> Result<&str, ApiErr> {
    let p = Path::new(name);
    let mut c = p.components();
    match (c.next(), c.next()) {
        (Some(Component::Normal(_)), None) if !name.contains(['/', '\\']) => Ok(name),
        _ => Err(bad("Недопустимое имя файла")),
    }
}

fn status_str(s: Status) -> &'static str {
    match s {
        Status::Stopped => "stopped",
        Status::Starting => "starting",
        Status::Running => "running",
        Status::Stopping => "stopping",
    }
}

fn tunnel_state_str(t: TunnelState) -> &'static str {
    match t {
        TunnelState::Closed => "closed",
        TunnelState::Open => "open",
        TunnelState::Reconnecting => "reconnecting",
    }
}

fn server_json(app: &App, s: &Server) -> Value {
    let p = app.procs.lock().unwrap().get(&s.id).cloned();
    let status = p.as_ref().map(|p| p.status()).unwrap_or(Status::Stopped);
    let (level, port) = level_and_port(&s.path);
    let mut v = serde_json::to_value(s).unwrap_or(json!({}));
    let notice = app.notices.lock().unwrap().get(&s.id).cloned();
    let extra = json!({
        "status": status_str(status),
        "status_ru": status.ru(),
        "players": p.as_ref().map(|p| p.players().len()).unwrap_or(0),
        "uptime": p.as_ref().and_then(|p| p.uptime()).map(|d| d.as_secs()),
        "busy": app.is_busy(&s.id),
        "notice": notice.map(|n| json!({"n": n.0, "text": n.1, "error": n.2})),
        "level_name": level,
        "port": port,
        "path_ok": Path::new(&s.path).is_dir(),
    });
    if let (Some(o), Value::Object(e)) = (v.as_object_mut(), extra) {
        o.extend(e);
    }
    v
}

fn user_json(u: &User) -> Value {
    json!({"id": u.id, "username": u.username, "admin": u.admin, "approved": u.approved, "created": u.created})
}

// ================================================================ вход

#[derive(Deserialize)]
struct Credentials {
    username: String,
    password: String,
}

async fn register(State(app): St, headers: HeaderMap, Json(c): Json<Credentials>) -> Result<Response, ApiErr> {
    if !headers.contains_key("x-mcsm") {
        return Err(ApiErr(StatusCode::FORBIDDEN, "Запрос отклонён".into()));
    }
    let username = c.username.trim().to_string();
    auth::validate_username(&username)?;
    auth::validate_password(&c.password)?;
    let pw = c.password.clone();
    let hash = blocking(move || auth::hash_password(&pw)).await??;
    let user = {
        let mut d = app.data.lock().unwrap();
        if d.users.iter().any(|u| u.username.to_lowercase() == username.to_lowercase()) {
            return Err(bad("Такой логин уже занят"));
        }
        let first = d.users.is_empty();
        let u = User {
            username,
            pass_hash: hash,
            admin: first,
            approved: first || d.settings.open_registration,
            ..Default::default()
        };
        d.users.push(u.clone());
        state::save_data(&app.data_dir, &d);
        u
    };
    if !user.approved {
        return Ok(Json(json!({"ok": true, "pending": true})).into_response());
    }
    let cookie = auth::new_session(&app, &user.id, auth::is_https(&headers));
    Ok(([(header::SET_COOKIE, cookie)], Json(json!({"ok": true, "user": user_json(&user)}))).into_response())
}

async fn login(State(app): St, headers: HeaderMap, Json(c): Json<Credentials>) -> Result<Response, ApiErr> {
    if !headers.contains_key("x-mcsm") {
        return Err(ApiErr(StatusCode::FORBIDDEN, "Запрос отклонён".into()));
    }
    let key = c.username.trim().to_lowercase();
    if let Some(sec) = auth::login_blocked(&app, &key) {
        return Err(ApiErr(StatusCode::TOO_MANY_REQUESTS, format!("Слишком много попыток. Подождите {} с", sec)));
    }
    let user = app.data.lock().unwrap().users.iter().find(|u| u.username.to_lowercase() == key).cloned();
    let pw = c.password.clone();
    // хешируем даже для несуществующего логина, чтобы по времени ответа нельзя было угадать логины
    let hash = user.as_ref().map(|u| u.pass_hash.clone()).unwrap_or_else(|| {
        "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHRzb21lc2FsdA$Y3/YH3ePNoWKHXcnDl5H5QX6gk1Ji6qTiXOpJWmc0Bw".into()
    });
    let good = blocking(move || auth::verify_password(&pw, &hash)).await?;
    let Some(user) = user.filter(|_| good) else {
        auth::login_failed(&app, &key);
        return Err(ApiErr(StatusCode::UNAUTHORIZED, "Неверный логин или пароль".into()));
    };
    auth::login_ok(&app, &key);
    if !user.approved {
        return Err(ApiErr(StatusCode::FORBIDDEN, "Аккаунт ждёт одобрения администратора".into()));
    }
    let cookie = auth::new_session(&app, &user.id, auth::is_https(&headers));
    Ok(([(header::SET_COOKIE, cookie)], Json(json!({"ok": true, "user": user_json(&user)}))).into_response())
}

async fn logout(State(app): St, headers: HeaderMap) -> Response {
    if let Some(t) = auth::session_token(&headers) {
        auth::drop_session(&app, &t);
    }
    ([(header::SET_COOKIE, auth::clear_cookie())], Json(json!({"ok": true}))).into_response()
}

async fn me(State(app): St, CurrentUser(u): CurrentUser) -> Res {
    let need_approval = app.data.lock().unwrap().users.iter().filter(|x| !x.approved).count();
    Ok(Json(json!({"user": user_json(&u), "pending_users": if u.admin { need_approval } else { 0 }})))
}

#[derive(Deserialize)]
struct PwChange {
    old: String,
    new: String,
}

async fn change_password(State(app): St, CurrentUser(u): CurrentUser, Json(b): Json<PwChange>) -> Res {
    auth::validate_password(&b.new)?;
    let (old, hash) = (b.old.clone(), u.pass_hash.clone());
    if !blocking(move || auth::verify_password(&old, &hash)).await? {
        return Err(bad("Текущий пароль неверный"));
    }
    let new = b.new.clone();
    let h = blocking(move || auth::hash_password(&new)).await??;
    let mut d = app.data.lock().unwrap();
    if let Some(x) = d.users.iter_mut().find(|x| x.id == u.id) {
        x.pass_hash = h;
    }
    state::save_data(&app.data_dir, &d);
    ok()
}

// ================================================================ серверы

async fn servers_list(State(app): St, CurrentUser(u): CurrentUser) -> Res {
    let mine: Vec<Server> =
        app.data.lock().unwrap().servers.iter().filter(|s| s.owner == u.id).map(|s| s.server.clone()).collect();
    let list: Vec<Value> = mine.iter().map(|s| server_json(&app, s)).collect();
    let t = app.tunnel(&u.id);
    Ok(Json(json!({
        "servers": list,
        "tunnel": {"state": tunnel_state_str(t.state()), "address": tunnel_address(&u.tunnel.command)},
    })))
}

async fn server_get(State(app): St, CurrentUser(u): CurrentUser, UrlPath(id): UrlPath<String>) -> Res {
    let s = my_server(&app, &u, &id)?;
    Ok(Json(server_json(&app, &s)))
}

/// Проверка настроек сервера перед сохранением.
fn validate_server(app: &App, u: &User, s: &mut Server) -> Result<(), ApiErr> {
    s.name = s.name.trim().to_string();
    if s.name.is_empty() {
        return Err(bad("Укажите название сервера"));
    }
    s.path = s.path.trim().to_string();
    if !Path::new(&s.path).is_dir() {
        return Err(bad(format!("Папка сервера не найдена: {}", s.path)));
    }
    check_path(app, u, &s.path)?;
    if !s.backup_dir.trim().is_empty() {
        check_path(app, u, &s.backup_dir)?;
    }
    if s.launch_mode != "args" {
        s.launch_mode = "jar".into();
    }
    // одну папку не может держать сервер другого пользователя
    let taken = app
        .data
        .lock()
        .unwrap()
        .servers
        .iter()
        .any(|o| o.owner != u.id && same_path(&o.server.path, &s.path));
    if taken {
        return Err(bad("Эта папка уже используется сервером другого пользователя"));
    }
    if !u.admin {
        let java = s.java.trim().to_string();
        if !(java.is_empty() || java == "java" || find_java().contains(&java)) {
            return Err(bad("Выберите Java из найденных на компьютере (свой путь может указать только администратор)"));
        }
    }
    Ok(())
}

async fn server_create(State(app): St, CurrentUser(u): CurrentUser, Json(mut s): Json<Server>) -> Res {
    s.id = uuid::Uuid::new_v4().simple().to_string();
    let app2 = app.clone();
    let u2 = u.clone();
    let s = blocking(move || -> Result<Server, ApiErr> {
        validate_server(&app2, &u2, &mut s)?;
        if s.jar.is_empty() && s.custom_args.is_empty() {
            let (mode, jar, args) = detect_launch(&s.path);
            s.launch_mode = mode;
            s.jar = jar;
            s.custom_args = args;
        }
        Ok(s)
    })
    .await??;
    {
        let mut d = app.data.lock().unwrap();
        d.servers.push(OwnedServer { owner: u.id.clone(), server: s.clone() });
        state::save_data(&app.data_dir, &d);
    }
    Ok(Json(server_json(&app, &s)))
}

async fn server_update(State(app): St, CurrentUser(u): CurrentUser, UrlPath(id): UrlPath<String>, Json(mut s): Json<Server>) -> Res {
    let old = my_server(&app, &u, &id)?;
    s.id = old.id.clone();
    let running = app.proc(&id).running();
    if running && !same_path(&old.path, &s.path) {
        return Err(bad("Нельзя менять папку работающего сервера"));
    }
    let (app2, u2) = (app.clone(), u.clone());
    let s = blocking(move || validate_server(&app2, &u2, &mut s).map(|_| s)).await??;
    {
        let mut d = app.data.lock().unwrap();
        if let Some(x) = d.servers.iter_mut().find(|x| x.server.id == id) {
            x.server = s.clone();
        }
        state::save_data(&app.data_dir, &d);
    }
    Ok(Json(server_json(&app, &s)))
}

async fn server_delete(State(app): St, CurrentUser(u): CurrentUser, UrlPath(id): UrlPath<String>) -> Res {
    my_server(&app, &u, &id)?;
    if app.proc(&id).running() {
        return Err(bad("Сначала остановите сервер"));
    }
    {
        let mut d = app.data.lock().unwrap();
        d.servers.retain(|s| s.server.id != id);
        state::save_data(&app.data_dir, &d);
    }
    app.procs.lock().unwrap().remove(&id);
    app.stats.lock().unwrap().remove(&id);
    ok()
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct StartReq {
    accept_eula: bool,
    stop_other: bool,
}

async fn server_start(State(app): St, CurrentUser(u): CurrentUser, UrlPath(id): UrlPath<String>, body: Option<Json<StartReq>>) -> Res {
    let req = body.map(|b| b.0).unwrap_or_default();
    let s = my_server(&app, &u, &id)?;
    if let Some(b) = app.is_busy(&id) {
        return Err(bad(format!("Подождите: {}", b)));
    }
    if app.proc(&id).running() {
        return Err(bad("Сервер уже запущен"));
    }
    if !Path::new(&s.path).is_dir() {
        return Err(bad(format!("Папка сервера не найдена: {}", s.path)));
    }
    if !eula_accepted(&s.path) {
        if !req.accept_eula {
            return Ok(Json(json!({"need_eula": true})));
        }
        accept_eula(&s.path)?;
    }
    // два сервера на одном порту не запустятся
    let port = props::server_port(&s.path);
    let running: Vec<(String, u16)> = {
        let procs = app.procs.lock().unwrap();
        procs.values().filter(|p| p.running() && p.sid != id).map(|p| p.sid.clone()).collect::<Vec<_>>()
    }
    .into_iter()
    .filter_map(|sid| app.server(&sid).map(|o| (sid, props::server_port(&o.server.path))))
    .collect();
    if let Some((other, _)) = running.iter().find(|(_, p)| *p == port) {
        let o = app.server(other).unwrap();
        if o.owner != u.id {
            return Err(bad(format!("Порт {} занят сервером другого пользователя", port)));
        }
        if !req.stop_other {
            return Ok(Json(json!({"conflict": {"id": other, "name": o.server.name, "port": port}})));
        }
        let p = app.proc(other);
        p.restart_after_stop.store(false, Ordering::SeqCst);
        app.start_after.lock().unwrap().insert(other.clone(), id.clone());
        p.stop(app.stop_timeout());
        return Ok(Json(json!({"ok": true, "queued": true})));
    }
    start_now(&app, &id)?;
    ok()
}

async fn server_stop(State(app): St, CurrentUser(u): CurrentUser, UrlPath(id): UrlPath<String>) -> Res {
    my_server(&app, &u, &id)?;
    let p = app.proc(&id);
    if !p.running() {
        return Err(bad("Сервер не запущен"));
    }
    p.restart_after_stop.store(false, Ordering::SeqCst);
    app.start_after.lock().unwrap().remove(&id);
    p.stop(app.stop_timeout());
    ok()
}

async fn server_restart(State(app): St, CurrentUser(u): CurrentUser, UrlPath(id): UrlPath<String>) -> Res {
    my_server(&app, &u, &id)?;
    let p = app.proc(&id);
    if p.running() {
        p.restart_after_stop.store(true, Ordering::SeqCst);
        p.stop(app.stop_timeout());
        ok()
    } else {
        start_now(&app, &id)?;
        ok()
    }
}

async fn server_kill(State(app): St, CurrentUser(u): CurrentUser, UrlPath(id): UrlPath<String>) -> Res {
    my_server(&app, &u, &id)?;
    let p = app.proc(&id);
    p.restart_after_stop.store(false, Ordering::SeqCst);
    p.kill();
    ok()
}

// ================================================================ консоль

fn level_str(l: Level) -> &'static str {
    match l {
        Level::Normal => "n",
        Level::Warn => "w",
        Level::Error => "e",
        Level::Info => "i",
        Level::Cmd => "c",
    }
}

async fn console(State(app): St, CurrentUser(u): CurrentUser, UrlPath(id): UrlPath<String>, Query(q): Query<HashMap<String, String>>) -> Res {
    my_server(&app, &u, &id)?;
    let since: Option<u64> = q.get("since").and_then(|s| s.parse().ok());
    let p = app.proc(&id);
    let sh = p.shared.lock().unwrap();
    let counter = sh.log_counter;
    let len = sh.log.len() as u64;
    let (reset, take) = match since {
        Some(s) if s <= counter && counter - s <= len => (false, (counter - s) as usize),
        _ => (true, len.min(2000) as usize),
    };
    let lines: Vec<Value> = sh.log.iter().skip(sh.log.len() - take).map(|l| json!([level_str(l.level), l.text])).collect();
    Ok(Json(json!({"counter": counter, "reset": reset, "lines": lines, "status": status_str(sh.status)})))
}

async fn console_clear(State(app): St, CurrentUser(u): CurrentUser, UrlPath(id): UrlPath<String>) -> Res {
    my_server(&app, &u, &id)?;
    app.proc(&id).clear_log();
    ok()
}

#[derive(Deserialize)]
struct Cmd {
    command: String,
}

async fn command(State(app): St, CurrentUser(u): CurrentUser, UrlPath(id): UrlPath<String>, Json(c): Json<Cmd>) -> Res {
    my_server(&app, &u, &id)?;
    app.proc(&id).send(&c.command)?;
    ok()
}

// ================================================================ статистика

async fn stats(State(app): St, CurrentUser(u): CurrentUser, UrlPath(id): UrlPath<String>) -> Res {
    let s = my_server(&app, &u, &id)?;
    let p = app.proc(&id);
    let (st, hist) = app.stats.lock().unwrap().get(&id).cloned().unwrap_or_default();
    let mut players = p.players();
    if players.is_empty() {
        players = st.ping.as_ref().map(|x| x.players.clone()).unwrap_or_default();
    }
    let path = s.path.clone();
    let world = blocking(move || {
        backup::world_dirs(&path).iter().map(|d| backup::dir_size(&Path::new(&path).join(d))).sum::<u64>()
    })
    .await?;
    let running = p.running();
    Ok(Json(json!({
        "status": status_str(p.status()),
        "status_ru": p.status().ru(),
        "uptime": p.uptime().map(|d| d.as_secs()),
        "running": running,
        "players": players,
        "stats": if running { serde_json::to_value(&st).unwrap_or(Value::Null) } else { Value::Null },
        "history": hist,
        "world_size": world,
        "xmx": s.xmx,
    })))
}

// ================================================================ списки

fn kind_of(s: &str) -> Result<ListKind, ApiErr> {
    Ok(match s {
        "ops" => ListKind::Ops,
        "whitelist" => ListKind::Whitelist,
        "banned-players" => ListKind::BannedPlayers,
        "banned-ips" => ListKind::BannedIps,
        _ => return Err(bad("Неизвестный список")),
    })
}

fn kind_key(k: ListKind) -> &'static str {
    match k {
        ListKind::Ops => "ops",
        ListKind::Whitelist => "whitelist",
        ListKind::BannedPlayers => "banned-players",
        ListKind::BannedIps => "banned-ips",
    }
}

async fn lists_get(State(app): St, CurrentUser(u): CurrentUser, UrlPath(id): UrlPath<String>) -> Res {
    let s = my_server(&app, &u, &id)?;
    let mut out = serde_json::Map::new();
    for k in lists::ALL {
        let cols: Vec<Value> = k.columns().iter().map(|(f, t)| json!([f, t])).collect();
        let rows: Vec<Value> = lists::load(&s.path, k)
            .iter()
            .map(|e| Value::Object(k.columns().iter().map(|(f, _)| (f.to_string(), json!(lists::field(e, f)))).collect()))
            .collect();
        out.insert(kind_key(k).into(), json!({"title": k.title(), "key": k.key(), "columns": cols, "rows": rows}));
    }
    let p = Properties::load(&s.path);
    Ok(Json(json!({
        "lists": out,
        "online_mode": p.get_bool("online-mode", true),
        "whitelist_on": p.get_bool("white-list", false),
        "running": app.proc(&id).running(),
    })))
}

#[derive(Deserialize)]
struct ListAdd {
    kind: String,
    name: String,
    #[serde(default)]
    reason: String,
}

async fn lists_add(State(app): St, CurrentUser(u): CurrentUser, UrlPath(id): UrlPath<String>, Json(b): Json<ListAdd>) -> Res {
    let s = my_server(&app, &u, &id)?;
    let k = kind_of(&b.kind)?;
    let name = b.name.trim().to_string();
    if name.is_empty() || name.contains(char::is_whitespace) {
        return Err(bad("Укажите ник или IP без пробелов"));
    }
    let p = app.proc(&id);
    if p.running() {
        p.send(&k.command(true, &name, b.reason.trim()))?;
        return Ok(Json(json!({"ok": true, "via_console": true})));
    }
    let online = Properties::load(&s.path).get_bool("online-mode", true);
    let reason = b.reason.clone();
    blocking(move || lists::add_offline(&s.path, k, &name, online, &reason)).await??;
    ok()
}

#[derive(Deserialize)]
struct ListRemove {
    kind: String,
    names: Vec<String>,
}

async fn lists_remove(State(app): St, CurrentUser(u): CurrentUser, UrlPath(id): UrlPath<String>, Json(b): Json<ListRemove>) -> Res {
    let s = my_server(&app, &u, &id)?;
    let k = kind_of(&b.kind)?;
    let p = app.proc(&id);
    for n in &b.names {
        if p.running() {
            p.send(&k.command(false, n, ""))?;
        } else {
            lists::remove_offline(&s.path, k, n)?;
        }
    }
    Ok(Json(json!({"ok": true, "via_console": p.running()})))
}

// ================================================================ server.properties

async fn props_get(State(app): St, CurrentUser(u): CurrentUser, UrlPath(id): UrlPath<String>) -> Res {
    let s = my_server(&app, &u, &id)?;
    let p = Properties::load(&s.path);
    let mut items = vec![];
    for (key, desc, kind) in KNOWN {
        if let Some(v) = p.get(key) {
            items.push(prop_json(key, v, desc, *kind));
        }
    }
    for (k, v) in p.items() {
        if props::known(k).is_none() {
            items.push(prop_json(k, v, "", Kind::Str));
        }
    }
    Ok(Json(json!({"exists": p.exists, "items": items, "running": app.proc(&id).running()})))
}

fn prop_json(key: &str, value: &str, desc: &str, kind: Kind) -> Value {
    let (t, choices): (&str, Vec<&str>) = match kind {
        Kind::Str => ("str", vec![]),
        Kind::Int => ("int", vec![]),
        Kind::Bool => ("bool", vec![]),
        Kind::Choice(c) => ("choice", c.to_vec()),
    };
    json!({"key": key, "value": value, "desc": desc, "type": t, "choices": choices})
}

#[derive(Deserialize)]
struct PropsPut {
    values: HashMap<String, String>,
}

async fn props_put(State(app): St, CurrentUser(u): CurrentUser, UrlPath(id): UrlPath<String>, Json(b): Json<PropsPut>) -> Res {
    let s = my_server(&app, &u, &id)?;
    let mut p = Properties::load(&s.path);
    for (k, v) in &b.values {
        let k = k.trim();
        if k.is_empty() || k.contains(['=', ':', '\n', '#']) || k.contains(char::is_whitespace) {
            return Err(bad(format!("Недопустимый ключ: {}", k)));
        }
        p.set(k, v);
    }
    p.save()?;
    Ok(Json(json!({"ok": true, "restart_needed": app.proc(&id).running()})))
}

// ================================================================ бэкапы и мир

async fn backups_list(State(app): St, CurrentUser(u): CurrentUser, UrlPath(id): UrlPath<String>) -> Res {
    let s = my_server(&app, &u, &id)?;
    let s2 = s.clone();
    let list = blocking(move || backup::list_backups(&s2)).await?;
    let items: Vec<Value> = list
        .iter()
        .map(|b| {
            let t: chrono::DateTime<chrono::Local> = b.mtime.into();
            json!({"name": b.name, "size": b.size, "time": t.format("%Y-%m-%d %H:%M:%S").to_string()})
        })
        .collect();
    Ok(Json(json!({
        "dir": backup::backup_dir(&s).to_string_lossy(),
        "backups": items,
        "worlds": backup::world_dirs(&s.path),
        "level_name": props::level_name(&s.path),
    })))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct BackupReq {
    label: String,
}

async fn backups_create(State(app): St, CurrentUser(u): CurrentUser, UrlPath(id): UrlPath<String>, Json(b): Json<BackupReq>) -> Res {
    my_server(&app, &u, &id)?;
    let sid = id.clone();
    let label = b.label.trim().to_string();
    app.run_bg(&id, "Создаю бэкап…", move |app| state::backup_now(app, &sid, &label))?;
    Ok(Json(json!({"ok": true, "background": true})))
}

#[derive(Deserialize)]
struct NameReq {
    name: String,
}

async fn backups_restore(State(app): St, CurrentUser(u): CurrentUser, UrlPath(id): UrlPath<String>, Json(b): Json<NameReq>) -> Res {
    let s = my_server(&app, &u, &id)?;
    if app.proc(&id).running() {
        return Err(bad("Остановите сервер перед восстановлением"));
    }
    let zip = backup::backup_dir(&s).join(plain_name(&b.name)?);
    if !zip.is_file() {
        return Err(bad("Бэкап не найден"));
    }
    app.run_bg(&id, "Восстанавливаю мир…", move |_| {
        if !backup::world_dirs(&s.path).is_empty() {
            backup::make_backup(&s, "before-restore", None).map_err(|e| format!("Не удалось сохранить текущий мир: {}", e))?;
        }
        let (_, main) = backup::restore_backup(&s, &zip).map_err(|e| e.to_string())?;
        let mut msg = "Мир восстановлен (текущий мир сохранён в бэкап before-restore)".to_string();
        if let Some(m) = main {
            if m != props::level_name(&s.path) {
                let mut p = Properties::load(&s.path);
                p.set("level-name", &m);
                let _ = p.save();
                msg += &format!(", level-name = {}", m);
            }
        }
        Ok(msg)
    })?;
    Ok(Json(json!({"ok": true, "background": true})))
}

#[derive(Deserialize)]
struct NamesReq {
    names: Vec<String>,
}

async fn backups_delete(State(app): St, CurrentUser(u): CurrentUser, UrlPath(id): UrlPath<String>, Json(b): Json<NamesReq>) -> Res {
    let s = my_server(&app, &u, &id)?;
    let dir = backup::backup_dir(&s);
    for n in &b.names {
        let n = plain_name(n)?;
        if !n.to_lowercase().ends_with(".zip") {
            return Err(bad("Можно удалять только .zip"));
        }
        fs::remove_file(dir.join(n))?;
    }
    ok()
}

async fn backups_download(State(app): St, CurrentUser(u): CurrentUser, UrlPath((id, name)): UrlPath<(String, String)>) -> Result<Response, ApiErr> {
    let s = my_server(&app, &u, &id)?;
    let path = backup::backup_dir(&s).join(plain_name(&name)?);
    let f = tokio::fs::File::open(&path).await.map_err(|_| ApiErr(StatusCode::NOT_FOUND, "Бэкап не найден".into()))?;
    let len = f.metadata().await.map(|m| m.len()).unwrap_or(0);
    let body = Body::from_stream(tokio_util::io::ReaderStream::new(f));
    let ascii: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '_' }).collect();
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (header::CONTENT_LENGTH, len.to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{}\"", ascii)),
        ],
        body,
    )
        .into_response())
}

#[derive(Deserialize)]
struct CopyReq {
    to: String,
}

async fn world_copy(State(app): St, CurrentUser(u): CurrentUser, UrlPath(id): UrlPath<String>, Json(b): Json<CopyReq>) -> Res {
    let src = my_server(&app, &u, &id)?;
    let dst = my_server(&app, &u, &b.to)?;
    if src.id == dst.id {
        return Err(bad("Выберите другой сервер"));
    }
    if app.proc(&dst.id).running() {
        return Err(bad(format!("Остановите «{}» перед заменой мира", dst.name)));
    }
    let to = dst.id.clone();
    app.run_bg(&to, "Копирую мир…", move |app| {
        if !backup::world_dirs(&dst.path).is_empty() {
            backup::make_backup(&dst, "before-copy", None).map_err(|e| e.to_string())?;
        }
        let p = app.proc(&src.id);
        let flushed = crate::core::server::flush_world(&p);
        let r = backup::copy_world(&src, &dst);
        if flushed {
            let _ = p.send("save-on");
        }
        r.map_err(|e| e.to_string())?;
        Ok(format!("Мир скопирован из «{}»", src.name))
    })?;
    Ok(Json(json!({"ok": true, "background": true})))
}

#[derive(Deserialize)]
struct PathReq {
    path: String,
}

async fn world_import(State(app): St, CurrentUser(u): CurrentUser, UrlPath(id): UrlPath<String>, Json(b): Json<PathReq>) -> Res {
    let s = my_server(&app, &u, &id)?;
    check_path(&app, &u, &b.path)?;
    if app.proc(&id).running() {
        return Err(bad("Остановите сервер перед импортом мира"));
    }
    let folder = PathBuf::from(b.path.trim());
    if !folder.join("level.dat").exists() {
        return Err(bad("В выбранной папке нет level.dat — это не мир Minecraft"));
    }
    app.run_bg(&id, "Импортирую мир…", move |_| {
        if !backup::world_dirs(&s.path).is_empty() {
            backup::make_backup(&s, "before-import", None).map_err(|e| e.to_string())?;
        }
        backup::import_world(&s, &folder).map_err(|e| e.to_string())?;
        Ok("Мир импортирован (прежний мир сохранён в бэкап before-import)".into())
    })?;
    Ok(Json(json!({"ok": true, "background": true})))
}

// ================================================================ моды

async fn mods_list(State(app): St, CurrentUser(u): CurrentUser, UrlPath(id): UrlPath<String>) -> Res {
    let s = my_server(&app, &u, &id)?;
    let path = s.path.clone();
    let list = blocking(move || mods::list_mods(&path)).await?;
    let items: Vec<Value> = list
        .iter()
        .map(|m| json!({"folder": m.folder, "file": m.file, "enabled": m.enabled, "name": m.name,
                        "version": m.version, "loader": m.loader, "desc": m.desc, "size": m.size}))
        .collect();
    Ok(Json(json!({"mods": items, "dirs": mods::mod_dirs(&s.path), "running": app.proc(&id).running()})))
}

#[derive(Deserialize)]
struct ModRef {
    folder: String,
    file: String,
}

fn mod_folder(f: &str) -> Result<&'static str, ApiErr> {
    match f {
        "mods" => Ok("mods"),
        "plugins" => Ok("plugins"),
        _ => Err(bad("Папка должна быть mods или plugins")),
    }
}

/// Текущий путь мода: x.jar или x.jar.disabled.
fn mod_path(dir: &str, m: &ModRef) -> Result<PathBuf, ApiErr> {
    let base = Path::new(dir).join(mod_folder(&m.folder)?).join(plain_name(&m.file)?);
    if base.exists() {
        return Ok(base);
    }
    let dis = PathBuf::from(format!("{}{}", base.to_string_lossy(), mods::DISABLED));
    if dis.exists() {
        Ok(dis)
    } else {
        Err(bad(format!("Файл не найден: {}", m.file)))
    }
}

#[derive(Deserialize)]
struct ModToggle {
    items: Vec<ModRef>,
    enabled: bool,
}

async fn mods_toggle(State(app): St, CurrentUser(u): CurrentUser, UrlPath(id): UrlPath<String>, Json(b): Json<ModToggle>) -> Res {
    let s = my_server(&app, &u, &id)?;
    let mut errors = vec![];
    for m in &b.items {
        let p = mod_path(&s.path, m)?;
        if let Err(e) = mods::set_enabled(&p, b.enabled) {
            errors.push(format!("{}: {}", m.file, e));
        }
    }
    if !errors.is_empty() {
        return Err(bad(errors.join("\n")));
    }
    Ok(Json(json!({"ok": true, "restart_needed": app.proc(&id).running()})))
}

async fn mods_delete(State(app): St, CurrentUser(u): CurrentUser, UrlPath(id): UrlPath<String>, Json(b): Json<ModToggle>) -> Res {
    let s = my_server(&app, &u, &id)?;
    for m in &b.items {
        fs::remove_file(mod_path(&s.path, m)?)?;
    }
    ok()
}

async fn mods_upload(State(app): St, CurrentUser(u): CurrentUser, UrlPath(id): UrlPath<String>, mut mp: Multipart) -> Res {
    let s = my_server(&app, &u, &id)?;
    let mut folder = "mods";
    let mut saved = vec![];
    while let Some(field) = mp.next_field().await.map_err(|e| bad(e.to_string()))? {
        if field.name() == Some("folder") {
            folder = mod_folder(&field.text().await.map_err(|e| bad(e.to_string()))?)?;
            continue;
        }
        let Some(fname) = field.file_name().map(|s| s.to_string()) else { continue };
        let fname = Path::new(&fname.replace('\\', "/")).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if !fname.to_lowercase().ends_with(".jar") {
            return Err(bad(format!("{}: можно загружать только .jar", fname)));
        }
        let data = field.bytes().await.map_err(|e| bad(e.to_string()))?;
        let dir = Path::new(&s.path).join(folder);
        fs::create_dir_all(&dir)?;
        fs::write(dir.join(plain_name(&fname)?), &data)?;
        saved.push(fname);
    }
    Ok(Json(json!({"ok": true, "saved": saved, "restart_needed": app.proc(&id).running()})))
}

// ================================================================ помощники для форм

async fn detect(CurrentUser(u): CurrentUser, State(app): St, Query(q): Query<HashMap<String, String>>) -> Res {
    let path = q.get("path").cloned().unwrap_or_default();
    check_path(&app, &u, &path)?;
    if !Path::new(&path).is_dir() {
        return Err(bad("Папка не найдена"));
    }
    let r = blocking(move || {
        let (mode, jar, args) = detect_launch(&path);
        json!({"launch_mode": mode, "jar": jar, "custom_args": args, "jars": list_jars(&path),
               "eula": eula_accepted(&path),
               "name": Path::new(&path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()})
    })
    .await?;
    Ok(Json(r))
}

async fn java(CurrentUser(_u): CurrentUser) -> Res {
    let list = blocking(find_java).await?;
    let items: Vec<Value> = list.iter().map(|j| json!({"path": j, "label": launch::java_label(j)})).collect();
    Ok(Json(json!({"java": items})))
}

fn roots(app: &App, u: &User) -> Vec<String> {
    let allowed = app.data.lock().unwrap().settings.allowed_roots.clone();
    if !u.admin && !allowed.is_empty() {
        return allowed;
    }
    if cfg!(windows) {
        (b'A'..=b'Z').map(|c| format!("{}:\\", c as char)).filter(|d| Path::new(d).exists()).collect()
    } else {
        let mut v = vec!["/".to_string()];
        if let Some(h) = dirs::home_dir() {
            v.insert(0, h.to_string_lossy().to_string());
        }
        v
    }
}

/// Обзор папок на ПК с панелью: для выбора папки сервера, бэкапов, мира.
async fn fs_list(State(app): St, CurrentUser(u): CurrentUser, Query(q): Query<HashMap<String, String>>) -> Res {
    let path = q.get("path").cloned().unwrap_or_default();
    if path.trim().is_empty() {
        return Ok(Json(json!({"path": "", "parent": null, "roots": roots(&app, &u), "dirs": [], "files": []})));
    }
    check_path(&app, &u, &path)?;
    let p = PathBuf::from(&path);
    if !p.is_dir() {
        return Err(bad("Папка не найдена"));
    }
    let parent = p
        .parent()
        .map(|x| x.to_string_lossy().to_string())
        .filter(|x| !x.is_empty() && check_path(&app, &u, x).is_ok());
    let mut dirs = vec![];
    let mut files = vec![];
    for e in fs::read_dir(&p)?.filter_map(|e| e.ok()) {
        let name = e.file_name().to_string_lossy().to_string();
        match e.metadata() {
            Ok(m) if m.is_dir() => dirs.push(name),
            Ok(m) => files.push(json!({"name": name, "size": m.len()})),
            Err(_) => {}
        }
    }
    dirs.sort_by_key(|d| d.to_lowercase());
    files.sort_by_key(|f| f["name"].as_str().unwrap_or("").to_lowercase());
    Ok(Json(json!({
        "path": p.to_string_lossy(),
        "parent": parent,
        "roots": roots(&app, &u),
        "dirs": dirs,
        "files": files,
        "is_server": p.join("server.properties").exists() || !list_jars(&path).is_empty(),
        "is_world": p.join("level.dat").exists(),
    })))
}

// ================================================================ туннель

fn tunnel_json(t: &crate::core::tunnel::Tunnel, settings: &TunnelSettings) -> Value {
    let (_, log) = t.log_lines();
    let tail: Vec<String> = log.iter().rev().take(200).rev().cloned().collect();
    json!({
        "state": tunnel_state_str(t.state()),
        "wanted": t.wanted(),
        "settings": settings,
        "address": tunnel_address(&settings.command),
        "log": tail,
    })
}

async fn tunnel_get(State(app): St, CurrentUser(u): CurrentUser) -> Res {
    Ok(Json(tunnel_json(&app.tunnel(&u.id), &u.tunnel)))
}

async fn tunnel_put(State(app): St, CurrentUser(u): CurrentUser, Json(mut ts): Json<TunnelSettings>) -> Res {
    ts.new_window = false;
    {
        let mut d = app.data.lock().unwrap();
        if let Some(x) = d.users.iter_mut().find(|x| x.id == u.id) {
            x.tunnel = ts.clone();
        }
        state::save_data(&app.data_dir, &d);
    }
    app.tunnel(&u.id).set_settings(ts.clone());
    Ok(Json(tunnel_json(&app.tunnel(&u.id), &ts)))
}

async fn tunnel_start(State(app): St, CurrentUser(u): CurrentUser) -> Res {
    let t = app.tunnel(&u.id);
    let mut ts = u.tunnel.clone();
    ts.new_window = false;
    t.set_settings(ts);
    t.start()?;
    Ok(Json(tunnel_json(&t, &u.tunnel)))
}

async fn tunnel_stop(State(app): St, CurrentUser(u): CurrentUser) -> Res {
    let t = app.tunnel(&u.id);
    t.stop();
    Ok(Json(tunnel_json(&t, &u.tunnel)))
}

// ================================================================ администрирование

async fn admin_users(State(app): St, Admin(_): Admin) -> Res {
    let d = app.data.lock().unwrap();
    let users: Vec<Value> = d
        .users
        .iter()
        .map(|u| {
            let mut v = user_json(u);
            v["servers"] = json!(d.servers.iter().filter(|s| s.owner == u.id).count());
            v
        })
        .collect();
    Ok(Json(json!({"users": users})))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct UserUpdate {
    approved: Option<bool>,
    admin: Option<bool>,
    password: Option<String>,
}

async fn admin_user_update(State(app): St, Admin(me): Admin, UrlPath(uid): UrlPath<String>, Json(b): Json<UserUpdate>) -> Res {
    if uid == me.id && (b.admin == Some(false) || b.approved == Some(false)) {
        return Err(bad("Нельзя снять права с самого себя"));
    }
    let hash = match b.password.clone() {
        Some(p) => {
            auth::validate_password(&p)?;
            Some(blocking(move || auth::hash_password(&p)).await??)
        }
        None => None,
    };
    let mut d = app.data.lock().unwrap();
    let u = d.users.iter_mut().find(|u| u.id == uid).ok_or_else(|| bad("Пользователь не найден"))?;
    if let Some(a) = b.approved {
        u.approved = a;
    }
    if let Some(a) = b.admin {
        u.admin = a;
    }
    if let Some(h) = hash {
        u.pass_hash = h;
    }
    if b.approved == Some(false) || b.password.is_some() {
        d.sessions.retain(|s| s.user != uid);
    }
    state::save_data(&app.data_dir, &d);
    ok()
}

async fn admin_user_delete(State(app): St, Admin(me): Admin, UrlPath(uid): UrlPath<String>) -> Res {
    if uid == me.id {
        return Err(bad("Нельзя удалить самого себя"));
    }
    let theirs: Vec<String> =
        app.data.lock().unwrap().servers.iter().filter(|s| s.owner == uid).map(|s| s.server.id.clone()).collect();
    if theirs.iter().any(|sid| app.proc(sid).running()) {
        return Err(bad("У пользователя запущен сервер — сначала остановите его"));
    }
    if let Some(t) = app.tunnels.lock().unwrap().remove(&uid) {
        t.stop();
    }
    let mut d = app.data.lock().unwrap();
    d.users.retain(|u| u.id != uid);
    d.servers.retain(|s| s.owner != uid);
    d.sessions.retain(|s| s.user != uid);
    state::save_data(&app.data_dir, &d);
    ok()
}

fn settings_json(app: &App) -> Value {
    let s = app.data.lock().unwrap().settings.clone();
    json!({
        "open_registration": s.open_registration,
        "allowed_roots": s.allowed_roots,
        "stop_timeout": s.stop_timeout,
        "panel_tunnel_autostart": s.panel_tunnel_autostart,
        "panel_tunnel": tunnel_json(&app.panel_tunnel, &s.panel_tunnel),
        "port": app.port,
        "data_dir": app.data_dir.to_string_lossy(),
    })
}

async fn admin_settings(State(app): St, Admin(_): Admin) -> Res {
    Ok(Json(settings_json(&app)))
}

#[derive(Deserialize)]
struct SettingsPut {
    open_registration: bool,
    allowed_roots: Vec<String>,
    stop_timeout: u64,
    panel_tunnel_command: String,
    panel_tunnel_autostart: bool,
    panel_tunnel_reconnect: bool,
}

async fn admin_settings_put(State(app): St, Admin(_): Admin, Json(b): Json<SettingsPut>) -> Res {
    {
        let mut d = app.data.lock().unwrap();
        let s = &mut d.settings;
        s.open_registration = b.open_registration;
        s.allowed_roots = b.allowed_roots.iter().map(|r| r.trim().to_string()).filter(|r| !r.is_empty()).collect();
        s.stop_timeout = b.stop_timeout.clamp(10, 600);
        s.panel_tunnel.command = b.panel_tunnel_command.trim().to_string();
        s.panel_tunnel.auto_reconnect = b.panel_tunnel_reconnect;
        s.panel_tunnel.new_window = false;
        s.panel_tunnel_autostart = b.panel_tunnel_autostart;
        app.panel_tunnel.set_settings(s.panel_tunnel.clone());
        state::save_data(&app.data_dir, &d);
    }
    Ok(Json(settings_json(&app)))
}

async fn panel_tunnel_start(State(app): St, Admin(_): Admin) -> Res {
    app.panel_tunnel.start()?;
    Ok(Json(settings_json(&app)))
}

async fn panel_tunnel_stop(State(app): St, Admin(_): Admin) -> Res {
    app.panel_tunnel.stop();
    Ok(Json(settings_json(&app)))
}

/// Переносит серверы из десктоп-версии (config.json) в аккаунт администратора.
async fn import_desktop(State(app): St, Admin(me): Admin) -> Res {
    let cfg = blocking(Config::load).await?;
    let mut d = app.data.lock().unwrap();
    let mut added = 0;
    for s in cfg.servers {
        if d.servers.iter().any(|o| o.server.id == s.id || same_path(&o.server.path, &s.path)) {
            continue;
        }
        d.servers.push(OwnedServer { owner: me.id.clone(), server: s });
        added += 1;
    }
    if added > 0 {
        state::save_data(&app.data_dir, &d);
    }
    Ok(Json(json!({"ok": true, "added": added, "found": d.servers.len()})))
}
