//! Админ-панель плагина MGP (мини-игры) в отдельном окне.
//!
//! Плагин поднимает HTTP API (plugins/MGP/config.yml → panel.host/port/token). Менеджер сам читает
//! этот файл из папки сервера, поэтому ничего настраивать не нужно. Данные обновляются раз в 2 секунды
//! в фоновом потоке, действия (старт игры, кик, отправка в хаб) идут через тот же поток.

use crate::ui::{self, AMBER, BLUE, GRAY, GREEN, RED};
use eframe::egui::{self, Color32, RichText, Ui};
use egui_extras::{Column, TableBuilder};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const REFRESH: Duration = Duration::from_secs(2);

// ------------------------------------------------------------------ данные API

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct Snapshot {
    pub plugin: String,
    pub server: String,
    pub tps: f64,
    #[serde(rename = "maxPlayers")]
    pub max_players: u32,
    pub players: Vec<PlayerInfo>,
    pub sessions: Vec<SessionInfo>,
    pub modes: Vec<ModeInfo>,
    pub maps: Vec<MapInfo>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct PlayerInfo {
    pub name: String,
    pub world: String,
    pub gamemode: String,
    pub health: f64,
    pub ping: i64,
    pub op: bool,
    pub session: Option<i64>,
    pub mode: Option<String>,
    #[serde(rename = "modeName")]
    pub mode_name: Option<String>,
    pub state: Option<String>,
    pub alive: bool,
    pub kills: i64,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct SessionInfo {
    pub id: i64,
    pub mode: String,
    #[serde(rename = "modeName")]
    pub mode_name: Option<String>,
    pub state: String,
    pub map: Option<String>,
    #[serde(rename = "mapName")]
    pub map_name: Option<String>,
    pub alive: i64,
    pub size: i64,
    pub min: i64,
    pub max: i64,
    pub elapsed: i64,
    pub players: Vec<String>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct ModeInfo {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub players: i64,
    pub maps: i64,
    #[serde(rename = "readyMaps")]
    pub ready_maps: i64,
    pub min: i64,
    pub max: i64,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct MapInfo {
    pub id: String,
    pub name: String,
    pub mode: String,
    pub template: bool,
    pub ready: bool,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct Named {
    pub id: String,
    pub name: String,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct Counts {
    pub played: i64,
    pub wins: i64,
    pub losses: i64,
    pub kills: i64,
    pub deaths: i64,
    pub kd: f64,
    #[serde(rename = "winRate")]
    pub win_rate: f64,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct StatPlayer {
    pub name: String,
    pub online: bool,
    pub total: Counts,
    pub modes: std::collections::HashMap<String, Counts>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct StatsResp {
    pub modes: Vec<Named>,
    pub total: i64,
    pub players: Vec<StatPlayer>,
}

// ------------------------------------------------------------------ подключение

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PanelAddr {
    pub base: String,
    pub token: String,
}

/// plugins/MGP/config.yml внутри папки сервера.
pub fn config_path(server_dir: &Path) -> PathBuf {
    server_dir.join("plugins").join("MGP").join("config.yml")
}

/// Есть ли на сервере плагин MGP (по его папке с конфигом).
pub fn installed(server_dir: &Path) -> bool {
    config_path(server_dir).is_file()
}

/// Читает секцию `panel:` из config.yml (простым разбором строк, без полного YAML).
pub fn read_addr(server_dir: &Path) -> Result<PanelAddr, String> {
    let text = std::fs::read_to_string(config_path(server_dir))
        .map_err(|e| format!("Не удалось прочитать plugins/MGP/config.yml: {}", e))?;
    parse_addr(&text)
}

fn parse_addr(text: &str) -> Result<PanelAddr, String> {
    let mut in_panel = false;
    let (mut host, mut port, mut token, mut enabled) = ("127.0.0.1".to_string(), 8765u16, String::new(), true);
    for line in text.lines() {
        let no_comment = strip_comment(line);
        if no_comment.trim().is_empty() {
            continue;
        }
        let indented = line.starts_with(' ') || line.starts_with('\t');
        if !indented {
            in_panel = no_comment.trim_end() == "panel:";
            continue;
        }
        if !in_panel {
            continue;
        }
        let Some((k, v)) = no_comment.trim().split_once(':') else { continue };
        let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
        match k.trim() {
            "host" => host = v,
            "port" => port = v.parse().unwrap_or(8765),
            "token" => token = v,
            "enabled" => enabled = v != "false",
            _ => {}
        }
    }
    if !enabled {
        return Err("Панель MGP выключена: panel.enabled = false в plugins/MGP/config.yml".into());
    }
    if token.is_empty() {
        return Err("В plugins/MGP/config.yml ещё нет ключа панели. Запустите сервер с MGP хотя бы раз.".into());
    }
    if host.is_empty() || host == "0.0.0.0" || host == "localhost" {
        host = "127.0.0.1".into();
    }
    Ok(PanelAddr { base: format!("http://{}:{}", host, port), token })
}

fn strip_comment(line: &str) -> &str {
    // комментарий начинается с « #»; внутри ключа (hex) решёток нет
    match line.find(" #") {
        Some(i) => &line[..i],
        None if line.trim_start().starts_with('#') => "",
        None => line,
    }
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(2))
        .timeout(Duration::from_secs(20))
        .build()
}

fn call(agent: &ureq::Agent, addr: &PanelAddr, method: &str, path: &str, body: Option<&Value>) -> Result<Value, String> {
    let url = format!("{}/api/{}", addr.base, path);
    let req = agent.request(method, &url).set("X-Token", &addr.token);
    let res = match body {
        Some(b) => req.send_json(b.clone()),
        None => req.call(),
    };
    match res {
        Ok(r) => r.into_json::<Value>().map_err(|e| format!("Неверный ответ: {}", e)),
        Err(ureq::Error::Status(code, r)) => {
            let msg = r
                .into_json::<Value>()
                .ok()
                .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(String::from))
                .unwrap_or_else(|| format!("HTTP {}", code));
            if code == 404 && msg.starts_with("Неизвестный запрос") {
                Err("Эта версия MGP не умеет работать с менеджером. Обновите плагин до 0.16.0 или новее.".into())
            } else {
                Err(msg)
            }
        }
        Err(e) => Err(format!("Нет связи с MGP ({}). Сервер запущен?", short_err(&e.to_string()))),
    }
}

fn short_err(s: &str) -> String {
    let s = s.rsplit(": ").next().unwrap_or(s);
    s.chars().take(80).collect()
}

// ------------------------------------------------------------------ фоновый поток

#[derive(Default)]
struct Shared {
    snap: Option<Snapshot>,
    error: Option<String>,
    last_ok: Option<Instant>,
    /// результаты действий: (успех, текст)
    notes: Vec<(bool, String)>,
    addr: Option<PanelAddr>,
    /// запрос статистики (строка после /api/stats), пока открыта вкладка «Статистика»
    stats_query: Option<String>,
    stats: Option<StatsResp>,
    stats_error: Option<String>,
}

struct Job {
    method: &'static str,
    path: String,
    body: Option<Value>,
    done: String,
}

fn worker(dir: PathBuf, shared: Arc<Mutex<Shared>>, rx: Receiver<Job>, ctx: egui::Context) {
    let agent = agent();
    let mut next_poll = Instant::now();
    loop {
        let wait = next_poll.saturating_duration_since(Instant::now());
        let job = match rx.recv_timeout(wait) {
            Ok(j) => Some(j),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        // ключ перечитываем каждый раз: его могли сменить, а порт — поменять в конфиге
        let addr = read_addr(&dir);
        if let Some(j) = job {
            let note = match &addr {
                Ok(a) => match call(&agent, a, j.method, &j.path, j.body.as_ref()) {
                    Ok(v) => {
                        let extra = v.get("message").and_then(|m| m.as_str()).map(|m| format!(": {}", m)).unwrap_or_default();
                        (true, format!("{}{}", j.done, extra))
                    }
                    Err(e) => (false, e),
                },
                Err(e) => (false, e.clone()),
            };
            if !(note.0 && j.done.is_empty()) {
                shared.lock().unwrap().notes.push(note);
            }
        }
        let result = addr.clone().and_then(|a| call(&agent, &a, "GET", "admin", None));
        let sq = shared.lock().unwrap().stats_query.clone();
        let stats = match (&addr, sq) {
            (Ok(a), Some(q)) => Some(call(&agent, a, "GET", &format!("stats{}", q), None)
                .and_then(|v| serde_json::from_value::<StatsResp>(v).map_err(|e| format!("Неверный ответ MGP: {}", e)))),
            _ => None,
        };
        {
            let mut s = shared.lock().unwrap();
            s.addr = addr.ok();
            match result.and_then(|v| serde_json::from_value::<Snapshot>(v).map_err(|e| format!("Неверный ответ MGP: {}", e))) {
                Ok(snap) => {
                    s.snap = Some(snap);
                    s.error = None;
                    s.last_ok = Some(Instant::now());
                }
                Err(e) => s.error = Some(e),
            }
            match stats {
                Some(Ok(st)) => {
                    s.stats = Some(st);
                    s.stats_error = None;
                }
                Some(Err(e)) => {
                    s.stats_error = Some(if e.contains("Неизвестный запрос") || e.contains("не умеет") {
                        "Статистика появилась в MGP 0.16.0: обновите плагин.".into()
                    } else {
                        e
                    })
                }
                None => {}
            }
        }
        ctx.request_repaint();
        next_poll = Instant::now() + REFRESH;
    }
}

// ------------------------------------------------------------------ окно

#[derive(Clone, Copy, PartialEq, Eq)]
enum PTab {
    Games,
    Players,
    Modes,
    Stats,
}

enum Ask {
    Kick(String, String),
    Message(String, String),
    CloseGame(i64, String),
    Reload,
}

pub struct MgpPanel {
    pub sid: String,
    title: String,
    shared: Arc<Mutex<Shared>>,
    tx: Sender<Job>,
    open: bool,
    tab: PTab,
    filter: String,
    broadcast: String,
    ask: Option<Ask>,
    notes: Vec<(bool, String, Instant)>,
    st_mode: String,
    st_sort: &'static str,
    st_search: String,
}

impl MgpPanel {
    pub fn new(ctx: &egui::Context, sid: &str, name: &str, dir: &Path) -> MgpPanel {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (tx, rx) = channel();
        let (sh, d, c) = (shared.clone(), dir.to_path_buf(), ctx.clone());
        thread::Builder::new()
            .name("mgp-panel".into())
            .spawn(move || worker(d, sh, rx, c))
            .expect("thread");
        MgpPanel {
            sid: sid.to_string(),
            title: format!("MGP — админ-панель · {}", name),
            shared,
            tx,
            open: true,
            tab: PTab::Games,
            filter: String::new(),
            broadcast: String::new(),
            ask: None,
            notes: vec![],
            st_mode: "all".into(),
            st_sort: "wins",
            st_search: String::new(),
        }
    }

    pub fn viewport_id(&self) -> egui::ViewportId {
        egui::ViewportId::from_hash_of(("mgp-panel", &self.sid))
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    fn send(&self, method: &'static str, path: String, body: Option<Value>, done: impl Into<String>) {
        let _ = self.tx.send(Job { method, path, body, done: done.into() });
    }

    fn player_action(&self, name: &str, action: &str, body: Value, done: String) {
        self.send("POST", format!("players/{}/{}", name, action), Some(body), done);
    }

    /// Рисует окно; вызывается каждый кадр главного окна.
    pub fn show(&mut self, ctx: &egui::Context) {
        let vid = self.viewport_id();
        let builder = egui::ViewportBuilder::default()
            .with_title(&self.title)
            .with_inner_size([1150.0, 720.0])
            .with_min_inner_size([760.0, 460.0])
            .with_icon(ui::app_icon());
        ctx.show_viewport_immediate(vid, builder, |ctx, class| {
            if class == egui::ViewportClass::Embedded {
                // платформа без отдельных окон: показываем внутри главного
                let mut open = true;
                egui::Window::new(&self.title).open(&mut open).default_size([1000.0, 640.0]).show(ctx, |ui| self.body(ui));
                if !open {
                    self.open = false;
                }
            } else {
                egui::CentralPanel::default().show(ctx, |ui| self.body(ui));
                if ctx.input(|i| i.viewport().close_requested()) {
                    self.open = false;
                }
            }
            self.dialog(ctx);
        });
    }

    fn body(&mut self, ui: &mut Ui) {
        let (snap, error, last_ok, addr) = {
            let mut s = self.shared.lock().unwrap();
            for (ok, t) in s.notes.drain(..) {
                self.notes.push((ok, t, Instant::now()));
            }
            (s.snap.clone(), s.error.clone(), s.last_ok, s.addr.clone())
        };
        self.notes.retain(|n| n.2.elapsed() < Duration::from_secs(8));
        let fresh = last_ok.map(|t| t.elapsed() < Duration::from_secs(6)).unwrap_or(false);

        // ---- шапка
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.heading(RichText::new("🎮 Мини-игры").strong());
            if fresh && error.is_none() {
                ui::status_dot(ui, GREEN, 12.0);
                ui.label(RichText::new("Подключено").color(GREEN));
            } else {
                ui::status_dot(ui, RED, 12.0);
                ui.label(RichText::new("Нет связи").color(RED));
            }
            if let Some(s) = &snap {
                ui.weak(format!("MGP {}  ·  {}", s.plugin, s.server));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add_enabled(addr.is_some(), egui::Button::new("🌐 Редактор режимов и карт"))
                    .on_hover_text("Открыть веб-панель MGP в браузере: настройка режимов, карт, импорт карт")
                    .clicked()
                {
                    if let Some(a) = &addr {
                        let _ = open::that_detached(format!("{}/#token={}", a.base, a.token));
                    }
                }
                if ui.add_enabled(fresh, egui::Button::new("🔄 Перечитать конфиги")).clicked() {
                    self.ask = Some(Ask::Reload);
                }
            });
        });
        if let Some(e) = &error {
            ui.label(RichText::new(format!("⚠ {}", e)).color(if snap.is_some() { AMBER } else { RED }));
        }
        let Some(snap) = snap else {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                if error.is_none() {
                    ui.spinner();
                    ui.weak("Подключаюсь к MGP…");
                } else {
                    ui.label(RichText::new("Запустите сервер — окно подключится само.").size(16.0).weak());
                }
            });
            return;
        };

        // ---- карточки
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            let in_games = snap.players.iter().filter(|p| p.session.is_some()).count();
            let playing = snap.sessions.iter().filter(|s| s.state == "PLAYING").count();
            ui::stat_card(ui, "Онлайн", &format!("{} / {}", snap.players.len(), snap.max_players), None);
            ui::stat_card(ui, "В играх / в хабе", &format!("{} / {}", in_games, snap.players.len() - in_games), None);
            ui::stat_card(ui, "Игры (идут)", &format!("{} ({})", snap.sessions.len(), playing), None);
            let tps_col = if snap.tps >= 18.0 { GREEN } else if snap.tps >= 15.0 { AMBER } else { RED };
            ui::stat_card(ui, "TPS", &format!("{:.1}", snap.tps), Some(tps_col));
            let ready = snap.maps.iter().filter(|m| m.ready).count();
            ui::stat_card(ui, "Карты готовы", &format!("{} / {}", ready, snap.maps.len()), None);
        });

        // ---- объявление
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("📢");
            let r = ui.add(egui::TextEdit::singleline(&mut self.broadcast).hint_text("Объявление всем игрокам…").desired_width(ui.available_width() - 110.0));
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (ui.add_enabled(!self.broadcast.trim().is_empty(), egui::Button::new("Отправить")).clicked() || enter)
                && !self.broadcast.trim().is_empty()
            {
                self.send("POST", "broadcast".into(), Some(json!({ "text": self.broadcast.trim() })), "Объявление отправлено");
                self.broadcast.clear();
            }
        });

        // одна строка под результат последнего действия: её высота постоянна, чтобы кнопки не прыгали
        match self.notes.last() {
            Some((ok, t, _)) => {
                let txt = if *ok { format!("✔ {}", t) } else { format!("✖ {}", t) };
                ui.add(egui::Label::new(RichText::new(txt).color(if *ok { GREEN } else { RED })).truncate());
            }
            None => {
                ui.label(" ");
            }
        }

        // ---- вкладки
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            for (t, l) in [
                (PTab::Games, format!("🎮  Игры ({})", snap.sessions.len())),
                (PTab::Players, format!("👥  Игроки ({})", snap.players.len())),
                (PTab::Modes, format!("🗺  Режимы и карты ({} / {})", snap.modes.len(), snap.maps.len())),
                (PTab::Stats, "🏆  Статистика".to_string()),
            ] {
                if ui.selectable_label(self.tab == t, RichText::new(l).size(15.0)).clicked() {
                    self.tab = t;
                }
            }
        });
        ui.separator();
        match self.tab {
            PTab::Games => self.tab_games(ui, &snap),
            PTab::Players => self.tab_players(ui, &snap),
            PTab::Modes => self.tab_modes(ui, &snap),
            PTab::Stats => self.tab_stats(ui),
        }
        // статистику запрашиваем только пока открыта её вкладка
        let q = (self.tab == PTab::Stats).then(|| {
            format!("?mode={}&sort={}&limit=500&search={}", enc(&self.st_mode), self.st_sort, enc(self.st_search.trim()))
        });
        let mut sh = self.shared.lock().unwrap();
        if sh.stats_query != q {
            let first = sh.stats_query.is_none() && q.is_some();
            sh.stats_query = q;
            drop(sh);
            if first {
                // сразу подтягиваем, не дожидаясь следующего опроса
                self.send("GET", "ping".into(), None, "");
            }
        }
    }

    fn tab_games(&mut self, ui: &mut Ui, snap: &Snapshot) {
        if snap.sessions.is_empty() {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| ui.label(RichText::new("Сейчас нет ни одной игры").size(16.0).weak()));
            return;
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for s in &snap.sessions {
                ui::card_frame(ui).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(format!("#{}  {}", s.id, s.mode_name.as_deref().unwrap_or(&s.mode))).size(16.0).strong());
                        state_pill(ui, &s.state);
                        let map = s.map_name.as_deref().or(s.map.as_deref()).unwrap_or("карта ещё не выбрана");
                        ui.weak(format!("🗺 {}", map));
                        ui.weak(format!("👥 {} / {} (мин. {})", s.size, s.max, s.min));
                        if s.state == "PLAYING" || s.state == "ENDING" {
                            ui.weak(format!("❤ живы: {}", s.alive));
                        }
                        ui.weak(format!("⏱ {}", mmss(s.elapsed)));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let close = egui::Button::new(RichText::new("✖ Закрыть").color(Color32::WHITE)).fill(RED.gamma_multiply(0.8));
                            if ui.add(close).on_hover_text("Завершить игру, всех игроков — в хаб").clicked() {
                                self.ask = Some(Ask::CloseGame(s.id, s.mode_name.clone().unwrap_or(s.mode.clone())));
                            }
                            let can_start = s.state == "WAITING" || s.state == "STARTING";
                            let start = egui::Button::new(RichText::new("▶ Начать сейчас").color(Color32::WHITE)).fill(GREEN.gamma_multiply(0.85));
                            if ui.add_enabled(can_start, start).on_hover_text("Запустить, не дожидаясь набора игроков").clicked() {
                                self.send("POST", format!("sessions/{}/start", s.id), None, format!("Игра #{} запускается", s.id));
                            }
                        });
                    });
                    ui.horizontal_wrapped(|ui| {
                        if s.players.is_empty() {
                            ui.weak("Игроков нет");
                        }
                        for n in &s.players {
                            let alive = snap.players.iter().find(|p| &p.name == n).map(|p| p.alive).unwrap_or(false);
                            let txt = if alive || !(s.state == "PLAYING" || s.state == "ENDING") { format!("👤 {}", n) } else { format!("💀 {}", n) };
                            let r = ui.add(egui::Button::new(txt).small()).on_hover_text("Клик — действия с игроком");
                            r.context_menu(|ui| self.player_menu(ui, n, snap));
                            if r.clicked() {
                                // открываем то же меню по левому клику
                                r.ctx.memory_mut(|m| m.open_popup(r.id.with("ctx")));
                            }
                            egui::popup::popup_below_widget(ui, r.id.with("ctx"), &r, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
                                ui.set_min_width(200.0);
                                self.player_menu(ui, n, snap)
                            });
                        }
                    });
                });
                ui.add_space(6.0);
            }
        });
    }

    fn tab_players(&mut self, ui: &mut Ui, snap: &Snapshot) {
        ui.horizontal(|ui| {
            ui.label("🔍");
            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Поиск по нику").desired_width(220.0));
        });
        let f = self.filter.to_lowercase();
        let list: Vec<&PlayerInfo> = snap.players.iter().filter(|p| f.is_empty() || p.name.to_lowercase().contains(&f)).collect();
        if list.is_empty() {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| ui.label(RichText::new(if snap.players.is_empty() { "На сервере никого нет" } else { "Никто не найден" }).size(16.0).weak()));
            return;
        }
        let modes: Vec<(String, String)> = snap.modes.iter().filter(|m| m.ready_maps > 0).map(|m| (m.id.clone(), m.name.clone())).collect();
        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::initial(140.0).at_least(100.0))
            .column(Column::initial(220.0).at_least(120.0))
            .column(Column::initial(105.0))
            .column(Column::initial(40.0))
            .column(Column::initial(65.0))
            .column(Column::initial(75.0))
            .column(Column::remainder().at_least(340.0))
            .header(24.0, |mut h| {
                for t in ["Ник", "Где", "Режим игры", "❤", "Пинг", "Убийства", "Действия"] {
                    h.col(|ui| {
                        ui.strong(t);
                    });
                }
            })
            .body(|mut body| {
                for p in list {
                    body.row(28.0, |mut row| {
                        row.col(|ui| {
                            let t = if p.op { format!("⭐ {}", p.name) } else { p.name.clone() };
                            ui.label(RichText::new(t).strong());
                        });
                        row.col(|ui| {
                            match p.session {
                                Some(id) => {
                                    let st = p.state.as_deref().unwrap_or("");
                                    let dead = (st == "PLAYING" || st == "ENDING") && !p.alive;
                                    ui.label(format!("#{} {}{}", id, p.mode_name.as_deref().or(p.mode.as_deref()).unwrap_or("?"), if dead { " (наблюдает)" } else { "" }));
                                    ui.weak(state_ru(st));
                                }
                                None => {
                                    ui.label(RichText::new("Хаб").color(BLUE));
                                    ui.weak(&p.world);
                                }
                            }
                        });
                        row.col(|ui| {
                            ui.label(gamemode_ru(&p.gamemode));
                        });
                        row.col(|ui| {
                            ui.label(format!("{:.0}", p.health));
                        });
                        row.col(|ui| {
                            let c = if p.ping < 100 { GREEN } else if p.ping < 250 { AMBER } else { RED };
                            ui.label(RichText::new(format!("{} мс", p.ping)).color(c));
                        });
                        row.col(|ui| {
                            ui.label(if p.session.is_some() { p.kills.to_string() } else { "—".into() });
                        });
                        row.col(|ui| {
                            if ui.add_enabled(p.session.is_some(), egui::Button::new("🏠 В хаб")).clicked() {
                                self.player_action(&p.name, "hub", json!({}), format!("{} отправлен в хаб", p.name));
                            }
                            ui.menu_button("🎮 В режим ▾", |ui| {
                                if modes.is_empty() {
                                    ui.weak("Нет режимов с готовыми картами");
                                }
                                for (id, name) in &modes {
                                    if ui.button(name).clicked() {
                                        self.player_action(&p.name, "join", json!({ "mode": id }), format!("{} → {}", p.name, name));
                                        ui.close_menu();
                                    }
                                }
                            });
                            if ui.button("✉").on_hover_text("Сообщение").clicked() {
                                self.ask = Some(Ask::Message(p.name.clone(), String::new()));
                            }
                            if ui.button(RichText::new("👢 Кик").color(RED)).clicked() {
                                self.ask = Some(Ask::Kick(p.name.clone(), String::new()));
                            }
                        });
                    });
                }
            });
    }

    fn tab_modes(&mut self, ui: &mut Ui, snap: &Snapshot) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.strong("Режимы");
            egui::Grid::new("mgp_modes").striped(true).num_columns(6).spacing([18.0, 6.0]).show(ui, |ui| {
                for t in ["Режим", "id", "Тип", "Игроков сейчас", "Карты (готовы / всего)", "Игроков в арене"] {
                    ui.weak(t);
                }
                ui.end_row();
                for m in &snap.modes {
                    ui.label(RichText::new(&m.name).strong());
                    ui.monospace(&m.id);
                    ui.label(&m.kind);
                    ui.label(m.players.to_string());
                    let c = if m.ready_maps == 0 { RED } else if m.ready_maps < m.maps { AMBER } else { GREEN };
                    ui.label(RichText::new(format!("{} / {}", m.ready_maps, m.maps)).color(c));
                    ui.label(format!("{}–{}", m.min, m.max));
                    ui.end_row();
                }
            });
            ui.add_space(12.0);
            ui.strong("Карты");
            let mut maps = snap.maps.clone();
            maps.sort_by(|a, b| a.mode.cmp(&b.mode).then(a.id.cmp(&b.id)));
            egui::Grid::new("mgp_maps").striped(true).num_columns(4).spacing([18.0, 6.0]).show(ui, |ui| {
                for t in ["Карта", "id", "Режим", "Состояние"] {
                    ui.weak(t);
                }
                ui.end_row();
                for m in &maps {
                    ui.label(RichText::new(&m.name).strong());
                    ui.monospace(&m.id);
                    let mode = snap.modes.iter().find(|x| x.id == m.mode).map(|x| x.name.as_str()).unwrap_or(&m.mode);
                    ui.label(mode);
                    if m.ready {
                        ui.label(RichText::new("✔ готова").color(GREEN));
                    } else if !m.template {
                        ui.label(RichText::new("✖ нет шаблона мира").color(RED));
                    } else {
                        ui.label(RichText::new("⚠ нет точек спавна").color(AMBER));
                    }
                    ui.end_row();
                }
            });
            ui.add_space(8.0);
            ui.weak("Настроить режимы и карты можно в редакторе (кнопка «🌐 Редактор режимов и карт» вверху).");
        });
    }

    fn tab_stats(&mut self, ui: &mut Ui) {
        let (stats, err) = {
            let s = self.shared.lock().unwrap();
            (s.stats.clone(), s.stats_error.clone())
        };
        ui.horizontal(|ui| {
            ui.label("Режим:");
            let modes = stats.as_ref().map(|s| s.modes.clone()).unwrap_or_default();
            let cur = if self.st_mode == "all" {
                "Все режимы".to_string()
            } else {
                modes.iter().find(|m| m.id == self.st_mode).map(|m| m.name.clone()).unwrap_or(self.st_mode.clone())
            };
            egui::ComboBox::from_id_salt("st_mode").selected_text(cur).width(200.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.st_mode, "all".to_string(), "Все режимы");
                for m in &modes {
                    ui.selectable_value(&mut self.st_mode, m.id.clone(), &m.name);
                }
            });
            ui.label("🔍");
            ui.add(egui::TextEdit::singleline(&mut self.st_search).hint_text("Поиск по нику").desired_width(200.0));
            if let Some(s) = &stats {
                ui.weak(format!("Всего игроков в статистике: {}", s.total));
            }
        });
        if let Some(e) = &err {
            ui.label(RichText::new(format!("⚠ {}", e)).color(AMBER));
        }
        let Some(stats) = stats else {
            if err.is_none() {
                ui.spinner();
            }
            return;
        };
        if stats.players.is_empty() {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| ui.label(RichText::new("Пока никто не сыграл ни одной игры").size(16.0).weak()));
            return;
        }
        let cols: [(&str, Option<&'static str>); 9] = [
            ("#", None), ("Ник", None), ("Игр", Some("played")), ("Побед", Some("wins")), ("Поражений", None),
            ("Убийств", Some("kills")), ("Смертей", Some("deaths")), ("K/D", Some("kd")), ("% побед", None),
        ];
        let mode = self.st_mode.clone();
        let mut new_sort = None;
        TableBuilder::new(ui)
            .striped(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(40.0))
            .column(Column::initial(180.0).at_least(110.0))
            .columns(Column::initial(85.0), 7)
            .header(26.0, |mut h| {
                for (t, key) in cols {
                    h.col(|ui| match key {
                        Some(k) => {
                            let on = self.st_sort == k;
                            let label = if on { format!("{} ⏷", t) } else { t.to_string() };
                            if ui.selectable_label(on, RichText::new(label).strong()).on_hover_text("Сортировать").clicked() {
                                new_sort = Some(k);
                            }
                        }
                        None => {
                            ui.strong(t);
                        }
                    });
                }
            })
            .body(|mut body| {
                for (i, p) in stats.players.iter().enumerate() {
                    let c = if mode == "all" { p.total.clone() } else { p.modes.get(&mode).cloned().unwrap_or_default() };
                    body.row(26.0, |mut row| {
                        row.col(|ui| {
                            ui.weak((i + 1).to_string());
                        });
                        row.col(|ui| {
                            ui::status_dot(ui, if p.online { GREEN } else { GRAY }, 10.0);
                            ui.label(RichText::new(&p.name).strong());
                        });
                        for v in [c.played.to_string(), c.wins.to_string(), c.losses.to_string(), c.kills.to_string(),
                                  c.deaths.to_string(), format!("{:.2}", c.kd), format!("{:.0} %", c.win_rate)] {
                            row.col(|ui| {
                                ui.label(v);
                            });
                        }
                    });
                }
            });
        if let Some(k) = new_sort {
            self.st_sort = k;
        }
    }

    fn player_menu(&mut self, ui: &mut Ui, n: &str, snap: &Snapshot) {
        if ui.button("🏠 В хаб").clicked() {
            self.player_action(n, "hub", json!({}), format!("{} отправлен в хаб", n));
            ui.close_menu();
        }
        ui.menu_button("🎮 В режим", |ui| {
            for m in snap.modes.iter().filter(|m| m.ready_maps > 0) {
                if ui.button(&m.name).clicked() {
                    self.player_action(n, "join", json!({ "mode": m.id }), format!("{} → {}", n, m.name));
                    ui.close_menu();
                }
            }
        });
        if ui.button("✉ Сообщение…").clicked() {
            self.ask = Some(Ask::Message(n.to_string(), String::new()));
            ui.close_menu();
        }
        if ui.button("👢 Кикнуть…").clicked() {
            self.ask = Some(Ask::Kick(n.to_string(), String::new()));
            ui.close_menu();
        }
    }

    fn dialog(&mut self, ctx: &egui::Context) {
        let Some(mut ask) = self.ask.take() else { return };
        let mut keep = true;
        let mut ok = false;
        egui::Modal::new(egui::Id::new("mgp_ask")).show(ctx, |ui| {
            ui.set_max_width(460.0);
            let (title, label, text): (String, String, Option<&mut String>) = match &mut ask {
                Ask::Kick(n, r) => ("Кик".into(), format!("Причина кика {} (можно пусто):", n), Some(r)),
                Ask::Message(n, t) => ("Сообщение".into(), format!("Сообщение для {}:", n), Some(t)),
                Ask::CloseGame(id, m) => ("Закрыть игру".into(), format!("Завершить игру #{} ({})?\nВсе игроки вернутся в хаб.", id, m), None),
                Ask::Reload => ("Перечитать конфиги".into(), "Перечитать config.yml, режимы и карты MGP?\nИдущие игры продолжатся со старыми настройками.".into(), None),
            };
            ui.heading(title);
            ui.add_space(6.0);
            ui.label(label);
            let mut enter = false;
            if let Some(t) = text {
                let r = ui.add(egui::TextEdit::singleline(t).desired_width(440.0));
                if !r.has_focus() && t.is_empty() {
                    r.request_focus();
                }
                enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button(RichText::new("  OK  ").strong()).clicked() || enter {
                    ok = true;
                    keep = false;
                }
                if ui.button("Отмена").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    keep = false;
                }
            });
        });
        if ok {
            match &ask {
                Ask::Kick(n, r) => self.player_action(n, "kick", json!({ "reason": r }), format!("{} кикнут", n)),
                Ask::Message(n, t) if !t.trim().is_empty() => self.player_action(n, "message", json!({ "text": t }), format!("Сообщение для {} отправлено", n)),
                Ask::Message(..) => {}
                Ask::CloseGame(id, _) => self.send("POST", format!("sessions/{}/close", id), None, format!("Игра #{} закрыта", id)),
                Ask::Reload => self.send("POST", "reload".into(), None, "Конфиги MGP перечитаны"),
            }
        }
        if keep {
            self.ask = Some(ask);
        }
    }
}

/// Кодирование значения для строки запроса.
fn enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

fn mmss(s: i64) -> String {
    format!("{}:{:02}", s / 60, s % 60)
}

fn state_ru(s: &str) -> &'static str {
    match s {
        "CREATING" => "готовится",
        "WAITING" => "ожидание игроков",
        "STARTING" => "отсчёт",
        "PLAYING" => "идёт игра",
        "ENDING" => "завершается",
        "CLOSED" => "закрыта",
        _ => "",
    }
}

fn state_pill(ui: &mut Ui, s: &str) {
    let c = match s {
        "PLAYING" => GREEN,
        "STARTING" => AMBER,
        "WAITING" | "CREATING" => BLUE,
        _ => GRAY,
    };
    let t = RichText::new(format!(" {} ", state_ru(s))).color(Color32::WHITE).background_color(c.gamma_multiply(0.85));
    ui.label(t);
}

fn gamemode_ru(g: &str) -> &'static str {
    match g {
        "SURVIVAL" => "Выживание",
        "CREATIVE" => "Творческий",
        "ADVENTURE" => "Приключение",
        "SPECTATOR" => "Наблюдатель",
        _ => "?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_panel_section() {
        let y = "hub:\n  world: world\n# Админ-панель\npanel:\n  enabled: true\n  host: 0.0.0.0          # comment\n  port: 9000\n  public-url: \"\"\n  token: abcdef0123456789abcdef\n\ndebug:\n  token: nope\n";
        let a = parse_addr(y).unwrap();
        assert_eq!(a.base, "http://127.0.0.1:9000");
        assert_eq!(a.token, "abcdef0123456789abcdef");
    }

    #[test]
    fn quoted_token_and_missing() {
        let a = parse_addr("panel:\n  token: 'abc123abc123abc123'\n").unwrap();
        assert_eq!(a.token, "abc123abc123abc123");
        assert_eq!(a.base, "http://127.0.0.1:8765");
        assert!(parse_addr("panel:\n  token: \"\"\n").is_err());
        assert!(parse_addr("panel:\n  enabled: false\n  token: x\n").is_err());
    }
}
