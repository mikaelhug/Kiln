"use strict";

const body = document.body;
const page = body.dataset.page;
const buildId = Number(body.dataset.build || 0);
const repoId = Number(body.dataset.repo || 0);
const encoder = new TextEncoder();
const baseTitle = document.title;

const ICON_CHECK = '<svg viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M4.5 10.5l3.5 3.5 7.5-8"/></svg>';
const ICON_ALERT = '<svg viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M10 3l8 14H2z"/><path d="M10 8.5v3.5M10 14.5v.01"/></svg>';

function stored(key, fallback) {
  try {
    const value = localStorage.getItem(key);
    return value === null ? fallback : value;
  } catch (_) {
    return fallback;
  }
}

function store(key, value) {
  try { localStorage.setItem(key, value); } catch (_) {}
}

function relative(seconds) {
  const delta = Math.round(Date.now() / 1000 - seconds);
  if (delta < 45) return "just now";
  if (delta < 3600) return `${Math.max(1, Math.round(delta / 60))} min ago`;
  if (delta < 86400) return `${Math.round(delta / 3600)} h ago`;
  if (delta < 86400 * 7) return `${Math.round(delta / 86400)} d ago`;
  return new Date(seconds * 1000).toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" });
}

function duration(seconds) {
  seconds = Math.max(0, Math.round(seconds));
  if (seconds < 60) return `${seconds}s`;
  if (seconds < 3600) return seconds % 60 ? `${Math.floor(seconds / 60)}m ${seconds % 60}s` : `${seconds / 60}m`;
  return `${Math.floor(seconds / 3600)}h ${Math.floor((seconds % 3600) / 60)}m`;
}

function tickTimes() {
  for (const el of document.querySelectorAll("[data-time]")) {
    const at = new Date(Number(el.dataset.time) * 1000);
    el.textContent = relative(Number(el.dataset.time));
    el.title = at.toLocaleString();
    el.setAttribute("datetime", at.toISOString());
  }
}

function tickDurations() {
  const now = Date.now() / 1000;
  for (const el of document.querySelectorAll("[data-started]")) {
    el.textContent = duration(now - Number(el.dataset.started));
  }
}

function toast(text, tone = "ok") {
  const holder = document.querySelector(".toasts") || document.body;
  const el = document.createElement("div");
  el.className = "toast";
  el.setAttribute("role", "status");
  el.innerHTML = tone === "ok" ? ICON_CHECK : ICON_ALERT;
  const span = document.createElement("span");
  span.textContent = text;
  el.append(span);
  holder.append(el);
  setTimeout(() => {
    el.classList.add("leaving");
    setTimeout(() => el.remove(), 220);
  }, 3800);
}

function confirmDialog(form) {
  return new Promise((resolve) => {
    const dialog = document.createElement("dialog");
    dialog.className = "confirm";
    const danger = form.hasAttribute("data-confirm-danger");
    dialog.innerHTML = `<form method="dialog"><div class="body"><span class="ico${danger ? " bad" : ""}">${ICON_ALERT}</span><div><h2></h2><p></p></div></div><div class="buttons"><button class="button" value="cancel" type="submit">Keep</button><button class="button ${danger ? "danger-solid" : "primary"}" value="ok" type="submit"></button></div></form>`;
    dialog.querySelector("h2").textContent = form.dataset.confirmTitle || "Are you sure?";
    dialog.querySelector("p").textContent = form.dataset.confirm;
    dialog.querySelector('[value="ok"]').textContent = form.dataset.confirmAction || "Continue";
    document.body.append(dialog);
    dialog.addEventListener("close", () => {
      resolve(dialog.returnValue === "ok");
      dialog.remove();
    });
    dialog.showModal();
    dialog.querySelector('[value="ok"]').focus();
  });
}

function busy(form) {
  const button = form.querySelector('button[type="submit"]');
  if (!button) return;
  button.classList.add("busy");
  button.setAttribute("aria-busy", "true");
}

function closeMenus(except) {
  for (const menu of document.querySelectorAll("details.menu[open]")) {
    if (menu !== except) menu.open = false;
  }
}

const bound = new WeakMap();
function once(el, key) {
  let keys = bound.get(el);
  if (!keys) bound.set(el, (keys = new Set()));
  if (keys.has(key)) return false;
  keys.add(key);
  return true;
}

function syncAttributes(el, next) {
  for (const { name } of [...el.attributes]) {
    if (next.hasAttribute(name)) continue;
    if (name === "open" && el.tagName === "DETAILS") continue;
    el.removeAttribute(name);
  }
  for (const { name, value } of [...next.attributes]) {
    if (el.getAttribute(name) !== value) el.setAttribute(name, value);
  }
}

function morph(from, to) {
  const incoming = [...to.childNodes];
  incoming.forEach((next, index) => {
    const current = from.childNodes[index];
    if (!current) {
      from.append(next);
      return;
    }
    const same = current.nodeType === next.nodeType && current.nodeName === next.nodeName && (current.nodeType !== 1 || current.id === next.id);
    if (!same) {
      from.replaceChild(next, current);
    } else if (current.nodeType !== 1) {
      if (current.nodeValue !== next.nodeValue) current.nodeValue = next.nodeValue;
    } else {
      syncAttributes(current, next);
      if (current.tagName === "TEMPLATE") current.innerHTML = next.innerHTML;
      else morph(current, next);
    }
  });
  while (from.childNodes.length > incoming.length) from.lastChild.remove();
}

const pending = new Map();
function refresh(selector, url) {
  if (pending.has(selector)) return;
  pending.set(selector, setTimeout(async () => {
    pending.delete(selector);
    const target = document.querySelector(selector);
    if (!target) return;
    if (document.querySelector("dialog[open]")) {
      refresh(selector, url);
      return;
    }
    try {
      const response = await fetch(url, { headers: { "X-Kiln-Fragment": "1" } });
      if (response.status === 401) { location.reload(); return; }
      if (!response.ok) return;
      const next = document.createElement("div");
      next.innerHTML = await response.text();
      const images = next.querySelector("template[data-images]");
      if (images) {
        images.remove();
        const holder = document.querySelector("#images");
        if (holder) {
          const content = document.createElement("div");
          content.innerHTML = images.innerHTML;
          morph(holder, content);
          enhance(holder);
        }
      }
      morph(target, next);
      enhance(target);
      if (selector === "#tasks") syncLog();
      if (selector === "#live" && page === "build") reflectState();
    } catch (_) {}
  }, 150));
}

function refreshPage() {
  if (page === "dashboard") refresh("#live", `/fragments/dashboard${location.search}`);
  if (page === "repos") refresh("#live", "/fragments/repos");
  if (page === "repo") refresh("#live", `/fragments/repo/${repoId}${location.search}`);
  if (page === "build") {
    refresh("#live", `/fragments/build/${buildId}${location.search}`);
    refresh("#tasks", `/fragments/tasks/${buildId}${location.search}`);
  }
}

function reflectState() {
  const marker = document.querySelector("[data-build-state]");
  if (!marker) return;
  const state = marker.dataset.buildState;
  const icon = document.querySelector('link[rel="icon"]');
  if (icon) icon.href = state === "wait" ? "/assets/favicon.svg" : `/assets/favicon-${state}.svg`;
  document.title = `${marker.dataset.buildLabel} · ${baseTitle}`;
}

const log = {
  attempt: 0,
  task: -1,
  bytes: 0,
  lines: 0,
  follow: true,
  pinned: false,
  line: 0,
  wrap: stored("kiln.wrap", "1") === "1",
  query: "",
  hit: -1,
};

function syncLog() {
  if (!log.pinned) {
    const preferred = document.querySelector(".task[data-default]");
    if (preferred && Number(preferred.dataset.task) !== log.task) {
      loadLog(Number(preferred.dataset.task), Number(preferred.dataset.attempt));
      return;
    }
  }
  const current = document.querySelector(`.task[data-task="${log.task}"]`);
  const shown = document.querySelector("#log .log-head")?.dataset.state;
  if (current && shown && current.dataset.state !== shown) loadLog(log.task, log.attempt);
}

function logBody() {
  return document.querySelector("#log .log-body");
}

function nearBottom(el) {
  return el.scrollHeight - el.scrollTop - el.clientHeight < 80;
}

function scrollLog() {
  const el = logBody();
  if (el && log.follow && !log.query) el.scrollTop = el.scrollHeight;
}

function markSelected() {
  for (const el of document.querySelectorAll("[data-task]")) {
    el.classList.toggle("selected", el.classList.contains("task") && Number(el.dataset.task) === log.task);
  }
}

async function loadLog(task, attempt) {
  const holder = document.querySelector("#log");
  if (!holder) return;
  const changed = task !== log.task || attempt !== log.attempt;
  log.task = task;
  log.attempt = attempt;
  markSelected();
  try {
    const response = await fetch(`/fragments/log/${buildId}/${attempt}/${task}`, { headers: { "X-Kiln-Fragment": "1" } });
    if (response.status === 401) { location.reload(); return; }
    if (!response.ok) return;
    const scroller = logBody();
    const keep = !changed && scroller ? scroller.scrollTop : null;
    holder.innerHTML = await response.text();
    log.bytes = Number(holder.querySelector("[data-bytes]")?.dataset.bytes || 0);
    log.lines = holder.querySelectorAll(".line").length;
    enhance(holder);
    const open = holder.querySelector("details[data-open-last]");
    if (open) open.open = true;
    if (changed) log.hit = -1;
    if (log.query) {
      const input = holder.querySelector("[data-find]");
      if (input) input.value = log.query;
      find(false);
    }
    if (log.line) {
      jumpToLine(log.line);
      log.line = 0;
    } else if (keep !== null && !log.follow) {
      logBody().scrollTop = keep;
    } else {
      scrollLog();
    }
  } catch (_) {}
}

function reveal(line) {
  const scroller = logBody();
  if (!scroller) return;
  const top = line.getBoundingClientRect().top - scroller.getBoundingClientRect().top + scroller.scrollTop;
  scroller.scrollTop = top - scroller.clientHeight / 2;
  const box = scroller.getBoundingClientRect();
  if (box.top < 60 || box.bottom > window.innerHeight) document.querySelector("#log")?.scrollIntoView({ block: "nearest" });
}

function errorLine(text) {
  return /^(error|fatal|panic)\b|^\s*(✗|×|✕)|\berror(\[\w+\])?:|\bFAIL(ED)?\b/i.test(text);
}

function jumpToLine(number) {
  const line = document.getElementById(`task-${log.task}-L${number}`);
  if (!line) return;
  for (const el of document.querySelectorAll(".line.target")) el.classList.remove("target");
  line.classList.add("target");
  const details = line.closest("details");
  if (details) details.open = true;
  log.follow = false;
  document.querySelector("[data-follow]")?.classList.remove("on");
  reveal(line);
}

function appendLog(text) {
  const container = [...document.querySelectorAll("#log .lines")].at(-1);
  if (!container) {
    loadLog(log.task, log.attempt);
    return;
  }
  const el = logBody();
  const stick = el && nearBottom(el);
  for (const raw of text.split("\n")) {
    if (raw === "") continue;
    if (raw.startsWith("##kiln[")) {
      loadLog(log.task, log.attempt);
      return;
    }
    log.lines += 1;
    const id = `task-${log.task}-L${log.lines}`;
    const line = document.createElement("div");
    const visible = raw.slice(raw.lastIndexOf("\r") + 1);
    line.className = visible.startsWith("$ ") ? "line cmd" : errorLine(visible) ? "line err" : "line";
    line.id = id;
    const number = document.createElement("a");
    number.className = "ln";
    number.href = `#${id}`;
    number.textContent = String(log.lines);
    const content = document.createElement("span");
    content.className = "tx";
    content.textContent = visible;
    line.append(number, content);
    if (log.query && content.textContent.toLowerCase().includes(log.query)) line.classList.add("match");
    container.append(line);
  }
  if (log.query) updateHits();
  if (stick || log.follow) scrollLog();
}

function matches() {
  return [...document.querySelectorAll("#log .line.match")];
}

function updateHits() {
  const hits = document.querySelector("[data-hits]");
  if (!hits) return;
  const count = matches().length;
  hits.textContent = log.query ? (count ? `${Math.max(0, log.hit) + 1}/${count}` : "0") : "";
}

function find(move) {
  const lines = document.querySelectorAll("#log .line");
  for (const line of lines) {
    const hit = Boolean(log.query) && line.querySelector(".tx").textContent.toLowerCase().includes(log.query);
    line.classList.toggle("match", hit);
    line.classList.remove("current");
  }
  const found = matches();
  if (!found.length) {
    log.hit = -1;
    updateHits();
    return;
  }
  if (move !== false) log.hit = (log.hit + (move === -1 ? -1 : 1) + found.length) % found.length;
  if (log.hit < 0) log.hit = 0;
  const current = found[Math.min(log.hit, found.length - 1)];
  current.classList.add("current");
  const details = current.closest("details");
  if (details) details.open = true;
  if (move !== false) {
    log.follow = false;
    document.querySelector("[data-follow]")?.classList.remove("on");
    reveal(current);
  }
  updateHits();
}

function applyFilter(input) {
  const query = input.value.trim().toLowerCase();
  for (const row of document.querySelectorAll("[data-name]")) {
    row.hidden = Boolean(query) && !row.dataset.name.toLowerCase().includes(query);
  }
}

function enhance(root) {
  for (const form of root.querySelectorAll("form[method='post'], form[method='POST']")) {
    if (!once(form, "submit")) continue;
    form.addEventListener("submit", async (event) => {
      if (form.dataset.confirm && !form.dataset.confirmed) {
        event.preventDefault();
        closeMenus();
        if (await confirmDialog(form)) {
          form.dataset.confirmed = "1";
          busy(form);
          form.requestSubmit();
        }
        return;
      }
      busy(form);
    });
  }
  for (const select of root.querySelectorAll("select[data-autosubmit]")) {
    if (!once(select, "change")) continue;
    select.addEventListener("change", () => select.form.submit());
  }
  for (const menu of root.querySelectorAll("details.menu")) {
    if (!once(menu, "toggle")) continue;
    menu.addEventListener("toggle", () => { if (menu.open) closeMenus(menu); });
  }
  for (const button of root.querySelectorAll("[data-copy]")) {
    if (!once(button, "copy")) continue;
    button.addEventListener("click", async () => {
      try {
        await navigator.clipboard.writeText(button.dataset.copy);
        toast("Copied to the clipboard");
      } catch (_) {
        toast("Copying is not allowed here", "warn");
      }
    });
  }
  for (const link of root.querySelectorAll("[data-task]")) {
    if (link.tagName !== "A" || !once(link, "task")) continue;
    link.addEventListener("click", (event) => {
      event.preventDefault();
      history.replaceState(null, "", `${location.pathname}${location.search}#task-${link.dataset.task}`);
      log.pinned = true;
      log.follow = true;
      loadLog(Number(link.dataset.task), Number(link.dataset.attempt));
      if (link.classList.contains("stage")) document.querySelector("#log")?.scrollIntoView({ block: "start", behavior: "smooth" });
    });
  }
  const filter = root.querySelector("[data-filter-rows]") || document.querySelector("[data-filter-rows]");
  if (filter) {
    if (once(filter, "filter")) filter.addEventListener("input", () => applyFilter(filter));
    applyFilter(filter);
  }
  if (log.task >= 0) markSelected();
  const scroller = root.querySelector(".log-body");
  if (scroller) {
    scroller.classList.toggle("nowrap", !log.wrap);
    scroller.addEventListener("scroll", () => {
      if (!nearBottom(scroller) && log.follow && document.querySelector("[data-follow]")) {
        log.follow = false;
        document.querySelector("[data-follow]")?.classList.remove("on");
      }
    });
    scroller.addEventListener("click", (event) => {
      const number = event.target.closest(".ln");
      if (!number) return;
      event.preventDefault();
      const id = number.getAttribute("href").slice(1);
      history.replaceState(null, "", `${location.pathname}${location.search}#${id}`);
      jumpToLine(Number(id.split("-L")[1]));
    });
  }
  const follow = root.querySelector("[data-follow]");
  if (follow) {
    follow.classList.toggle("on", log.follow);
    follow.setAttribute("aria-pressed", String(log.follow));
    follow.addEventListener("click", () => {
      log.follow = !log.follow;
      follow.classList.toggle("on", log.follow);
      follow.setAttribute("aria-pressed", String(log.follow));
      if (log.follow) scrollLog();
    });
  }
  const wrap = root.querySelector("[data-wrap]");
  if (wrap) {
    wrap.classList.toggle("on", log.wrap);
    wrap.setAttribute("aria-pressed", String(log.wrap));
    wrap.addEventListener("click", () => {
      log.wrap = !log.wrap;
      store("kiln.wrap", log.wrap ? "1" : "0");
      wrap.classList.toggle("on", log.wrap);
      wrap.setAttribute("aria-pressed", String(log.wrap));
      logBody()?.classList.toggle("nowrap", !log.wrap);
    });
  }
  const expand = root.querySelector("[data-expand]");
  if (expand) {
    expand.addEventListener("click", () => {
      const steps = [...document.querySelectorAll("#log details")];
      const open = steps.some((d) => !d.open);
      for (const step of steps) step.open = open;
      expand.title = open ? "Collapse all steps" : "Expand all steps";
    });
  }
  const input = root.querySelector("[data-find]");
  if (input) {
    input.addEventListener("input", () => {
      log.query = input.value.trim().toLowerCase();
      log.hit = -1;
      find(log.query ? 1 : false);
    });
    input.addEventListener("keydown", (event) => {
      if (event.key === "Enter") {
        event.preventDefault();
        find(event.shiftKey ? -1 : 1);
      } else if (event.key === "Escape") {
        input.value = "";
        log.query = "";
        find(false);
        input.blur();
      }
    });
  }
  tickTimes();
  tickDurations();
}

function connect() {
  const live = document.querySelector(".live");
  let source;
  let wasOpen = false;
  const open = () => {
    source = new EventSource("/events");
    source.onopen = () => {
      live?.classList.add("on");
      live?.classList.remove("off");
      if (wasOpen) refreshPage();
      wasOpen = true;
    };
    source.onerror = async () => {
      live?.classList.remove("on");
      live?.classList.add("off");
      try {
        const response = await fetch("/fragments/ping", { headers: { "X-Kiln-Fragment": "1" } });
        if (response.status === 401) location.reload();
      } catch (_) {}
    };
    source.onmessage = onEvent;
  };
  open();
}

function onEvent(message) {
  const event = JSON.parse(message.data);
  if (event.type === "build") {
    if (page === "dashboard" || page === "repos" || (page === "repo" && event.repo === repoId) || (page === "build" && event.build === buildId)) {
      refreshPage();
    }
  } else if (event.type === "repo") {
    if (page === "dashboard" || page === "repos" || (page === "repo" && event.repo === repoId)) refreshPage();
  } else if (event.type === "log" && page === "build" && event.build === buildId) {
    if (event.attempt !== log.attempt || event.task !== log.task) return;
    if (event.offset === log.bytes) {
      log.bytes += encoder.encode(event.text).length;
      appendLog(event.text);
    } else if (event.offset > log.bytes) {
      loadLog(log.task, log.attempt);
    }
  } else if (event.type === "log_reset" && page === "build" && event.build === buildId) {
    if (event.attempt === log.attempt && event.task === log.task) loadLog(log.task, log.attempt);
  }
}

document.addEventListener("click", (event) => {
  if (!event.target.closest("details.menu")) closeMenus();
});

document.addEventListener("keydown", (event) => {
  if (event.key === "Escape") closeMenus();
  const typing = event.target.closest("input, textarea, select, [contenteditable]");
  if (event.key === "/" && !typing && !event.metaKey && !event.ctrlKey) {
    const target = document.querySelector("[data-find]") || document.querySelector("[data-filter-rows]");
    if (target) {
      event.preventDefault();
      target.focus();
      target.select();
    }
  }
});

enhance(document);
setInterval(tickTimes, 30000);
setInterval(tickDurations, 1000);
const flash = document.cookie.split("; ").find((c) => c.startsWith("kiln_flash="));
if (flash) {
  document.cookie = "kiln_flash=; Max-Age=0; Path=/; Secure; SameSite=Strict";
  toast(decodeURIComponent(flash.slice("kiln_flash=".length).replace(/\+/g, " ")));
}
if (page) connect();
if (page === "build") {
  reflectState();
  const fromHash = /^#task-(\d+)(?:-L(\d+))?$/.exec(location.hash);
  const selected = document.querySelector(".task.selected") || document.querySelector(".task[data-default]");
  log.pinned = Boolean(fromHash);
  if (fromHash && fromHash[2]) log.line = Number(fromHash[2]);
  const task = fromHash ? Number(fromHash[1]) : Number(selected?.dataset.task ?? -1);
  const attempt = Number(selected?.dataset.attempt || body.dataset.attempt || 1);
  if (task >= 0) loadLog(task, attempt);
}
