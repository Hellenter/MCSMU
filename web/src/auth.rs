//! Регистрация, вход, сессии (cookie) и проверка прав.

use crate::state::{save_data, App, StoredSession, User};
use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const COOKIE: &str = "mcsm_session";
pub const SESSION_DAYS: i64 = 30;

/// Ошибка API: код + текст для пользователя.
pub struct ApiErr(pub StatusCode, pub String);

impl IntoResponse for ApiErr {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

impl From<String> for ApiErr {
    fn from(s: String) -> Self {
        ApiErr(StatusCode::BAD_REQUEST, s)
    }
}

impl From<&str> for ApiErr {
    fn from(s: &str) -> Self {
        ApiErr(StatusCode::BAD_REQUEST, s.to_string())
    }
}

impl From<std::io::Error> for ApiErr {
    fn from(e: std::io::Error) -> Self {
        ApiErr(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    }
}

pub fn bad(msg: impl Into<String>) -> ApiErr {
    ApiErr(StatusCode::BAD_REQUEST, msg.into())
}

pub fn hash_password(pw: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default().hash_password(pw.as_bytes(), &salt).map(|h| h.to_string()).map_err(|e| e.to_string())
}

pub fn verify_password(pw: &str, hash: &str) -> bool {
    PasswordHash::new(hash).map(|h| Argon2::default().verify_password(pw.as_bytes(), &h).is_ok()).unwrap_or(false)
}

fn sha(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

pub fn validate_username(name: &str) -> Result<(), String> {
    let n = name.chars().count();
    if !(3..=32).contains(&n) {
        return Err("Логин: от 3 до 32 символов".into());
    }
    if !name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.') {
        return Err("Логин: только буквы, цифры, «_», «-» и «.»".into());
    }
    Ok(())
}

pub fn validate_password(pw: &str) -> Result<(), String> {
    if pw.chars().count() < 6 {
        return Err("Пароль: не короче 6 символов".into());
    }
    Ok(())
}

/// Создаёт сессию и возвращает значение заголовка Set-Cookie.
pub fn new_session(app: &App, uid: &str, secure: bool) -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let token = hex::encode(bytes);
    let mut d = app.data.lock().unwrap();
    d.sessions.push(StoredSession {
        hash: sha(&token),
        user: uid.to_string(),
        expires: chrono::Local::now().timestamp() + SESSION_DAYS * 86400,
    });
    save_data(&app.data_dir, &d);
    format!(
        "{}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{}",
        COOKIE,
        token,
        SESSION_DAYS * 86400,
        if secure { "; Secure" } else { "" }
    )
}

pub fn clear_cookie() -> String {
    format!("{}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0", COOKIE)
}

pub fn session_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == COOKIE)
        .map(|(_, v)| v.to_string())
}

pub fn drop_session(app: &App, token: &str) {
    let h = sha(token);
    let mut d = app.data.lock().unwrap();
    d.sessions.retain(|s| s.hash != h);
    save_data(&app.data_dir, &d);
}

/// Пришёл ли запрос через HTTPS-прокси (nginx на домене).
pub fn is_https(headers: &HeaderMap) -> bool {
    headers.get("x-forwarded-proto").and_then(|v| v.to_str().ok()).map(|v| v.eq_ignore_ascii_case("https")).unwrap_or(false)
}

/// Защита от перебора: после 5 неудачных попыток вход по логину блокируется на минуту.
pub fn login_blocked(app: &App, key: &str) -> Option<u64> {
    let f = app.login_fails.lock().unwrap();
    let (n, t) = f.get(key)?;
    if *n >= 5 && t.elapsed() < Duration::from_secs(60) {
        Some(60 - t.elapsed().as_secs())
    } else {
        None
    }
}

pub fn login_failed(app: &App, key: &str) {
    let mut f = app.login_fails.lock().unwrap();
    let e = f.entry(key.to_string()).or_insert((0, Instant::now()));
    if e.1.elapsed() > Duration::from_secs(600) || e.0 >= 5 {
        *e = (0, Instant::now());
    }
    e.0 += 1;
    e.1 = Instant::now();
}

pub fn login_ok(app: &App, key: &str) {
    app.login_fails.lock().unwrap().remove(key);
}

/// Вошедший и одобренный пользователь.
pub struct CurrentUser(pub User);

/// Администратор панели.
pub struct Admin(pub User);

#[axum::async_trait]
impl FromRequestParts<Arc<App>> for CurrentUser {
    type Rejection = ApiErr;

    async fn from_request_parts(parts: &mut Parts, app: &Arc<App>) -> Result<Self, Self::Rejection> {
        // Изменяющие запросы шлёт только наш интерфейс с этим заголовком: так чужой сайт
        // не сможет отправить форму от имени вошедшего пользователя.
        if parts.method != Method::GET && parts.method != Method::HEAD && !parts.headers.contains_key("x-mcsm") {
            return Err(ApiErr(StatusCode::FORBIDDEN, "Запрос отклонён".into()));
        }
        let unauth = || ApiErr(StatusCode::UNAUTHORIZED, "Нужно войти".into());
        let token = session_token(&parts.headers).ok_or_else(unauth)?;
        let h = sha(&token);
        let now = chrono::Local::now().timestamp();
        let d = app.data.lock().unwrap();
        let s = d.sessions.iter().find(|s| s.hash == h && s.expires > now).ok_or_else(unauth)?;
        let u = d.users.iter().find(|u| u.id == s.user).ok_or_else(unauth)?;
        if !u.approved {
            return Err(ApiErr(StatusCode::FORBIDDEN, "Аккаунт ждёт одобрения администратора".into()));
        }
        Ok(CurrentUser(u.clone()))
    }
}

#[axum::async_trait]
impl FromRequestParts<Arc<App>> for Admin {
    type Rejection = ApiErr;

    async fn from_request_parts(parts: &mut Parts, app: &Arc<App>) -> Result<Self, Self::Rejection> {
        let CurrentUser(u) = CurrentUser::from_request_parts(parts, app).await?;
        if !u.admin {
            return Err(ApiErr(StatusCode::FORBIDDEN, "Только для администратора".into()));
        }
        Ok(Admin(u))
    }
}
