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
  $$(".tab").forEach((x) => x.classList.toggle("active", x.dataset.tab === name));
  $("#tab-web").hidden = name !== "web";
  $("#tab-cli").hidden = name !== "cli";
}
$$(".tab").forEach((t) =>
  t.addEventListener("click", () => {
    showTab(t.dataset.tab);
    history.replaceState(null, "", t.dataset.tab === "cli" ? "#cli" : location.pathname);
  })
);
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

$("#token").addEventListener("change", (e) => store.set(TOKEN_KEY, e.target.value.trim()));
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

function handleFiles(list) {
  const files = [...list];
  if (!files.length) return;
  if (isLocked()) { shake($("#lock-banner")); return; }
  if (cfg.auth_required && !$("#token").value.trim()) {
    tokenHint("Enter your upload token first.");
    $("#token").focus();
    shake($("#token-field"));
    return;
  }
  showTab("web");
  files.reduce((p, f) => p.then(() => upload(f)), Promise.resolve());
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

function upload(file) {
  return new Promise((resolve) => {
    const thumb = isImage(file) ? URL.createObjectURL(file) : null;
    const li = makeItem(file.name, file.size, thumb);
    $("#uploads").prepend(li);
    updateSent();

    if (isLocked()) {
      fail(li, "Uploads are paused: too many wrong tokens.");
      return resolve();
    }
    if (file.size > cfg.max_upload_size) {
      fail(li, `Too large: the limit is ${humanSize(cfg.max_upload_size)} per file.`);
      return resolve();
    }

    const xhr = new XMLHttpRequest();
    xhr.open("PUT", "/" + encodeURIComponent(file.name || "file"));
    xhr.setRequestHeader("Accept", "application/json");
    xhr.setRequestHeader("Max-Days", $("#days").value);
    const md = $("#max-downloads").value;
    if (md) xhr.setRequestHeader("Max-Downloads", md);
    const token = $("#token").value.trim();
    if (token) xhr.setRequestHeader("Authorization", "Bearer " + token);

    const bar = $(".bar", li);
    const started = Date.now();
    xhr.upload.onprogress = (e) => {
      if (!e.lengthComputable) return;
      const pct = (e.loaded / e.total) * 100;
      bar.style.width = pct + "%";
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
      } else {
        const msg = (xhr.responseText || `Upload failed (${xhr.status})`).trim();
        if (xhr.status === 401) {
          store.del(TOKEN_KEY);
          const left = xhr.getResponseHeader("X-Auth-Attempts-Left");
          tokenHint(left === null
            ? "Upload token required."
            : `Wrong token · ${left} attempt${left === "1" ? "" : "s"} left`);
          $("#token").focus();
          $("#token").select();
        } else if (xhr.status === 429) {
          setLock(Number(xhr.getResponseHeader("Retry-After")) || 900);
          tokenHint(null);
        }
        fail(li, msg);
      }
      resolve();
    };
    xhr.onerror = () => { fail(li, "Network error: the upload did not finish."); resolve(); };
    xhr.send(file);
  });
}

function fail(li, msg) {
  li.classList.add("failed");
  $(".progress", li).hidden = true;
  $(".error", li).hidden = false;
  $(".error-text", li).textContent = msg;
  setStub(li, "Status", "FAILED");
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
    e.currentTarget.disabled = true;
    try { await fetch(entry.delete_url, { method: "DELETE" }); } catch {}
    store.set(HISTORY_KEY, store.get(HISTORY_KEY, []).filter((h) => h.url !== entry.url));
    li.remove();
    updateSent();
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
