//! MC Server Manager Web — панель управления серверами Minecraft в браузере.
//! Запускается на компьютере с серверами; открывается по http://localhost:8080
//! или через домен (nginx на ex1.ru + туннель панели).

mod api;
mod auth;
mod core;
mod state;

use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use std::net::SocketAddr;
use std::path::PathBuf;

const INDEX: &str = include_str!("../static/index.html");
const APP_JS: &str = include_str!("../static/app.js");
const STYLE: &str = include_str!("../static/style.css");

struct Args {
    host: String,
    port: u16,
    data_dir: PathBuf,
    open_browser: bool,
}

fn parse_args() -> Args {
    let mut a = Args {
        host: "0.0.0.0".into(),
        port: std::env::var("MCSM_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8080),
        data_dir: crate::core::config::app_dir(),
        open_browser: cfg!(windows),
    };
    let mut it = std::env::args().skip(1);
    while let Some(x) = it.next() {
        match x.as_str() {
            "--port" | "-p" => a.port = it.next().and_then(|p| p.parse().ok()).unwrap_or(a.port),
            "--host" => a.host = it.next().unwrap_or(a.host),
            "--data-dir" => a.data_dir = it.next().map(PathBuf::from).unwrap_or(a.data_dir),
            "--no-browser" => a.open_browser = false,
            "--help" | "-h" => {
                println!(
                    "MCServerManagerWeb [--port 8080] [--host 0.0.0.0] [--data-dir ПАПКА] [--no-browser]\n\
                     --host 127.0.0.1 — открыть панель только на этом компьютере (и через туннель/домен)"
                );
                std::process::exit(0);
            }
            other => eprintln!("Неизвестный параметр: {}", other),
        }
    }
    a
}

fn page(ct: &'static str, body: &'static str) -> impl IntoResponse {
    ([(header::CONTENT_TYPE, ct), (header::CACHE_CONTROL, "no-cache")], body)
}

#[tokio::main]
async fn main() {
    let args = parse_args();
    let app = state::App::new(args.data_dir.clone(), args.port);
    {
        let a = app.clone();
        std::thread::spawn(move || state::background(a));
    }
    if app.data.lock().unwrap().settings.panel_tunnel_autostart {
        if let Err(e) = app.panel_tunnel.start() {
            eprintln!("Туннель панели не открыт: {}", e);
        }
    }

    let router = api::router(app.clone())
        .route("/", get(|| async { page("text/html; charset=utf-8", INDEX) }))
        .route("/app.js", get(|| async { page("text/javascript; charset=utf-8", APP_JS) }))
        .route("/style.css", get(|| async { page("text/css; charset=utf-8", STYLE) }));

    let addr: SocketAddr = format!("{}:{}", args.host, args.port).parse().unwrap_or_else(|_| {
        eprintln!("Неверный адрес {}:{}", args.host, args.port);
        std::process::exit(2)
    });
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Не удалось занять порт {}: {}\nУкажите другой: --port 8081", args.port, e);
            std::process::exit(1);
        }
    };
    let url = format!("http://localhost:{}", args.port);
    println!("MC Server Manager Web запущен: {}", url);
    println!("Данные панели: {}", args.data_dir.join("web.json").display());
    if app.data.lock().unwrap().users.is_empty() {
        println!("Пользователей ещё нет: первый зарегистрированный станет администратором.");
    }
    println!("Для выхода нажмите Ctrl+C (запущенные серверы будут корректно остановлены).");
    if args.open_browser {
        #[cfg(windows)]
        let _ = std::process::Command::new("cmd").args(["/C", "start", "", &url]).spawn();
    }

    let a = app.clone();
    let shutdown = async move {
        #[cfg(windows)]
        {
            // закрытие окна консоли крестиком тоже должно останавливать серверы
            let mut close = tokio::signal::windows::ctrl_close().expect("ctrl_close");
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = close.recv() => {}
            }
        }
        #[cfg(not(windows))]
        let _ = tokio::signal::ctrl_c().await;
        println!("Останавливаю серверы…");
        let _ = tokio::task::spawn_blocking(move || state::stop_all(&a)).await;
    };
    if let Err(e) = axum::serve(listener, router).with_graceful_shutdown(shutdown).await {
        eprintln!("Ошибка сервера: {}", e);
    }
}
