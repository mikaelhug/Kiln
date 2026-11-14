"use strict";

const body = document.body;
const page = body.dataset.page;
const buildId = Number(body.dataset.build || 0);
const repoId = Number(body.dataset.repo || 0);
const encoder = new TextEncoder();

function relative(seconds) {
  const delta = Math.round(Date.now() / 1000 - seconds);
  if (delta < 45) return "just now";
  if (delta < 3600) return `${Math.round(delta / 60)} min ago`;
  if (delta < 86400) return `${Math.round(delta / 3600)} h ago`;
  if (delta < 86400 * 7) return `${Math.round(delta / 86400)} d ago`;
  return new Date(seconds * 1000).toLocaleDateString();
}

function duration(seconds) {
  seconds = Math.max(0, Math.round(seconds));
  if (seconds < 60) return `${seconds}s`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
  return `${Math.floor(seconds / 3600)}h ${Math.floor((seconds % 3600) / 60)}m`;
}

function tickTimes() {
  for (const el of document.querySelectorAll("[data-time]")) {
    el.textContent = relative(Number(el.dataset.time));
    el.title = new Date(Number(el.dataset.time) * 1000).toLocaleString();
  }
}

function tickDurations() {
  const now = Date.now() / 1000;
  for (const el of document.querySelectorAll("[data-started]")) {
    el.textContent = duration(now - Number(el.dataset.started));
  }
}

function toast(text) {
  const el = document.createElement("div");
  el.className = "toast";
  el.textContent = text;
  document.body.append(el);
  setTimeout(() => el.remove(), 3500);
}

const pending = new Map();
function refresh(selector, url) {
  if (pending.has(selector)) return;
  pending.set(selector, setTimeout(async () => {
    pending.delete(selector);
    const target = document.querySelector(selector);
    if (!target) return;
    try {
      const response = await fetch(url, { headers: { "X-Kiln-Fragment": "1" } });
      if (response.status === 401) { location.reload(); return; }
      if (!response.ok) return;
      target.innerHTML = await response.text();
      enhance(target);
      if (selector === "#tasks") syncLog();
    } catch (_) {}
  }, 120));
}

function refreshPage() {
  if (page === "dashboard") refresh("#live", "/fragments/dashboard");
  if (page === "repo") refresh("#live", `/fragments/repo/${repoId}`);
  if (page === "build") {
    refresh("#live", `/fragments/build/${buildId}${location.search}`);
    refresh("#tasks", `/fragments/tasks/${buildId}${location.search}`);
  }
}

const log = {
  attempt: 0,
  task: -1,
  bytes: 0,
  follow: true,
  pinned: false,
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
  if (el && log.follow) el.scrollTop = el.scrollHeight;
}

async function loadLog(task, attempt) {
  const holder = document.querySelector("#log");
  if (!holder) return;
  log.task = task;
  log.attempt = attempt;
  for (const el of document.querySelectorAll(".task")) {
    el.classList.toggle("selected", Number(el.dataset.task) === task);
  }
  try {
    const response = await fetch(`/fragments/log/${buildId}/${attempt}/${task}`);
    if (!response.ok) return;
    holder.innerHTML = await response.text();
    log.bytes = Number(holder.querySelector("[data-bytes]")?.dataset.bytes || 0);
    enhance(holder);
    const open = holder.querySelector("details[data-open-last]");
    if (open) open.open = true;
    scrollLog();
  } catch (_) {}
}

function appendLog(text) {
  const container = document.querySelector("#log .lines:last-of-type") || document.querySelector("#log .log-body");
  if (!container) return;
  const el = logBody();
  const stick = el && nearBottom(el);
  for (const raw of text.split("\n")) {
    if (raw === "") continue;
    if (raw.startsWith("##kiln[")) {
      loadLog(log.task, log.attempt);
      return;
    }
    const line = document.createElement("div");
    line.className = raw.startsWith("$ ") ? "line cmd" : "line";
    line.textContent = raw.slice(raw.lastIndexOf("\r") + 1);
    container.append(line);
  }
  if (stick || log.follow) scrollLog();
}

function enhance(root) {
  const stats = root.querySelector("template[data-stats]");
  if (stats) {
    const target = document.querySelector("#stats");
    if (target) target.innerHTML = stats.innerHTML;
    stats.remove();
  }
  const images = root.querySelector("template[data-images]");
  if (images) {
    let holder = document.querySelector("#images");
    if (!holder) {
      holder = document.createElement("div");
      holder.id = "images";
      document.querySelector("#log")?.after(holder);
    }
    holder.innerHTML = images.innerHTML;
    images.remove();
  }
  if (log.task >= 0) {
    for (const el of root.querySelectorAll(".task[data-task]")) {
      el.classList.toggle("selected", Number(el.dataset.task) === log.task);
    }
  }
  for (const form of root.querySelectorAll("form[data-confirm]")) {
    form.addEventListener("submit", (event) => {
      if (!confirm(form.dataset.confirm)) event.preventDefault();
    });
  }
  for (const link of root.querySelectorAll(".task[data-task]")) {
    link.addEventListener("click", (event) => {
      event.preventDefault();
      history.replaceState(null, "", `${location.pathname}${location.search}#task-${link.dataset.task}`);
      log.pinned = true;
      loadLog(Number(link.dataset.task), Number(link.dataset.attempt));
    });
  }
  const follow = root.querySelector("[data-follow]");
  if (follow) {
    follow.classList.toggle("on", log.follow);
    follow.addEventListener("click", () => {
      log.follow = !log.follow;
      follow.classList.toggle("on", log.follow);
      if (log.follow) scrollLog();
    });
  }
  const scroller = root.querySelector(".log-body");
  if (scroller) {
    scroller.addEventListener("scroll", () => {
      if (!nearBottom(scroller) && log.follow) {
        log.follow = false;
        document.querySelector("[data-follow]")?.classList.remove("on");
      }
    });
  }
  tickTimes();
  tickDurations();
}

function connect() {
  const dot = document.querySelector(".live-dot");
  const source = new EventSource("/events");
  source.onopen = () => dot?.classList.add("on");
  source.onerror = () => dot?.classList.remove("on");
  source.onmessage = (message) => {
    const event = JSON.parse(message.data);
    if (event.type === "build") {
      if (page === "dashboard" || (page === "repo" && event.repo === repoId) || (page === "build" && event.build === buildId)) {
        refreshPage();
      }
    } else if (event.type === "repo") {
      if (page === "dashboard" || page === "repos" || (page === "repo" && event.repo === repoId)) {
        if (page === "repos") refresh("#live", "/fragments/repos");
        else refreshPage();
      }
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
  };
}

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
  const fromHash = /^#task-(\d+)$/.exec(location.hash);
  const selected = document.querySelector(".task.selected") || document.querySelector(".task[data-default]");
  log.pinned = Boolean(fromHash);
  const task = fromHash ? Number(fromHash[1]) : Number(selected?.dataset.task ?? -1);
  const attempt = Number(selected?.dataset.attempt || body.dataset.attempt || 1);
  if (task >= 0) loadLog(task, attempt);
}
