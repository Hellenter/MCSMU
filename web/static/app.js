// MC Server Manager Web — интерфейс в браузере (без сборки и зависимостей).
"use strict";

// ------------------------------------------------------------------ утилиты
function h(tag, attrs, ...kids) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v === null || v === undefined || v === false) continue;
    if (k === "class") el.className = v;
    else if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
    else if (k === "value") el.value = v;
    else if (k === "checked") el.checked = !!v;
    else if (k === "style") el.style.cssText = v;
    else el.setAttribute(k, v === true ? "" : v);
  }
  for (const c of kids.flat(Infinity)) {
    if (c === null || c === undefined || c === false) continue;
    el.append(c instanceof Node ? c : document.createTextNode(String(c)));
  }
  return el;
}
const $ = (sel, root = document) => root.querySelector(sel);
// append/replaceChildren тоже принимают массивы и пропускают пустые значения, как h()
for (const name of ["append", "replaceChildren"]) {
  const orig = Element.prototype[name];
  Element.prototype[name] = function (...kids) {
    return orig.apply(this, kids.flat(Infinity).filter((c) => c !== null && c !== undefined && c !== false));
  };
}

async function api(method, url, body) {
  const opt = { method, headers: { "X-MCSM": "1" }, credentials: "same-origin" };
  if (body instanceof FormData) opt.body = body;
  else if (body !== undefined) {
    opt.headers["Content-Type"] = "application/json";
    opt.body = JSON.stringify(body);
  }
  let r;
  try {
    r = await fetch(url, opt);
  } catch (e) {
    throw new Error("Нет связи с панелью");
  }
  let data = null;
  try { data = await r.json(); } catch (e) { /* пустой ответ */ }
  if (r.status === 401) {
    if (S.user) { S.user = null; render(); }
    throw new Error((data && data.error) || "Нужно войти");
  }
  if (!r.ok) throw new Error((data && data.error) || "Ошибка " + r.status);
  return data;
}
const GET = (u) => api("GET", u);
const POST = (u, b) => api("POST", u, b === undefined ? {} : b);
const PUT = (u, b) => api("PUT", u, b);
const DEL = (u) => api("DELETE", u);

function toast(text, error) {
  const t = h("div", { class: "toast" + (error ? " error" : "") }, text);
  $("#toasts").append(t);
  setTimeout(() => t.remove(), error ? 9000 : 4500);
}
async function act(fn, okText) {
  try {
    const r = await fn();
    if (okText) toast(okText);
    return r;
  } catch (e) {
    toast(e.message, true);
    return undefined;
  }
}

function size(n) {
  if (n === null || n === undefined) return "—";
  const u = ["Б", "КБ", "МБ", "ГБ", "ТБ"];
  let i = 0;
  while (n >= 1024 && i < u.length - 1) { n /= 1024; i++; }
  return (i === 0 ? n.toFixed(0) : n.toFixed(1)) + " " + u[i];
}
function dur(s) {
  if (s === null || s === undefined) return "—";
  const d = Math.floor(s / 86400), hh = Math.floor((s % 86400) / 3600), m = Math.floor((s % 3600) / 60);
  if (d) return `${d} д ${hh} ч`;
  if (hh) return `${hh} ч ${m} мин`;
  return `${m} мин ${s % 60} с`;
}
function parseMem(s) {
  const m = /^\s*(\d+(?:[.,]\d+)?)\s*([kmgt]?)/i.exec(s || "");
  if (!m) return null;
  const v = parseFloat(m[1].replace(",", "."));
  return { k: v / 1024, m: v, g: v * 1024, t: v * 1048576, "": v / 1048576 }[m[2].toLowerCase()];
}

// ------------------------------------------------------------------ окна
function modal(title, body, buttons, wide) {
  return new Promise((resolve) => {
    const close = (v) => { ov.remove(); document.removeEventListener("keydown", onKey); resolve(v); };
    const onKey = (e) => { if (e.key === "Escape") close(undefined); };
    const foot = h("footer", {}, buttons.map((b) =>
      h("button", { class: "btn " + (b.cls || ""), onclick: async () => {
        if (b.action) {
          const v = await b.action();
          if (v === false) return;
          close(v === undefined ? b.value : v);
        } else close(b.value);
      } }, b.label)));
    const ov = h("div", { class: "overlay", onmousedown: (e) => { if (e.target === ov) close(undefined); } },
      h("div", { class: "modal" + (wide ? " wide" : "") }, h("header", {}, title), h("div", { class: "body" }, body), foot));
    document.body.append(ov);
    document.addEventListener("keydown", onKey);
    const f = ov.querySelector("input, select, textarea");
    if (f) f.focus();
  });
}
const confirmBox = (title, text, okLabel, danger) =>
  modal(title, h("div", { style: "white-space:pre-wrap" }, text),
    [{ label: "Отмена", value: false }, { label: okLabel || "OK", value: true, cls: danger ? "danger solid" : "primary" }]);

async function promptBox(title, fields, okLabel) {
  const inputs = fields.map((f) => h("input", { value: f.value || "", placeholder: f.placeholder || "", type: f.type || "text" }));
  const body = h("div", { style: "display:grid;gap:10px" },
    fields.map((f, i) => h("label", { style: "display:grid;gap:4px" }, h("span", { class: "muted" }, f.label), inputs[i])));
  inputs.forEach((inp) => inp.addEventListener("keydown", (e) => { if (e.key === "Enter") body.closest(".modal").querySelector("footer .primary").click(); }));
  return modal(title, body, [
    { label: "Отмена", value: null },
    { label: okLabel || "OK", cls: "primary", action: () => inputs.map((i) => i.value) },
  ]);
}

/** Выбор папки на компьютере, где запущена панель. */
async function pickFolder(title, start) {
  let cur = start || "";
  const pathIn = h("input", { class: "grow mono", value: cur, placeholder: "Путь к папке" });
  const list = h("div", { class: "picker-list" });
  const roots = h("div", { class: "row", style: "margin-top:8px" });
  const info = h("div", { class: "muted small", style: "margin-top:6px" });
  async function load(p) {
    try {
      const r = await GET("/api/fs?path=" + encodeURIComponent(p || ""));
      cur = r.path;
      pathIn.value = r.path;
      roots.replaceChildren(...r.roots.map((x) => h("button", { class: "btn small", onclick: () => load(x) }, x)));
      list.replaceChildren(
        ...(r.parent ? [h("div", { onclick: () => load(r.parent) }, "⬑ ..")] : []),
        ...r.dirs.map((d) => h("div", { onclick: () => load(joinPath(r.path, d)) }, "📁 " + d)),
      );
      if (!r.path) list.replaceChildren(h("div", { class: "muted" }, "Выберите диск или папку выше"));
      info.textContent = r.is_world ? "Это папка мира (есть level.dat)" : r.is_server ? "Похоже на папку сервера" : "";
    } catch (e) {
      toast(e.message, true);
    }
  }
  pathIn.addEventListener("keydown", (e) => { if (e.key === "Enter") load(pathIn.value); });
  const body = h("div", {}, h("div", { class: "row" }, pathIn, h("button", { class: "btn", onclick: () => load(pathIn.value) }, "Перейти")), roots, list, info);
  load(cur);
  return modal(title, body, [
    { label: "Отмена", value: null },
    { label: "Выбрать эту папку", cls: "primary", action: () => (pathIn.value.trim() ? pathIn.value.trim() : false) },
  ], true);
}
function joinPath(base, name) {
  const sep = base.includes("\\") ? "\\" : "/";
  return base.endsWith(sep) ? base + name : base + sep + name;
}

// ------------------------------------------------------------------ состояние
const S = {
  user: null,
  pendingUsers: 0,
  servers: [],
  tunnel: null,
  route: { page: "home" },
  noticeSeen: {},
  timers: [],
  menuHook: false,
};

function parseRoute() {
  const p = location.hash.replace(/^#\/?/, "").split("/");
  if (p[0] === "s" && p[1]) return { page: "server", id: p[1], tab: p[2] || "console" };
  if (p[0] === "new") return { page: "new" };
  if (p[0] === "tunnel") return { page: "tunnel" };
  if (p[0] === "admin") return { page: "admin" };
  return { page: "home" };
}
const go = (hash) => { location.hash = hash; };
window.addEventListener("hashchange", () => { S.route = parseRoute(); renderMain(); renderSide(); });

function clearTimers() { S.timers.forEach(clearInterval); S.timers = []; }
function every(ms, fn) { fn(); S.timers.push(setInterval(fn, ms)); }

// ------------------------------------------------------------------ вход
function renderAuth(mode) {
  clearTimers();
  mode = mode || "login";
  const user = h("input", { autocomplete: "username", required: true });
  const pass = h("input", { type: "password", autocomplete: mode === "login" ? "current-password" : "new-password", required: true });
  const pass2 = h("input", { type: "password", autocomplete: "new-password" });
  const msg = h("div", { class: "small" });
  const submit = h("button", { class: "btn primary", type: "submit" }, mode === "login" ? "Войти" : "Зарегистрироваться");
  const form = h("form", { onsubmit: async (e) => {
    e.preventDefault();
    msg.className = "small err";
    msg.textContent = "";
    if (mode === "register" && pass.value !== pass2.value) { msg.textContent = "Пароли не совпадают"; return; }
    submit.disabled = true;
    try {
      const r = await POST("/api/" + mode, { username: user.value, password: pass.value });
      if (r.pending) {
        msg.className = "small";
        msg.textContent = "Аккаунт создан и ждёт одобрения администратора. Войдите после одобрения.";
        return;
      }
      await boot();
    } catch (err) {
      msg.textContent = err.message;
    } finally {
      submit.disabled = false;
    }
  } },
    h("label", {}, "Логин", user),
    h("label", {}, "Пароль", pass),
    mode === "register" ? h("label", {}, "Пароль ещё раз", pass2) : null,
    msg, submit);
  const tab = (m, label) => h("button", { class: m === mode ? "on" : "", onclick: () => renderAuth(m) }, label);
  $("#root").replaceChildren(h("div", { class: "auth" }, h("div", { class: "auth-card" },
    h("div", { class: "brand" }, h("i"), "MC Server Manager"),
    h("div", { class: "muted small" }, "Управление серверами Minecraft"),
    h("div", { class: "auth-tabs" }, tab("login", "Вход"), tab("register", "Регистрация")),
    form)));
  user.focus();
}

// ------------------------------------------------------------------ каркас
async function boot() {
  try {
    const r = await GET("/api/me");
    S.user = r.user;
    S.pendingUsers = r.pending_users;
  } catch (e) {
    S.user = null;
  }
  render();
}

function render() {
  if (!S.user) return renderAuth();
  S.route = parseRoute();
  $("#root").replaceChildren(h("div", { class: "app" },
    h("div", { class: "top" },
      h("button", { class: "btn ghost menu-btn", onclick: () => $(".side").classList.toggle("open") }, "☰"),
      h("div", { class: "brand" }, h("i"), "MC Server Manager"),
      h("span", { id: "tunnel-pill" }),
      h("div", { class: "user" },
        h("button", { class: "btn ghost", onclick: (e) => { e.stopPropagation(); toggleUserMenu(); } }, "👤 " + S.user.username + " ▾"),
        h("div", { class: "dropdown hidden", id: "user-menu" },
          h("button", { onclick: changePassword }, "Сменить пароль"),
          S.user.admin ? h("button", { onclick: () => go("#/admin") }, "Администрирование") : null,
          h("button", { onclick: logout }, "Выйти")))),
    h("nav", { class: "side" }),
    h("main", { class: "main" })));
  if (!S.menuHook) {
    S.menuHook = true;
    document.addEventListener("click", () => { const m = $("#user-menu"); if (m) m.classList.add("hidden"); });
  }
  refreshServers();
  if (!S.pollList) S.pollList = setInterval(refreshServers, 2000);
  renderSide();
  renderMain();
}
function toggleUserMenu() { $("#user-menu").classList.toggle("hidden"); }

async function logout() {
  await act(() => POST("/api/logout"));
  S.user = null;
  S.servers = [];
  clearTimers();
  render();
}

async function changePassword() {
  const v = await promptBox("Смена пароля", [
    { label: "Текущий пароль", type: "password" },
    { label: "Новый пароль", type: "password" },
    { label: "Новый пароль ещё раз", type: "password" },
  ], "Сменить");
  if (!v) return;
  if (v[1] !== v[2]) return toast("Пароли не совпадают", true);
  await act(() => POST("/api/me/password", { old: v[0], new: v[1] }), "Пароль изменён");
}

async function refreshServers() {
  if (!S.user) return;
  let r;
  try { r = await GET("/api/servers"); } catch (e) { return; }
  S.servers = r.servers;
  S.tunnel = r.tunnel;
  for (const s of S.servers) {
    if (s.notice) {
      const seen = S.noticeSeen[s.id];
      if (seen !== undefined && seen < s.notice.n) toast(`${s.name}: ${s.notice.text}`, s.notice.error);
      S.noticeSeen[s.id] = s.notice.n;
    } else if (S.noticeSeen[s.id] === undefined) S.noticeSeen[s.id] = 0;
  }
  renderSide();
  renderTunnelPill();
  updateServerHead();
}

function renderTunnelPill() {
  const el = $("#tunnel-pill");
  if (!el || !S.tunnel) return;
  const st = S.tunnel.state;
  const txt = st === "open" ? "открыт" : st === "reconnecting" ? "переподключение" : "закрыт";
  el.replaceChildren(h("span", { class: "pill", title: "Доступ из интернета (SSH-туннель)", onclick: () => go("#/tunnel") },
    h("span", { class: "dot " + (st === "open" ? "running" : st === "reconnecting" ? "starting" : "") }),
    h("span", { class: "lbl" }, "Доступ: "), txt,
    st === "open" && S.tunnel.address ? h("span", { class: "muted lbl" }, " · " + S.tunnel.address) : null));
}

function renderSide() {
  const side = $(".side");
  if (!side) return;
  const r = S.route;
  side.replaceChildren(
    h("h3", {}, "Мои серверы"),
    S.servers.map((s) => h("div", { class: "srv" + (r.page === "server" && r.id === s.id ? " on" : ""), onclick: () => { side.classList.remove("open"); go("#/s/" + s.id + "/" + (r.tab || "console")); } },
      h("span", { class: "dot " + s.status }),
      h("div", { class: "grow" }, h("div", { class: "name" }, s.name),
        h("div", { class: "sub" }, s.status === "running" ? `${s.status_ru} · игроков: ${s.players}` : s.status_ru, s.busy ? " · ⏳" : "")))),
    S.servers.length ? null : h("div", { class: "muted small", style: "padding:4px 8px" }, "Пока нет серверов"),
    h("button", { class: "btn", style: "margin-top:6px", onclick: () => { side.classList.remove("open"); go("#/new"); } }, "+ Добавить сервер"),
    h("div", { class: "nav" },
      h("a", { href: "#/tunnel", class: r.page === "tunnel" ? "on" : "" }, "🌐 Доступ из интернета"),
      S.user.admin ? h("a", { href: "#/admin", class: r.page === "admin" ? "on" : "" }, "⚙ Администрирование",
        S.pendingUsers ? h("span", { class: "badge" }, S.pendingUsers) : null) : null));
}

function renderMain() {
  clearTimers();
  const main = $(".main");
  if (!main) return;
  main.replaceChildren();
  const r = S.route;
  if (r.page === "server") return renderServer(main, r);
  if (r.page === "new") return renderEditor(main, null);
  if (r.page === "tunnel") return renderTunnel(main);
  if (r.page === "admin" && S.user.admin) return renderAdmin(main);
  // главная: открываем первый сервер или пустой экран
  const first = () => {
    if (S.servers.length) return go("#/s/" + S.servers[0].id + "/console");
    main.replaceChildren(h("div", { class: "empty" },
      h("h2", {}, "Добро пожаловать!"),
      h("p", {}, "Добавьте сервер: укажите папку, где лежит jar-файл сервера Minecraft."),
      h("button", { class: "btn primary", onclick: () => go("#/new") }, "+ Добавить сервер"),
      S.user.admin ? h("p", { class: "small" }, "Серверы из десктоп-версии можно перенести в разделе ",
        h("a", { href: "#/admin" }, "Администрирование"), ".") : null));
  };
  if (S.servers.length) first();
  else GET("/api/servers").then((x) => { S.servers = x.servers; first(); }).catch(() => first());
}

// ------------------------------------------------------------------ сервер
const TABS = [
  ["console", "Консоль"],
  ["stats", "Статистика"],
  ["access", "Игроки и доступ"],
  ["props", "Настройки сервера"],
  ["world", "Мир и бэкапы"],
  ["mods", "Моды"],
  ["edit", "Параметры запуска"],
];
const cur = () => S.servers.find((s) => s.id === S.route.id);

function renderServer(main, r) {
  const s = cur();
  if (!s) {
    GET("/api/servers/" + r.id).then((x) => {
      if (!S.servers.find((y) => y.id === x.id)) S.servers.push(x);
      renderMain();
    }).catch(() => { main.replaceChildren(h("div", { class: "empty" }, h("h2", {}, "Сервер не найден"))); });
    return;
  }
  main.append(
    h("div", { class: "head", id: "srv-head" }),
    h("div", { class: "tabs" }, TABS.map(([k, l]) => h("a", { href: `#/s/${s.id}/${k}`, class: r.tab === k ? "on" : "" }, l))),
    h("div", { id: "tab" }));
  updateServerHead();
  const tab = $("#tab");
  ({ console: tabConsole, stats: tabStats, access: tabAccess, props: tabProps, world: tabWorld, mods: tabMods, edit: (t, x) => renderEditor(t, x) }[r.tab] || tabConsole)(tab, s);
}

function updateServerHead() {
  const head = $("#srv-head");
  const s = cur();
  if (!head || !s) return;
  const st = s.status;
  const running = st === "running" || st === "starting";
  head.replaceChildren(
    h("h2", {}, s.name),
    h("span", { class: "status " + st }, s.status_ru),
    running && s.uptime !== null ? h("span", { class: "muted small" }, "⏱ " + dur(s.uptime)) : null,
    h("span", { class: "muted small" }, "порт " + s.port),
    s.busy ? h("span", { class: "busy" }, h("span", { class: "spin" }), s.busy) : null,
    !s.path_ok ? h("span", { class: "err small" }, "Папка сервера не найдена") : null,
    h("div", { class: "actions" },
      h("button", { class: "btn primary", disabled: st !== "stopped", onclick: () => startServer(s) }, "▶ Запустить"),
      h("button", { class: "btn", disabled: st !== "running" && st !== "starting", onclick: () => act(() => POST(`/api/servers/${s.id}/stop`), "Останавливаю…").then(refreshServers) }, "■ Остановить"),
      h("button", { class: "btn", disabled: st === "stopping", onclick: () => act(() => POST(`/api/servers/${s.id}/restart`), "Перезапуск…").then(refreshServers) }, "↻ Перезапустить"),
      h("button", { class: "btn danger", disabled: st === "stopped", title: "Принудительно завершить процесс", onclick: async () => {
        if (await confirmBox("Принудительное завершение", "Процесс будет убит без сохранения мира. Продолжить?", "Завершить", true))
          act(() => POST(`/api/servers/${s.id}/kill`)).then(refreshServers);
      } }, "✕")));
}

async function startServer(s, extra) {
  const r = await act(() => POST(`/api/servers/${s.id}/start`, extra || {}));
  if (!r) return;
  if (r.need_eula) {
    const ok = await modal("Лицензия Minecraft (EULA)", h("div", {},
      "Для запуска нужно принять лицензионное соглашение Minecraft: ",
      h("a", { href: "https://aka.ms/MinecraftEULA", target: "_blank", rel: "noopener" }, "aka.ms/MinecraftEULA"),
      ". Принять и записать eula=true?"),
      [{ label: "Отмена", value: false }, { label: "Принимаю", value: true, cls: "primary" }]);
    if (ok) return startServer(s, { ...(extra || {}), accept_eula: true });
    return;
  }
  if (r.conflict) {
    const ok = await confirmBox("Порт занят", `На порту ${r.conflict.port} уже работает «${r.conflict.name}».\nОстановить его и запустить «${s.name}»?`, "Остановить и запустить");
    if (ok) return startServer(s, { ...(extra || {}), stop_other: true });
    return;
  }
  toast(r.queued ? "Останавливаю другой сервер, затем запущу этот…" : `Запускаю «${s.name}»…`);
  refreshServers();
  if (S.route.tab !== "console") go(`#/s/${s.id}/console`);
}

// --- консоль
function tabConsole(t, s) {
  const out = h("div", { class: "console" });
  const input = h("input", { placeholder: "Команда сервера, например: say Привет (без /)", autocomplete: "off" });
  const auto = h("input", { type: "checkbox", checked: true });
  let counter = null;
  const hist = [];
  let hi = 0;
  async function poll() {
    let r;
    try {
      r = await GET(`/api/servers/${s.id}/console` + (counter === null ? "" : "?since=" + counter));
    } catch (e) { return; }
    if (r.reset) out.replaceChildren();
    const frag = document.createDocumentFragment();
    for (const [lvl, text] of r.lines) frag.append(h("div", { class: lvl }, text));
    const atBottom = out.scrollHeight - out.scrollTop - out.clientHeight < 40;
    out.append(frag);
    while (out.childNodes.length > 5000) out.firstChild.remove();
    if (r.lines.length && auto.checked && (atBottom || r.reset)) out.scrollTop = out.scrollHeight;
    counter = r.counter;
  }
  async function send(cmd) {
    cmd = (cmd ?? input.value).trim();
    if (!cmd) return;
    if (hist[hist.length - 1] !== cmd) hist.push(cmd);
    hi = hist.length;
    input.value = "";
    await act(() => POST(`/api/servers/${s.id}/command`, { command: cmd }));
    poll();
  }
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") send();
    else if (e.key === "ArrowUp" && hi > 0) { input.value = hist[--hi]; e.preventDefault(); }
    else if (e.key === "ArrowDown") { hi = Math.min(hist.length, hi + 1); input.value = hist[hi] || ""; e.preventDefault(); }
  });
  const quick = [["list", "Игроки"], ["save-all", "Сохранить мир"], ["time set day", "День"], ["weather clear", "Ясно"], ["tps", "TPS (Paper)"]];
  t.append(out,
    h("div", { class: "cmdline" }, input, h("button", { class: "btn primary", onclick: () => send() }, "Отправить")),
    h("div", { class: "quick" },
      quick.map(([c, l]) => h("button", { class: "btn small", onclick: () => send(c) }, l)),
      h("span", { class: "grow" }),
      h("label", { class: "small muted row" }, auto, "Автопрокрутка"),
      h("button", { class: "btn small", onclick: () => act(() => POST(`/api/servers/${s.id}/console/clear`)).then(poll) }, "Очистить")));
  every(1000, poll);
  input.focus();
}

// --- статистика
function drawChart(canvas, values, color, minMax, unit, digits) {
  const dpr = window.devicePixelRatio || 1;
  const w = canvas.clientWidth, hgt = canvas.clientHeight;
  canvas.width = w * dpr; canvas.height = hgt * dpr;
  const c = canvas.getContext("2d");
  c.scale(dpr, dpr);
  const css = getComputedStyle(document.documentElement);
  const line = css.getPropertyValue("--line"), muted = css.getPropertyValue("--muted");
  const nums = values.filter((v) => v !== null && v !== undefined);
  const max = Math.max(minMax, ...nums) * 1.1 || 1;
  const padL = 46, padB = 4, padT = 6;
  c.font = "11px system-ui, sans-serif";
  c.fillStyle = muted; c.strokeStyle = line; c.lineWidth = 1;
  for (let i = 0; i <= 3; i++) {
    const y = padT + (hgt - padT - padB) * (i / 3);
    c.beginPath(); c.moveTo(padL, y); c.lineTo(w, y); c.stroke();
    c.fillText((max * (1 - i / 3)).toFixed(digits) + (unit ? " " + unit : ""), 2, y + 4);
  }
  if (!nums.length) return;
  const n = 150, step = (w - padL) / (n - 1), off = n - values.length;
  const pts = values.map((v, i) => v === null || v === undefined ? null : [padL + (off + i) * step, padT + (hgt - padT - padB) * (1 - v / max)]);
  c.strokeStyle = color; c.lineWidth = 2; c.beginPath();
  let started = false;
  for (const p of pts) { if (!p) { started = false; continue; } if (!started) { c.moveTo(p[0], p[1]); started = true; } else c.lineTo(p[0], p[1]); }
  c.stroke();
  const valid = pts.filter(Boolean);
  if (valid.length > 1) {
    c.lineTo(valid[valid.length - 1][0], hgt - padB); c.lineTo(valid[0][0], hgt - padB); c.closePath();
    c.globalAlpha = 0.12; c.fillStyle = color; c.fill(); c.globalAlpha = 1;
  }
}

function tabStats(t, s) {
  const cards = h("div", { class: "cards" });
  const charts = [["CPU", "#e5534b", 10, "%", 1], ["RAM", "#4f8ff7", 1024, "МБ", 0], ["Игроки", "#3fb560", 5, "", 0]]
    .map(([name, color, mm, unit, dg]) => ({ name, color, mm, unit, dg, cv: h("canvas", { class: "chart" }), val: h("span", { class: "muted small" }) }));
  const players = h("div", { class: "players" });
  const motd = h("div", { class: "muted small" });
  t.append(cards, motd, h("div", { class: "stats-grid" },
    h("div", {}, charts.map((c) => h("div", { class: "card" }, h("div", { class: "row" }, h("h3", { class: "grow", style: "margin:0" }, c.name), c.val), c.cv)),
      h("div", { class: "muted small" }, "Графики за последние 5 минут")),
    h("div", { class: "card" }, h("h3", {}, "Игроки онлайн"), players)));
  const card = (k, v) => h("div", { class: "stat" }, h("div", { class: "k" }, k), h("div", { class: "v" }, v));
  async function poll() {
    let r;
    try { r = await GET(`/api/servers/${s.id}/stats`); } catch (e) { return; }
    const st = r.stats || {};
    const p = st.ping;
    cards.replaceChildren(
      card("Статус", r.status_ru), card("Время работы", dur(r.uptime)),
      card("Игроки", p ? `${p.online} / ${p.max}` : r.running ? String(r.players.length) : "—"),
      card("Версия", p ? p.version : "—"),
      card("Пинг (локально)", p && p.latency_ms !== null ? p.latency_ms + " мс" : "—"),
      card("CPU процесса", st.cpu !== undefined && st.cpu !== null ? st.cpu.toFixed(1) + " %" : "—"),
      card("RAM процесса", st.rss ? size(st.rss) : "—"),
      card("RAM системы", st.sys_total ? `${size(st.sys_used)} / ${size(st.sys_total)}` : "—"),
      card("Размер мира", size(r.world_size)));
    motd.textContent = p && p.motd ? "MOTD: " + p.motd.replace(/\n/g, " ") : "";
    const hist = r.history || [];
    const series = [hist.map((x) => x[1]), hist.map((x) => x[2]), hist.map((x) => x[3])];
    charts[1].mm = parseMem(r.xmx) || 1024;
    charts.forEach((c, i) => {
      drawChart(c.cv, series[i], c.color, c.mm, c.unit, c.dg);
      const last = series[i].filter((v) => v !== null).pop();
      c.val.textContent = last === undefined ? "" : last.toFixed(c.dg) + (c.unit ? " " + c.unit : "");
    });
    players.replaceChildren(...(r.players.length ? r.players.map((n) => h("div", { class: "player" },
      h("span", {}, "👤 " + n), h("button", { class: "btn small", onclick: (e) => playerMenu(e, s, n) }, "Действия ▾")))
      : [h("div", { class: "muted" }, r.running ? "Никого нет" : "Сервер не запущен")]));
  }
  every(2000, poll);
}

async function playerMenu(e, s, name) {
  e.stopPropagation();
  const send = (c) => act(() => POST(`/api/servers/${s.id}/command`, { command: c }), "Отправлено: " + c);
  const items = [
    ["Кик…", async () => { const v = await promptBox("Кик " + name, [{ label: "Причина (необязательно)" }]); if (v) send(`kick ${name} ${v[0]}`.trim()); }],
    ["Бан…", async () => { const v = await promptBox("Бан " + name, [{ label: "Причина (необязательно)" }]); if (v) send(`ban ${name} ${v[0]}`.trim()); }],
    ["Сообщение…", async () => { const v = await promptBox("Сообщение для " + name, [{ label: "Текст" }]); if (v && v[0]) send(`tell ${name} ${v[0]}`); }],
    ["Выдать OP", () => send("op " + name)],
    ["Снять OP", () => send("deop " + name)],
    ["Режим: выживание", () => send("gamemode survival " + name)],
    ["Режим: творческий", () => send("gamemode creative " + name)],
    ["Режим: приключение", () => send("gamemode adventure " + name)],
    ["Режим: наблюдатель", () => send("gamemode spectator " + name)],
  ];
  document.querySelectorAll(".player .dropdown").forEach((d) => d.remove());
  const dd = h("div", { class: "dropdown" }, items.map(([l, f]) => h("button", { onclick: () => { dd.remove(); f(); } }, l)));
  const host = e.target.parentElement;
  host.style.position = "relative";
  host.append(dd);
  setTimeout(() => document.addEventListener("click", () => dd.remove(), { once: true }));
}

// --- игроки и доступ
function tabAccess(t, s) {
  let kind = "ops";
  let data = null;
  const box = h("div");
  t.append(box);
  async function load() {
    data = await act(() => GET(`/api/servers/${s.id}/lists`));
    if (data) draw();
  }
  function draw() {
    const L = data.lists[kind];
    const sel = new Set();
    const name = h("input", { placeholder: kind === "banned-ips" ? "IP-адрес" : "Ник игрока", class: "grow" });
    const reason = h("input", { placeholder: "Причина (необязательно)", class: "grow" });
    const ban = kind.startsWith("banned");
    const add = async () => {
      if (!name.value.trim()) return;
      const r = await act(() => POST(`/api/servers/${s.id}/lists`, { kind, name: name.value.trim(), reason: reason.value }));
      if (r) { toast(r.via_console ? "Команда отправлена в консоль" : "Добавлено"); setTimeout(load, r.via_console ? 1500 : 0); }
    };
    name.addEventListener("keydown", (e) => { if (e.key === "Enter") add(); });
    const removeBtn = h("button", { class: "btn danger", disabled: true, onclick: async () => {
      const r = await act(() => POST(`/api/servers/${s.id}/lists/remove`, { kind, names: [...sel] }));
      if (r) setTimeout(load, r.via_console ? 1500 : 0);
    } }, "Удалить выбранных");
    const rows = L.rows.map((row) => {
      const key = row[L.key];
      const cb = h("input", { type: "checkbox", onchange: () => { cb.checked ? sel.add(key) : sel.delete(key); removeBtn.disabled = !sel.size; } });
      return h("tr", {}, h("td", {}, cb), L.columns.map(([f]) => h("td", { class: f === "uuid" ? "mono small" : "" }, row[f])));
    });
    box.replaceChildren(
      h("div", { class: "subtabs" }, ["ops", "whitelist", "banned-players", "banned-ips"].map((k) => [k, data.lists[k]]).map(([k, v]) =>
        h("button", { class: k === kind ? "on" : "", onclick: () => { kind = k; draw(); } }, `${v.title} (${v.rows.length})`))),
      h("div", { class: "muted small", style: "margin-bottom:10px" },
        data.online_mode ? "online-mode = true: UUID берутся у Mojang (лицензия)." : "online-mode = false: офлайн-UUID (пиратки).",
        kind === "whitelist" ? (data.whitelist_on ? " Белый список включён." : " Белый список выключен (white-list в настройках сервера).") : "",
        data.running ? " Сервер запущен: изменения идут командами в консоль." : " Сервер остановлен: изменения пишутся прямо в файлы."),
      h("div", { class: "card" },
        h("div", { class: "row" }, name, ban ? reason : null, h("button", { class: "btn primary", onclick: add }, "Добавить")),
        h("div", { class: "table-wrap", style: "margin-top:12px" }, h("table", {},
          h("thead", {}, h("tr", {}, h("th", { style: "width:30px" }), L.columns.map(([, title]) => h("th", {}, title)))),
          h("tbody", {}, rows.length ? rows : h("tr", {}, h("td", { colspan: 9, class: "muted" }, "Список пуст"))))),
        h("div", { class: "row", style: "margin-top:10px" }, removeBtn, h("button", { class: "btn", onclick: load }, "Обновить"))));
  }
  load();
}

// --- server.properties
function tabProps(t, s) {
  const box = h("div");
  t.append(box);
  const changed = {};
  async function load() {
    const r = await act(() => GET(`/api/servers/${s.id}/properties`));
    if (!r) return;
    for (const k of Object.keys(changed)) delete changed[k];
    const filter = h("input", { placeholder: "Поиск параметра…", class: "grow" });
    const save = h("button", { class: "btn primary", disabled: true, onclick: async () => {
      const x = await act(() => PUT(`/api/servers/${s.id}/properties`, { values: changed }));
      if (x) { toast(x.restart_needed ? "Сохранено. Изменения применятся после перезапуска сервера" : "Сохранено"); load(); }
    } }, "Сохранить");
    const mark = (tr, key, val, orig) => {
      if (val === orig) delete changed[key]; else changed[key] = val;
      tr.classList.toggle("changed", val !== orig);
      save.disabled = !Object.keys(changed).length;
      save.textContent = Object.keys(changed).length ? `Сохранить (${Object.keys(changed).length})` : "Сохранить";
    };
    const rows = r.items.map((it) => {
      let input;
      if (it.type === "bool") input = h("select", {}, ["true", "false"].map((o) => h("option", { value: o, selected: it.value.trim().toLowerCase() === o }, o === "true" ? "да (true)" : "нет (false)")));
      else if (it.type === "choice") {
        const opts = it.choices.includes(it.value) ? it.choices : [it.value, ...it.choices];
        input = h("select", {}, opts.map((o) => h("option", { value: o, selected: o === it.value }, o)));
      } else input = h("input", { value: it.value, type: it.type === "int" ? "number" : "text" });
      const tr = h("tr", { "data-q": (it.key + " " + it.desc).toLowerCase() }, h("td", {}, it.key), h("td", { class: "muted small" }, it.desc), h("td", {}, input));
      input.addEventListener("input", () => mark(tr, it.key, input.value, it.value));
      input.addEventListener("change", () => mark(tr, it.key, input.value, it.value));
      return tr;
    });
    filter.addEventListener("input", () => {
      const q = filter.value.toLowerCase();
      rows.forEach((tr) => tr.classList.toggle("hidden", !!q && !tr.dataset.q.includes(q)));
    });
    const addBtn = h("button", { class: "btn", onclick: async () => {
      const v = await promptBox("Новый параметр", [{ label: "Ключ" }, { label: "Значение" }], "Добавить");
      if (v && v[0].trim()) { changed[v[0].trim()] = v[1]; save.click(); }
    } }, "+ Параметр");
    box.replaceChildren(
      r.exists ? null : h("div", { class: "card" }, "Файла server.properties ещё нет: он появится после первого запуска сервера."),
      r.running ? h("div", { class: "muted small", style: "margin-bottom:8px" }, "Сервер запущен: изменения вступят в силу после перезапуска.") : null,
      h("div", { class: "row", style: "margin-bottom:10px" }, filter, addBtn, save),
      h("div", { class: "card table-wrap", style: "padding:6px 10px" }, h("table", { class: "props" },
        h("thead", {}, h("tr", {}, h("th", {}, "Параметр"), h("th", {}, "Описание"), h("th", { style: "width:38%" }, "Значение"))),
        h("tbody", {}, rows))));
  }
  load();
}

// --- мир и бэкапы
function tabWorld(t, s) {
  const box = h("div");
  t.append(box);
  async function load() {
    const r = await act(() => GET(`/api/servers/${s.id}/backups`));
    if (!r) return;
    const label = h("input", { placeholder: "Метка (необязательно); «keep» — не удалять при ротации", class: "grow" });
    const others = S.servers.filter((x) => x.id !== s.id);
    const target = h("select", {}, others.map((o) => h("option", { value: o.id }, o.name)));
    box.replaceChildren(
      h("div", { class: "card" },
        h("h3", {}, "Мир"),
        h("div", {}, "Папка мира: ", h("b", {}, r.level_name), r.worlds.length ? h("span", { class: "muted" }, " (" + r.worlds.join(", ") + ")") : h("span", { class: "err" }, " — мира пока нет")),
        h("div", { class: "muted small" }, "Бэкапы хранятся в: ", h("span", { class: "mono" }, r.dir), " · папку и автобэкап можно поменять в «Параметрах запуска»"),
        h("div", { class: "row", style: "margin-top:12px" }, label,
          h("button", { class: "btn primary", onclick: async () => {
            const x = await act(() => POST(`/api/servers/${s.id}/backups`, { label: label.value }));
            if (x) { toast("Бэкап создаётся в фоне…"); waitIdle(load); }
          } }, "Создать бэкап"))),
      h("div", { class: "card" },
        h("h3", {}, `Бэкапы (${r.backups.length})`),
        h("div", { class: "table-wrap" }, h("table", {},
          h("thead", {}, h("tr", {}, h("th", {}, "Файл"), h("th", {}, "Дата"), h("th", {}, "Размер"), h("th", {}))),
          h("tbody", {}, r.backups.length ? r.backups.map((b) => h("tr", {},
            h("td", { class: "mono small" }, b.name), h("td", {}, b.time), h("td", {}, size(b.size)),
            h("td", { class: "acts" },
              h("a", { class: "btn small", href: `/api/servers/${s.id}/backups/download/${encodeURIComponent(b.name)}`, download: b.name }, "Скачать"),
              h("button", { class: "btn small", onclick: async () => {
                if (!(await confirmBox("Восстановить мир", `Текущий мир будет заменён содержимым ${b.name}.\nПеред этим он сохранится в бэкап «before-restore».`, "Восстановить"))) return;
                if (await act(() => POST(`/api/servers/${s.id}/backups/restore`, { name: b.name }))) { toast("Восстанавливаю…"); waitIdle(load); }
              } }, "Восстановить"),
              h("button", { class: "btn small danger", onclick: async () => {
                if (!(await confirmBox("Удалить бэкап", b.name, "Удалить", true))) return;
                if (await act(() => POST(`/api/servers/${s.id}/backups/delete`, { names: [b.name] }))) load();
              } }, "Удалить")))) : h("tr", {}, h("td", { colspan: 4, class: "muted" }, "Бэкапов пока нет")))))),
      h("div", { class: "card" },
        h("h3", {}, "Перенос мира"),
        others.length ? h("div", { class: "row" }, "Скопировать мир этого сервера в", target,
          h("button", { class: "btn", onclick: async () => {
            const o = others.find((x) => x.id === target.value);
            if (!(await confirmBox("Копирование мира", `Мир «${o.name}» будет заменён миром «${s.name}» (старый сохранится в бэкап «before-copy»).`, "Скопировать"))) return;
            if (await act(() => POST(`/api/servers/${s.id}/world/copy`, { to: o.id }))) toast("Копирую мир…");
          } }, "Копировать")) : h("div", { class: "muted small" }, "Других серверов нет."),
        h("div", { class: "row", style: "margin-top:10px" }, "Импортировать мир из папки (например, .minecraft/saves/Мир):",
          h("button", { class: "btn", onclick: async () => {
            const p = await pickFolder("Папка мира (с level.dat)");
            if (!p) return;
            if (!(await confirmBox("Импорт мира", `Текущий мир будет заменён миром из\n${p}\n(старый сохранится в бэкап «before-import»).`, "Импортировать"))) return;
            if (await act(() => POST(`/api/servers/${s.id}/world/import`, { path: p }))) { toast("Импортирую мир…"); waitIdle(load); }
          } }, "Выбрать папку…"))));
  }
  load();
}

/** Ждёт окончания фоновой операции сервера и вызывает fn. */
function waitIdle(fn) {
  const id = S.route.id;
  let n = 0;
  const t = setInterval(async () => {
    n++;
    await refreshServers();
    const s = S.servers.find((x) => x.id === id);
    if (!s || !s.busy || n > 600) { clearInterval(t); if (S.route.id === id) fn(); }
  }, 1000);
}

// --- моды
function tabMods(t, s) {
  const box = h("div");
  t.append(box);
  async function load() {
    const r = await act(() => GET(`/api/servers/${s.id}/mods`));
    if (!r) return;
    const sel = new Set();
    const keyOf = (m) => m.folder + "/" + m.file;
    const filter = h("input", { placeholder: "Поиск…", class: "grow" });
    const toggle = async (items, enabled) => {
      const x = await act(() => POST(`/api/servers/${s.id}/mods/toggle`, { items, enabled }));
      if (x) { if (x.restart_needed) toast("Изменения применятся после перезапуска сервера"); load(); }
    };
    const selected = () => r.mods.filter((m) => sel.has(keyOf(m))).map((m) => ({ folder: m.folder, file: m.file }));
    const rows = r.mods.map((m) => {
      const cb = h("input", { type: "checkbox", onchange: () => (cb.checked ? sel.add(keyOf(m)) : sel.delete(keyOf(m))) });
      const sw = h("input", { type: "checkbox", checked: m.enabled, title: m.enabled ? "Выключить" : "Включить", onchange: () => toggle([{ folder: m.folder, file: m.file }], sw.checked) });
      return h("tr", { class: m.enabled ? "" : "off", "data-q": (m.name + " " + m.file).toLowerCase() },
        h("td", {}, cb), h("td", {}, sw),
        h("td", {}, h("div", {}, h("b", {}, m.name)), m.desc ? h("div", { class: "muted small" }, m.desc.slice(0, 160)) : null),
        h("td", {}, m.version), h("td", {}, m.loader), h("td", { class: "mono small" }, m.folder + "/" + m.file), h("td", {}, size(m.size)));
    });
    filter.addEventListener("input", () => {
      const q = filter.value.toLowerCase();
      rows.forEach((tr) => tr.classList.toggle("hidden", !!q && !tr.dataset.q.includes(q)));
    });
    const fileIn = h("input", { type: "file", multiple: true, accept: ".jar", class: "hidden" });
    const folder = h("select", {}, ["mods", "plugins"].map((f) => h("option", { value: f, selected: r.dirs[0] === f }, f)));
    fileIn.addEventListener("change", async () => {
      if (!fileIn.files.length) return;
      const fd = new FormData();
      fd.append("folder", folder.value);
      for (const f of fileIn.files) fd.append("files", f, f.name);
      toast("Загружаю…");
      const x = await act(() => api("POST", `/api/servers/${s.id}/mods/upload`, fd));
      if (x) { toast("Загружено: " + x.saved.join(", ")); load(); }
    });
    const on = r.mods.filter((m) => m.enabled).length;
    box.replaceChildren(
      h("div", { class: "muted small", style: "margin-bottom:8px" },
        `Всего ${r.mods.length}, включено ${on}. Выключенный мод переименовывается в .jar.disabled, как в Prism Launcher.`,
        r.running ? " Сервер запущен: изменения применятся после перезапуска." : ""),
      h("div", { class: "row", style: "margin-bottom:10px" }, filter,
        h("button", { class: "btn", onclick: () => toggle(selected(), true) }, "Включить выбранные"),
        h("button", { class: "btn", onclick: () => toggle(selected(), false) }, "Выключить выбранные"),
        h("button", { class: "btn danger", onclick: async () => {
          const it = selected();
          if (!it.length) return;
          if (!(await confirmBox("Удалить моды", it.map((m) => m.file).join("\n"), "Удалить", true))) return;
          if (await act(() => POST(`/api/servers/${s.id}/mods/delete`, { items: it, enabled: false }))) load();
        } }, "Удалить")),
      h("div", { class: "row", style: "margin-bottom:10px" }, "Загрузить .jar в", folder, fileIn,
        h("button", { class: "btn primary", onclick: () => fileIn.click() }, "Выбрать файлы…"),
        h("button", { class: "btn", onclick: load }, "Обновить")),
      r.mods.length
        ? h("div", { class: "card table-wrap", style: "padding:6px 10px" }, h("table", {},
          h("thead", {}, h("tr", {}, h("th", {}), h("th", {}, "Вкл"), h("th", {}, "Мод"), h("th", {}, "Версия"), h("th", {}, "Загрузчик"), h("th", {}, "Файл"), h("th", {}, "Размер"))),
          h("tbody", {}, rows)))
        : h("div", { class: "card muted" }, "В папках mods и plugins нет модов."));
  }
  load();
}

// --- параметры запуска (создание и редактирование)
async function renderEditor(t, s) {
  const isNew = !s;
  const v = Object.assign({
    name: "", path: "", launch_mode: "jar", jar: "", custom_args: "", java: "java", xms: "1G", xmx: "4G",
    jvm_args: "", server_args: "nogui", backup_dir: "", keep_backups: 10, auto_backup_min: 0, backup_on_stop: false,
  }, s || {});
  const f = {
    name: h("input", { value: v.name }),
    path: h("input", { value: v.path, class: "grow mono" }),
    mode: h("select", {}, h("option", { value: "jar", selected: v.launch_mode === "jar" }, "jar-файл (Vanilla, Paper, Fabric, старый Forge)"),
      h("option", { value: "args", selected: v.launch_mode === "args" }, "аргументы (Forge/NeoForge 1.17+)")),
    jar: h("input", { value: v.jar, list: "jars", class: "mono" }),
    jars: h("datalist", { id: "jars" }),
    args: h("input", { value: v.custom_args, class: "mono" }),
    java: h("select", {}),
    javaCustom: h("input", { value: v.java, class: "mono", placeholder: "Путь к java/java.exe" }),
    xms: h("input", { value: v.xms, style: "width:90px" }),
    xmx: h("input", { value: v.xmx, style: "width:90px" }),
    jvm: h("input", { value: v.jvm_args, class: "mono" }),
    sargs: h("input", { value: v.server_args, class: "mono" }),
    bdir: h("input", { value: v.backup_dir, class: "grow mono", placeholder: "по умолчанию: <папка сервера>/backups" }),
    keep: h("input", { type: "number", min: 0, value: v.keep_backups, style: "width:90px" }),
    auto: h("input", { type: "number", min: 0, value: v.auto_backup_min, style: "width:90px" }),
    onstop: h("input", { type: "checkbox", checked: v.backup_on_stop }),
  };
  const syncMode = () => {
    jarField.classList.toggle("hidden", f.mode.value !== "jar");
    argsField.classList.toggle("hidden", f.mode.value !== "args");
  };
  const jarField = h("div", { class: "full form" }, h("label", {}, "Jar-файл"), h("div", {}, f.jar, f.jars));
  const argsField = h("div", { class: "full form" }, h("label", {}, "Аргументы запуска"), f.args,
    h("div", { class: "hint" }, "Например: @user_jvm_args.txt @libraries/net/minecraftforge/forge/1.20.1-47.2.0/win_args.txt"));
  f.mode.addEventListener("change", syncMode);
  async function detect(p, fill) {
    try {
      const r = await GET("/api/detect?path=" + encodeURIComponent(p));
      f.jars.replaceChildren(...r.jars.map((j) => h("option", { value: j })));
      if (fill) {
        f.mode.value = r.launch_mode; f.jar.value = r.jar; f.args.value = r.custom_args;
        if (!f.name.value.trim()) f.name.value = r.name;
        syncMode();
      }
    } catch (e) { if (fill) toast(e.message, true); }
  }
  f.path.addEventListener("change", () => detect(f.path.value, true));
  const javaList = await GET("/api/java").catch(() => ({ java: [{ path: "java", label: "java (PATH)" }] }));
  const javaOpts = javaList.java.map((j) => h("option", { value: j.path, selected: j.path === v.java }, j.label + (j.path !== "java" ? "  —  " + j.path : "")));
  const known = javaList.java.some((j) => j.path === v.java);
  f.java.replaceChildren(...javaOpts, S.user.admin ? h("option", { value: "__custom", selected: !known }, "Другой путь…") : null);
  const syncJava = () => f.javaCustom.classList.toggle("hidden", f.java.value !== "__custom");
  f.java.addEventListener("change", syncJava);
  if (!known && !S.user.admin) f.java.value = "java";

  const collect = () => ({
    name: f.name.value, path: f.path.value, launch_mode: f.mode.value, jar: f.jar.value, custom_args: f.args.value,
    java: f.java.value === "__custom" ? f.javaCustom.value : f.java.value, xms: f.xms.value, xmx: f.xmx.value,
    jvm_args: f.jvm.value, server_args: f.sargs.value, backup_dir: f.bdir.value,
    keep_backups: parseInt(f.keep.value) || 0, auto_backup_min: parseInt(f.auto.value) || 0, backup_on_stop: f.onstop.checked,
  });
  const save = async () => {
    const body = collect();
    const r = await act(() => (isNew ? POST("/api/servers", body) : PUT("/api/servers/" + s.id, body)), isNew ? "Сервер добавлен" : "Сохранено");
    if (!r) return;
    await refreshServers();
    go(`#/s/${r.id}/${isNew ? "console" : "edit"}`);
  };
  t.append(
    isNew ? h("div", { class: "head" }, h("h2", {}, "Новый сервер")) : null,
    h("div", { class: "card" }, h("div", { class: "form" },
      h("label", {}, "Папка сервера"), h("div", { class: "row" }, f.path,
        h("button", { class: "btn", onclick: async () => { const p = await pickFolder("Папка сервера", f.path.value); if (p) { f.path.value = p; detect(p, true); } } }, "Обзор…")),
      h("div", { class: "hint" }, "Папка на компьютере с панелью, где лежит jar сервера. Способ запуска определится сам."),
      h("label", {}, "Название"), f.name,
      h("label", {}, "Способ запуска"), f.mode,
      jarField, argsField,
      h("label", {}, "Java"), h("div", { style: "display:grid;gap:6px" }, f.java, f.javaCustom),
      h("div", { class: "hint" }, "Для 1.20.5+ нужна Java 21, для 1.17–1.20.4 — Java 17, для старых — Java 8."),
      h("label", {}, "Память (мин / макс)"), h("div", { class: "row" }, f.xms, "—", f.xmx, h("span", { class: "muted small" }, "например 2G, 6G")),
      h("label", {}, "Доп. аргументы JVM"), f.jvm,
      h("label", {}, "Аргументы сервера"), f.sargs)),
    h("div", { class: "card" }, h("h3", {}, "Бэкапы"), h("div", { class: "form" },
      h("label", {}, "Папка бэкапов"), h("div", { class: "row" }, f.bdir,
        h("button", { class: "btn", onclick: async () => { const p = await pickFolder("Папка для бэкапов", f.bdir.value || f.path.value); if (p) f.bdir.value = p; } }, "Обзор…")),
      h("label", {}, "Хранить последних"), h("div", { class: "row" }, f.keep, h("span", { class: "muted small" }, "0 = хранить все; бэкапы с «keep» в имени не удаляются")),
      h("label", {}, "Автобэкап каждые, мин"), h("div", { class: "row" }, f.auto, h("span", { class: "muted small" }, "0 = выключен; делается, пока сервер работает")),
      h("label", {}, "Бэкап после остановки"), h("label", { class: "row" }, f.onstop, "Делать бэкап после каждой остановки сервера"))),
    h("div", { class: "row" },
      h("button", { class: "btn primary", onclick: save }, isNew ? "Добавить сервер" : "Сохранить"),
      isNew ? h("button", { class: "btn", onclick: () => history.back() }, "Отмена") : null,
      h("span", { class: "grow" }),
      !isNew ? h("button", { class: "btn danger", onclick: async () => {
        if (!(await confirmBox("Убрать сервер из списка", `«${s.name}» пропадёт из панели. Файлы сервера на диске останутся.`, "Убрать", true))) return;
        if (await act(() => DEL("/api/servers/" + s.id))) { await refreshServers(); go("#/"); }
      } }, "Убрать из списка") : null));
  syncMode();
  syncJava();
  if (v.path) detect(v.path, false);
  if (isNew) f.path.focus();
}

// ------------------------------------------------------------------ туннель
const SSH_HELP = "Туннель работает без окна, поэтому вход на сервер должен быть по SSH-ключу, а не по паролю. " +
  "Один раз выполните на этом ПК: ssh-keygen -t ed25519 (Enter на все вопросы), затем " +
  "type %USERPROFILE%\\.ssh\\id_ed25519.pub | ssh minecraft@ex1.ru \"mkdir -p ~/.ssh && cat >> ~/.ssh/authorized_keys\"";

function tunnelCard(t, opts) {
  // opts: {title, load(), save(body), start(), stop(), extra(r) -> nodes}
  const box = h("div");
  t.append(box);
  async function draw(r) {
    if (!r) r = await act(opts.load);
    if (!r) return;
    const tn = opts.pick(r);
    const cmd = h("input", { value: tn.settings.command, class: "mono grow" });
    const rec = h("input", { type: "checkbox", checked: tn.settings.auto_reconnect });
    const st = tn.state;
    const extraFields = opts.fields ? opts.fields(r) : [];
    box.replaceChildren(h("div", { class: "card" },
      h("div", { class: "row" }, h("h3", { class: "grow", style: "margin:0" }, opts.title),
        h("span", { class: "status " + (st === "open" ? "running" : st === "reconnecting" ? "starting" : "") },
          st === "open" ? "Открыт" : st === "reconnecting" ? "Переподключение…" : "Закрыт"),
        tn.wanted || st !== "closed"
          ? h("button", { class: "btn", onclick: async () => draw(await act(opts.stop)) }, "Закрыть")
          : h("button", { class: "btn primary", onclick: async () => draw(await act(opts.start)) }, "Открыть")),
      tn.address ? h("div", { class: "muted small", style: "margin-top:6px" }, opts.addrText(tn.address)) : null,
      h("div", { class: "form", style: "margin-top:12px" },
        h("label", {}, "Команда"), cmd,
        h("label", {}, "Переподключение"), h("label", { class: "row" }, rec, "Переподключаться при обрыве"),
        extraFields.map((x) => [h("label", {}, x.label), h("label", { class: "row" }, x.input, x.text)])),
      h("div", { class: "row", style: "margin-top:12px" },
        h("button", { class: "btn primary", onclick: async () => {
          const body = opts.body(r, cmd.value, rec.checked, extraFields);
          const x = await act(() => opts.save(body), "Сохранено");
          if (x) draw(x);
        } }, "Сохранить"),
        h("button", { class: "btn", onclick: () => draw() }, "Обновить")),
      h("div", { class: "muted small", style: "margin-top:10px" }, SSH_HELP),
      h("h3", { style: "margin-top:14px" }, "Журнал"),
      h("div", { class: "log" }, tn.log.length ? tn.log.join("\n") : "пусто")));
  }
  draw();
  every(3000, () => { if (!box.querySelector("input:focus")) draw(); });
}

function renderTunnel(main) {
  main.append(h("div", { class: "head" }, h("h2", {}, "Доступ из интернета")),
    h("p", { class: "muted" }, "SSH-туннель пробрасывает порт Minecraft на внешний сервер, чтобы друзья могли подключиться. Каждый пользователь настраивает свой туннель."));
  tunnelCard(main, {
    title: "Туннель для серверов Minecraft",
    load: () => GET("/api/tunnel"),
    pick: (r) => r,
    start: () => POST("/api/tunnel/start"),
    stop: () => POST("/api/tunnel/stop"),
    save: (b) => PUT("/api/tunnel", b),
    addrText: (a) => "Адрес для игроков: " + a,
    fields: (r) => [
      { label: "Вместе с сервером", input: h("input", { type: "checkbox", checked: r.settings.start_with_server }), text: "Открывать при запуске сервера" },
      { label: "", input: h("input", { type: "checkbox", checked: r.settings.stop_with_server }), text: "Закрывать при остановке сервера" },
    ],
    body: (r, command, rec, ex) => ({ ...r.settings, command, auto_reconnect: rec, start_with_server: ex[0].input.checked, stop_with_server: ex[1].input.checked }),
  });
}

// ------------------------------------------------------------------ администрирование
function renderAdmin(main) {
  main.append(h("div", { class: "head" }, h("h2", {}, "Администрирование")));
  const users = h("div");
  const settings = h("div");
  main.append(users, settings);
  async function loadUsers() {
    const r = await act(() => GET("/api/admin/users"));
    if (!r) return;
    S.pendingUsers = r.users.filter((u) => !u.approved).length;
    renderSide();
    const upd = (u, b, msg) => act(() => POST("/api/admin/users/" + u.id, b), msg).then((x) => x && loadUsers());
    users.replaceChildren(h("div", { class: "card" }, h("h3", {}, "Пользователи"), h("div", { class: "table-wrap" }, h("table", {},
      h("thead", {}, h("tr", {}, h("th", {}, "Логин"), h("th", {}, "Создан"), h("th", {}, "Серверов"), h("th", {}, "Статус"), h("th", {}))),
      h("tbody", {}, r.users.map((u) => h("tr", {},
        h("td", {}, h("b", {}, u.username), u.id === S.user.id ? h("span", { class: "muted" }, " (вы)") : null),
        h("td", {}, u.created), h("td", {}, u.servers),
        h("td", {}, !u.approved ? h("span", { class: "status starting" }, "ждёт одобрения") : u.admin ? h("span", { class: "status running" }, "администратор") : "пользователь"),
        h("td", { class: "acts" },
          u.id === S.user.id ? null : [
            !u.approved ? h("button", { class: "btn small primary", onclick: () => upd(u, { approved: true }, "Одобрен") }, "Одобрить")
              : h("button", { class: "btn small", onclick: () => upd(u, { approved: false }, "Заблокирован") }, "Заблокировать"),
            u.approved ? h("button", { class: "btn small", onclick: () => upd(u, { admin: !u.admin }) }, u.admin ? "Снять админа" : "Сделать админом") : null,
            h("button", { class: "btn small", onclick: async () => {
              const v = await promptBox("Новый пароль для " + u.username, [{ label: "Пароль", type: "password" }]);
              if (v && v[0]) upd(u, { password: v[0] }, "Пароль изменён");
            } }, "Пароль"),
            h("button", { class: "btn small danger", onclick: async () => {
              if (await confirmBox("Удалить пользователя", `Удалить ${u.username}? Его серверы пропадут из панели (файлы на диске останутся).`, "Удалить", true))
                act(() => DEL("/api/admin/users/" + u.id), "Удалён").then((x) => x && loadUsers());
            } }, "Удалить")])))))),
      h("div", { class: "muted small", style: "margin-top:8px" }, "Важно: пользователь панели может запускать программы на этом компьютере (Java-серверы, ssh). Одобряйте только тех, кому доверяете.")));
  }
  async function loadSettings() {
    const r = await act(() => GET("/api/admin/settings"));
    if (!r) return;
    const reg = h("input", { type: "checkbox", checked: r.open_registration });
    const roots = h("textarea", { rows: 3, class: "mono", placeholder: "Например:\nD:\\Minecraft\\Друзья" }, r.allowed_roots.join("\n"));
    const timeout = h("input", { type: "number", min: 10, max: 600, value: r.stop_timeout, style: "width:90px" });
    const saveAll = (extra) => PUT("/api/admin/settings", Object.assign({
      open_registration: reg.checked, allowed_roots: roots.value.split("\n"), stop_timeout: parseInt(timeout.value) || 60,
      panel_tunnel_command: r.panel_tunnel.settings.command, panel_tunnel_autostart: r.panel_tunnel_autostart,
      panel_tunnel_reconnect: r.panel_tunnel.settings.auto_reconnect,
    }, extra || {}));
    const panelBox = h("div");
    settings.replaceChildren(
      h("div", { class: "card" }, h("h3", {}, "Настройки панели"), h("div", { class: "form" },
        h("label", {}, "Регистрация"), h("label", { class: "row" }, reg, "Без одобрения администратора (не рекомендуется)"),
        h("label", {}, "Папки пользователей"), roots,
        h("div", { class: "hint" }, "Если заполнено, обычные пользователи смогут держать серверы и бэкапы только внутри этих папок (по одной в строке). Пусто — без ограничений."),
        h("label", {}, "Таймаут остановки, с"), timeout),
        h("div", { class: "row", style: "margin-top:12px" },
          h("button", { class: "btn primary", onclick: () => act(saveAll, "Сохранено").then((x) => x && loadSettings()) }, "Сохранить"),
          h("span", { class: "grow" }),
          h("button", { class: "btn", onclick: async () => {
            const x = await act(() => POST("/api/admin/import-desktop"));
            if (x) { toast(x.added ? `Перенесено серверов: ${x.added}` : "Новых серверов в десктоп-версии не найдено"); refreshServers(); }
          } }, "Перенести серверы из десктоп-версии")),
        h("div", { class: "muted small", style: "margin-top:8px" }, `Порт панели: ${r.port}. Данные: ${r.data_dir}`)),
      panelBox);
    tunnelCard(panelBox, {
      title: "Туннель панели (доступ к этой странице через домен)",
      load: () => GET("/api/admin/settings"),
      pick: (x) => x.panel_tunnel,
      start: () => POST("/api/admin/panel-tunnel/start"),
      stop: () => POST("/api/admin/panel-tunnel/stop"),
      save: (b) => PUT("/api/admin/settings", b),
      addrText: () => "Панель на внешнем сервере доступна на порту из команды; снаружи её открывает nginx по вашему домену (см. web/deploy в репозитории).",
      fields: (x) => [{ label: "Автозапуск", input: h("input", { type: "checkbox", checked: x.panel_tunnel_autostart }), text: "Открывать при запуске панели" }],
      body: (x, command, rec, ex) => ({
        open_registration: x.open_registration, allowed_roots: x.allowed_roots, stop_timeout: x.stop_timeout,
        panel_tunnel_command: command, panel_tunnel_reconnect: rec, panel_tunnel_autostart: ex[0].input.checked,
      }),
    });
  }
  loadUsers();
  loadSettings();
}

boot();
