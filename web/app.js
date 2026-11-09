"use strict";

const $ = (s, el = document) => el.querySelector(s);
const $$ = (s, el = document) => [...el.querySelectorAll(s)];
const ORIGIN = location.origin;
const HISTORY_KEY = "fsend.history";
const TOKEN_KEY = "fsend.token";
const LOCK_KEY = "fsend.lockedUntil";

const store = {
  get(k, d) { try { const v = localStorage.getItem(k); return v === null ? d : JSON.parse(v); } catch { return d; } },
  set(k, v) { try { localStorage.setItem(k, JSON.stringify(v)); } catch {} },
  del(k) { try { localStorage.removeItem(k); } catch {} },
};

let cfg = { auth_required: false, max_upload_size: 10 * 1024 ** 3, default_days: 3, max_days: 30 };

// ---------- formatting ----------
function humanSize(n) {
  const u = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1024 && i < u.length - 1) { n /= 1024; i++; }
  return i === 0 ? `${n} B` : `${+n.toFixed(1)} ${u[i]}`;
}

function fmtDays(d) {
  if (d < 1) { const h = Math.round(d * 24); return `${h} hour${h === 1 ? "" : "s"}`; }
  return `${+d.toFixed(2)} day${d === 1 ? "" : "s"}`;
}

function shortLeft(expiresAt) {
  let s = Math.floor(expiresAt - Date.now() / 1000);
  if (s <= 0) return "expired";
  const d = Math.floor(s / 86400); s -= d * 86400;
  const h = Math.floor(s / 3600); s -= h * 3600;
  const m = Math.floor(s / 60);
  if (d) return `${d}d ${h}h`;
  if (h) return `${h}h ${m}m`;
  return `${Math.max(m, 1)}m`;
}

const ext = (n) => (n.includes(".") ? n.split(".").pop().slice(0, 4).toUpperCase() : "FILE");
const isImage = (f) => /^image\/(png|jpeg|gif|webp|avif|bmp)$/.test(f.type);

async function copy(text, btn) {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    const ta = document.createElement("textarea");
    ta.value = text; document.body.appendChild(ta); ta.select();
    document.execCommand("copy"); ta.remove();
  }
  const old = btn.textContent;
  btn.textContent = "Copied!";
  btn.classList.add("ok");
  setTimeout(() => { btn.textContent = old; btn.classList.remove("ok"); }, 1400);
}

// ---------- tabs ----------
function showTab(name) {
  $$(".tab").forEach((x) => {
    const on = x.dataset.tab === name;
    x.classList.toggle("active", on);
    x.setAttribute("aria-selected", String(on));
    x.tabIndex = on ? 0 : -1;
  });
  $("#tab-web").hidden = name !== "web";
  $("#tab-cli").hidden = name !== "cli";
}
function pickTab(t) {
  showTab(t.dataset.tab);
  history.replaceState(null, "", t.dataset.tab === "cli" ? "#cli" : location.pathname);
}
$$(".tab").forEach((t) => {
  t.addEventListener("click", () => pickTab(t));
  t.addEventListener("keydown", (e) => {
    if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
    const tabs = $$(".tab");
    const next = tabs[(tabs.indexOf(t) + (e.key === "ArrowRight" ? 1 : tabs.length - 1)) % tabs.length];
    pickTab(next);
    next.focus();
  });
});
if (location.hash === "#cli") showTab("cli");

// ---------- CLI snippets (token flags only when the server requires one) ----------
function renderSnippets() {
  const vars = {
    origin: ORIGIN,
    token: cfg.auth_required ? " --token YOUR_TOKEN" : "",
    curl_token: cfg.auth_required ? " --oauth2-bearer YOUR_TOKEN" : "",
  };
  $$("[data-tpl]").forEach((el) => {
    if (!el.dataset.src) el.dataset.src = el.textContent;
    el.textContent = el.dataset.src.replace(/\{\{(\w+)\}\}/g, (_, k) => vars[k] ?? "");
    const pre = el.closest(".term");
    if (!pre || pre.hasAttribute("data-nocopy")) return;
    const host = pre.closest(".example") || pre;
    if (host.querySelector(".term-copy")) return;
    const b = document.createElement("button");
    b.type = "button";
    b.className = "term-copy";
    b.textContent = "Copy";
    b.addEventListener("click", () => copy(el.textContent, b));
    host.appendChild(b);
  });
  $$("[data-auth]").forEach((el) => (el.hidden = !cfg.auth_required));
}

// ---------- config ----------
async function loadConfig() {
  try { cfg = await (await fetch("/api/config")).json(); } catch {}
  $("#max-size").textContent = humanSize(cfg.max_upload_size);
  $$(".js-default-days-text").forEach((el) => (el.textContent = fmtDays(cfg.default_days)));
  $$(".js-default-days").forEach((el) => (el.textContent = cfg.default_days));
  $$(".js-max-days").forEach((el) => (el.textContent = cfg.max_days));
  $("#max-days").textContent = fmtDays(cfg.max_days);
  $("#token-field").hidden = !cfg.auth_required;
  $("#token").value = store.get(TOKEN_KEY, "");
  renderSnippets();

  const choices = [1 / 24, 6 / 24, 1, 3, 7, 14, 30, 90, 365].filter((d) => d <= cfg.max_days);
  if (!choices.includes(cfg.default_days)) choices.push(cfg.default_days);
  choices.sort((a, b) => a - b);
  const sel = $("#days");
  sel.innerHTML = "";
  for (const d of choices) {
    const o = new Option(fmtDays(d), d);
    if (d === cfg.default_days) o.selected = true;
    sel.add(o);
  }
}

// ---------- token feedback & brute-force lockout ----------
const drop = $("#drop");
const input = $("#file");

function tokenEntered() {
  store.set(TOKEN_KEY, $("#token").value.trim());
  sendWaiting();
}
$("#token").addEventListener("change", tokenEntered);
$("#token").addEventListener("keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); tokenEntered(); } });
$("#token").addEventListener("input", () => tokenHint(null));

function tokenHint(msg) {
  $("#token-field").classList.toggle("invalid", !!msg);
  $("#token-hint").hidden = !msg;
  $("#token-hint").textContent = msg || "";
}

function shake(el) {
  el.classList.remove("shake");
  void el.offsetWidth;
  el.classList.add("shake");
}

let lockTimer = null;
const lockedUntil = () => store.get(LOCK_KEY, 0);
const isLocked = () => lockedUntil() > Date.now();

function setLock(seconds) {
  store.set(LOCK_KEY, Date.now() + seconds * 1000);
  applyLock();
}

function applyLock() {
  clearInterval(lockTimer);
  const render = () => {
    const ms = lockedUntil() - Date.now();
    const locked = ms > 0;
    $("#lock-banner").hidden = !locked;
    drop.classList.toggle("locked", locked);
    drop.setAttribute("aria-disabled", String(locked));
    $("#token").disabled = locked;
    if (!locked) {
      clearInterval(lockTimer);
      store.del(LOCK_KEY);
      return;
    }
    const s = Math.ceil(ms / 1000);
    $("#lock-left").textContent = `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
  };
  render();
  if (isLocked()) lockTimer = setInterval(render, 1000);
}

// ---------- drop zone ----------
drop.addEventListener("keydown", (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); input.click(); } });
input.addEventListener("change", () => { handleFiles(input.files); input.value = ""; });
["dragenter", "dragover"].forEach((ev) =>
  document.addEventListener(ev, (e) => { e.preventDefault(); drop.classList.add("over"); })
);
["dragleave", "drop"].forEach((ev) =>
  document.addEventListener(ev, (e) => {
    e.preventDefault();
    if (ev === "dragleave" && e.relatedTarget) return;
    drop.classList.remove("over");
  })
);
document.addEventListener("drop", (e) => handleFiles(e.dataTransfer.files));
document.addEventListener("paste", (e) => {
  const files = [...(e.clipboardData?.files || [])];
  if (files.length) handleFiles(files);
});

// ---------- upload queue ----------
// Files are sent one at a time. Without a token they wait (nothing is lost);
// a rejected token stops the queue, so one drop can't burn several attempts.
const queue = [];
const waiting = [];
let running = false;

const needsToken = () => cfg.auth_required && !$("#token").value.trim();
const reducedMotion = () => matchMedia("(prefers-reduced-motion: reduce)").matches;

function announce(msg) {
  const live = $("#live");
  live.textContent = "";
  setTimeout(() => { live.textContent = msg; }, 50);
}

function askToken(msg) {
  tokenHint(msg);
  $("#token").focus();
  shake($("#token-field"));
}

function revealTicket(li) {
  const r = li.getBoundingClientRect();
  if (r.top >= 0 && r.bottom <= innerHeight) return;
  li.scrollIntoView({ behavior: reducedMotion() ? "auto" : "smooth", block: "nearest" });
}

function handleFiles(list) {
  const files = [...list];
  if (!files.length) return;
  if (isLocked()) { shake($("#lock-banner")); return; }
  showTab("web");
  const first = $("#uploads").firstChild;
  const items = files.map((file) => {
    const it = { file, li: makeItem(file.name, file.size, isImage(file) ? URL.createObjectURL(file) : null) };
    $(".retry", it.li).addEventListener("click", () => retry(it));
    $(".dismiss", it.li).addEventListener("click", () => dismiss(it));
    $("#uploads").insertBefore(it.li, first);
    return it;
  });
  updateSent();
  if (needsToken()) {
    hold(items);
    askToken(items.length === 1 ? "Enter your upload token to send this file." : `Enter your upload token to send these ${items.length} files.`);
    return;
  }
  revealTicket(items[0].li);
  enqueue(items);
}

function enqueue(items) {
  for (const it of items) {
    const li = it.li;
    li.classList.remove("failed", "waiting");
    $(".error", li).hidden = true;
    $(".progress", li).hidden = false;
    setProgress(li, 0);
    $(".ticket-meta", li).textContent = humanSize(it.file.size);
    setStub(li, "Queued", "…");
    queue.push(it);
  }
  run();
}

async function run() {
  if (running) return;
  running = true;
  while (queue.length) {
    const it = queue.shift();
    const outcome = await upload(it);
    if (outcome === "auth") {
      hold([it, ...queue.splice(0)]);
    } else if (outcome === "locked") {
      for (const x of queue.splice(0)) fail(x, "Not sent: uploads are paused.", true);
    }
  }
  running = false;
}

function hold(items) {
  for (const it of items) {
    const li = it.li;
    li.classList.add("waiting");
    li.classList.remove("failed");
    $(".progress", li).hidden = true;
    $(".error", li).hidden = false;
    $(".error", li).classList.add("pending");
    $(".error-text", li).textContent = "Waiting for the upload token.";
    $(".retry", li).hidden = false;
    $(".retry", li).textContent = "Send";
    $(".dismiss", li).hidden = false;
    $(".ticket-meta", li).textContent = humanSize(it.file.size);
    setStub(li, "Waiting", "Token");
    if (!waiting.includes(it)) waiting.push(it);
  }
}

function sendWaiting() {
  if (!waiting.length || needsToken() || isLocked()) return;
  enqueue(waiting.splice(0));
}

function retry(it) {
  if (isLocked()) { shake($("#lock-banner")); return; }
  if (needsToken()) { hold([it]); askToken("Enter your upload token to send this file."); return; }
  const i = waiting.indexOf(it);
  if (i >= 0) waiting.splice(i, 1);
  enqueue([it]);
}

function dismiss(it) {
  const i = waiting.indexOf(it);
  if (i >= 0) waiting.splice(i, 1);
  it.li.remove();
  updateSent();
}

// ---------- tickets ----------
function makeItem(name, size, thumbSrc) {
  const li = $("#item-tpl").content.firstElementChild.cloneNode(true);
  $(".ticket-name", li).textContent = name;
  $(".ticket-name", li).title = name;
  $(".ticket-meta", li).textContent = humanSize(size);
  const t = $(".thumb", li);
  if (thumbSrc) {
    const img = new Image();
    img.src = thumbSrc;
    img.alt = "";
    img.onerror = () => { img.remove(); t.textContent = ext(name); };
    t.appendChild(img);
  } else {
    t.textContent = ext(name);
  }
  return li;
}

function setStub(li, label, value) {
  $(".stub-label", li).textContent = label;
  $(".stub-value", li).textContent = value;
}

function updateSent() {
  const n = $("#uploads").children.length;
  $("#sent").hidden = n === 0;
  $("#sent-count").textContent = n;
}

function setProgress(li, pct) {
  $(".bar", li).style.width = pct + "%";
  $(".progress", li).setAttribute("aria-valuenow", String(Math.floor(pct)));
}

// Resolves to "ok", "auth" (token missing or wrong), "locked" or "failed".
function upload(it) {
  const { file, li } = it;
  return new Promise((resolve) => {
    if (isLocked()) { fail(it, "Not sent: uploads are paused.", true); return resolve("locked"); }
    if (file.size > cfg.max_upload_size) {
      fail(it, `Too large: the limit is ${humanSize(cfg.max_upload_size)} per file.`, false);
      return resolve("failed");
    }
    setStub(li, "Uploading", "0%");

    const xhr = new XMLHttpRequest();
    xhr.open("PUT", "/" + encodeURIComponent(file.name || "file"));
    xhr.setRequestHeader("Accept", "application/json");
    xhr.setRequestHeader("Max-Days", $("#days").value);
    const md = $("#max-downloads").value;
    if (md) xhr.setRequestHeader("Max-Downloads", md);
    const token = $("#token").value.trim();
    if (token) xhr.setRequestHeader("Authorization", "Bearer " + token);

    const started = Date.now();
    xhr.upload.onprogress = (e) => {
      if (!e.lengthComputable) return;
      const pct = (e.loaded / e.total) * 100;
      setProgress(li, pct);
      setStub(li, "Uploading", `${Math.floor(pct)}%`);
      const secs = (Date.now() - started) / 1000;
      const speed = secs > 0.3 ? ` · ${humanSize(e.loaded / secs)}/s` : "";
      $(".ticket-meta", li).textContent = `${humanSize(e.loaded)} of ${humanSize(e.total)}${speed}`;
    };
    xhr.onload = () => {
      if (xhr.status === 201) {
        const r = JSON.parse(xhr.responseText);
        const entry = {
          url: r.url, delete_url: r.delete_url, filename: r.filename, size: r.size,
          expires_at: r.expires_at, max_downloads: r.max_downloads,
          image: isImage(file) && !r.max_downloads,
        };
        const hist = store.get(HISTORY_KEY, []);
        hist.unshift(entry);
        store.set(HISTORY_KEY, hist.slice(0, 50));
        tokenHint(null);
        showResult(li, entry);
        announce(`${r.filename} uploaded. The link is ready to copy.`);
        return resolve("ok");
      }
      const msg = (xhr.responseText || `Upload failed (${xhr.status})`).trim();
      if (xhr.status === 401) {
        store.del(TOKEN_KEY);
        const left = xhr.getResponseHeader("X-Auth-Attempts-Left");
        askToken(left === null
          ? "Upload token required."
          : `Wrong token · ${left} attempt${left === "1" ? "" : "s"} left`);
        $("#token").select();
        announce(left === null ? "Upload token required." : "Wrong upload token. Files are waiting.");
        return resolve("auth");
      }
      if (xhr.status === 429) {
        setLock(Number(xhr.getResponseHeader("Retry-After")) || 900);
        tokenHint(null);
        fail(it, msg, true);
        return resolve("locked");
      }
      fail(it, msg, xhr.status >= 500);
      resolve("failed");
    };
    xhr.onerror = () => { fail(it, "Network error: the upload did not finish.", true); resolve("failed"); };
    xhr.send(file);
  });
}

function fail(it, msg, canRetry) {
  const li = it.li;
  li.classList.add("failed");
  li.classList.remove("waiting");
  $(".progress", li).hidden = true;
  $(".error", li).hidden = false;
  $(".error", li).classList.remove("pending");
  $(".error-text", li).textContent = msg;
  $(".retry", li).hidden = !canRetry;
  $(".retry", li).textContent = "Retry";
  $(".dismiss", li).hidden = false;
  $(".ticket-meta", li).textContent = humanSize(it.file.size);
  setStub(li, "Status", "FAILED");
  announce(`${it.file.name}: ${msg}`);
}

function showResult(li, entry) {
  li.classList.add("done");
  li.dataset.expires = entry.expires_at;
  $(".progress", li).hidden = true;
  $(".result", li).hidden = false;
  $(".url", li).value = entry.url;
  const limit = entry.max_downloads
    ? `${entry.max_downloads} download${entry.max_downloads > 1 ? "s" : ""}`
    : "unlimited downloads";
  $(".ticket-meta", li).textContent = `${humanSize(entry.size)} · ${limit}`;
  setStub(li, "Expires in", shortLeft(entry.expires_at));
  $(".url", li).addEventListener("focus", (e) => e.target.select());
  $(".copy", li).addEventListener("click", (e) => copy(entry.url, e.currentTarget));
  $(".open", li).href = entry.url;
  $(".qr-btn", li).addEventListener("click", () => {
    const q = $(".qr", li);
    if (!q.firstChild) {
      const img = new Image();
      img.src = entry.url + "/qr.svg";
      img.alt = "QR code for " + entry.filename;
      const note = document.createElement("span");
      note.className = "muted";
      note.textContent = "Scan to open on a phone";
      q.append(img, note);
    }
    q.hidden = !q.hidden;
  });
  $(".del", li).addEventListener("click", async (e) => {
    if (!confirm(`Delete "${entry.filename}" now? The link will stop working.`)) return;
    const btn = e.currentTarget;
    btn.disabled = true;
    $(".error", li).hidden = true;
    let status = 0;
    try { status = (await fetch(entry.delete_url, { method: "DELETE" })).status; } catch {}
    // 404: already gone (expired, used up, or deleted elsewhere), so it's fine to drop it.
    if (status !== 200 && status !== 404) {
      btn.disabled = false;
      const msg = status === 429
        ? "Couldn't delete: too many requests. Try again in a few minutes."
        : "Couldn't delete the file. Check your connection and try again.";
      $(".error", li).hidden = false;
      $(".error", li).classList.remove("pending");
      $(".error-text", li).textContent = msg;
      announce(msg);
      return;
    }
    store.set(HISTORY_KEY, store.get(HISTORY_KEY, []).filter((h) => h.url !== entry.url));
    li.remove();
    updateSent();
    announce(`${entry.filename} deleted.`);
  });
}

// ---------- history ----------
function renderHistory() {
  const now = Date.now() / 1000;
  const hist = store.get(HISTORY_KEY, []).filter((h) => h.expires_at > now);
  store.set(HISTORY_KEY, hist);
  for (const h of hist) {
    const li = makeItem(h.filename, h.size, h.image ? h.url + "?inline=1" : null);
    $("#uploads").appendChild(li);
    showResult(li, h);
  }
  updateSent();
}

function tickExpiry() {
  $$("#uploads .ticket.done").forEach((li) => {
    const left = shortLeft(Number(li.dataset.expires));
    if (left === "expired") li.remove();
    else $(".stub-value", li).textContent = left;
  });
  updateSent();
}

$("#clear-history").addEventListener("click", () => {
  store.set(HISTORY_KEY, []);
  $$("#uploads .ticket.done, #uploads .ticket.failed").forEach((li) => li.remove());
  updateSent();
});

renderSnippets();
loadConfig();
renderHistory();
applyLock();
setInterval(tickExpiry, 30000);
