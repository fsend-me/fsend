"use strict";

const cd = document.getElementById("countdown");
const expires = Number(cd.dataset.expires);

const on = document.getElementById("expires-on");
on.textContent = new Date(expires * 1000).toLocaleString(undefined, {
  month: "short", day: "numeric", hour: "2-digit", minute: "2-digit",
});

const pad = (n) => String(n).padStart(2, "0");
function tick() {
  let s = Math.max(0, Math.floor(expires - Date.now() / 1000));
  if (s <= 0) { cd.textContent = "expired"; return; }
  const d = Math.floor(s / 86400); s %= 86400;
  const h = Math.floor(s / 3600); s %= 3600;
  const m = Math.floor(s / 60); s %= 60;
  cd.textContent = d ? `${d}d ${pad(h)}:${pad(m)}` : `${pad(h)}:${pad(m)}:${pad(s)}`;
}
tick();
setInterval(tick, 1000);

const url = document.getElementById("url");
url.addEventListener("focus", () => url.select());
document.getElementById("copy").addEventListener("click", async (e) => {
  const b = e.currentTarget;
  try { await navigator.clipboard.writeText(url.value); } catch { url.select(); document.execCommand("copy"); }
  b.textContent = "Copied!";
  b.classList.add("ok");
  setTimeout(() => { b.textContent = "Copy link"; b.classList.remove("ok"); }, 1400);
});
