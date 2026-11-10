use crate::{
    db::{now, FileRow},
    limiter::client_ip,
    util::*,
    AppState,
};
use axum::{
    body::Body,
    extract::{ConnectInfo, Path, Query, Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::{Html, IntoResponse, Response},
    Json,
};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tower::ServiceExt;
use tower_http::services::ServeFile;

type Res = Result<Response, Response>;

fn err(status: StatusCode, msg: &str) -> Response {
    (status, format!("{msg}\n")).into_response()
}

fn internal<E: std::fmt::Display>(e: E) -> Response {
    tracing::error!("internal error: {e}");
    err(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
}

fn wants_html(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("text/html"))
}

fn file_url(origin: &str, f: &FileRow) -> String {
    format!("{origin}/{}/{}", f.id, encode_segment(&f.filename))
}

// ---------- static pages ----------

const INDEX_HTML: &str = include_str!("../web/index.html");
const FILE_HTML: &str = include_str!("../web/file.html");
const NOT_FOUND_HTML: &str = include_str!("../web/404.html");
const APP_JS: &str = include_str!("../web/app.js");
const FILE_JS: &str = include_str!("../web/file.js");
const THEME_JS: &str = include_str!("../web/theme.js");
const STYLE_CSS: &str = include_str!("../web/style.css");
const FAVICON: &str = include_str!("../web/favicon.svg");
/// highlight.js (BSD-3-Clause, web/vendor/highlight.js-LICENSE.txt): common languages plus a few extras.
const HIGHLIGHT_JS: &str = include_str!("../web/vendor/highlight-11.12.0.min.js");
const HIGHLIGHT_JS_NAME: &str = "highlight-11.12.0.min.js";

/// Version shown in page footers: the image tag CI passes in (`v0.1.4`, `sha-abc1234`),
/// or `v<crate version>-dev` for local builds.
pub fn version() -> &'static str {
    static V: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| match option_env!("FSEND_VERSION") {
        Some(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => format!("v{}-dev", env!("CARGO_PKG_VERSION")),
    });
    &V
}

pub async fn index() -> Html<&'static str> {
    static PAGE: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| INDEX_HTML.replace("{{version}}", &html_escape(version())));
    Html(&PAGE)
}

const FONTS: &[(&str, &[u8])] = &[
    ("geist-latin.woff2", include_bytes!("../web/fonts/geist-latin.woff2")),
    ("geist-latin-ext.woff2", include_bytes!("../web/fonts/geist-latin-ext.woff2")),
    ("geist-vietnamese.woff2", include_bytes!("../web/fonts/geist-vietnamese.woff2")),
    ("geist-mono-latin.woff2", include_bytes!("../web/fonts/geist-mono-latin.woff2")),
    ("geist-mono-latin-ext.woff2", include_bytes!("../web/fonts/geist-mono-latin-ext.woff2")),
    ("geist-mono-vietnamese.woff2", include_bytes!("../web/fonts/geist-mono-vietnamese.woff2")),
];

pub async fn font(Path(name): Path<String>) -> Response {
    match FONTS.iter().find(|(n, _)| *n == name) {
        Some((_, bytes)) => (
            [(header::CONTENT_TYPE, "font/woff2"), (header::CACHE_CONTROL, "public, max-age=31536000, immutable")],
            *bytes,
        )
            .into_response(),
        None => err(StatusCode::NOT_FOUND, "Not found"),
    }
}

pub async fn asset(Path(name): Path<String>) -> Response {
    let (ct, body) = match name.as_str() {
        "app.js" => ("text/javascript; charset=utf-8", APP_JS),
        "file.js" => ("text/javascript; charset=utf-8", FILE_JS),
        "theme.js" => ("text/javascript; charset=utf-8", THEME_JS),
        "style.css" => ("text/css; charset=utf-8", STYLE_CSS),
        "favicon.svg" => ("image/svg+xml", FAVICON),
        HIGHLIGHT_JS_NAME => {
            return (
                [(header::CONTENT_TYPE, "text/javascript; charset=utf-8"), (header::CACHE_CONTROL, "public, max-age=31536000, immutable")],
                HIGHLIGHT_JS,
            )
                .into_response();
        }
        _ => return err(StatusCode::NOT_FOUND, "Not found"),
    };
    ([(header::CONTENT_TYPE, ct), (header::CACHE_CONTROL, "public, max-age=300")], body).into_response()
}

pub async fn healthz() -> &'static str {
    "ok"
}

pub async fn config(State(st): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({
        "auth_required": st.cfg.auth_token.is_some(),
        "max_upload_size": st.cfg.max_upload_size,
        "default_days": st.cfg.default_days,
        "max_days": st.cfg.max_days,
    }))
}

const INSTALL_SH: &str = include_str!("../web/install.sh");

pub async fn install_sh(headers: HeaderMap) -> Response {
    let script = INSTALL_SH.replace("__FSEND_ORIGIN__", &origin(&headers));
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], script).into_response()
}

// ---------- upload ----------

fn too_many_attempts(left: std::time::Duration) -> Response {
    too_many(left, "Too many invalid tokens")
}

fn too_many(left: std::time::Duration, what: &str) -> Response {
    let minutes = left.as_secs().div_ceil(60).max(1);
    let mut r = err(
        StatusCode::TOO_MANY_REQUESTS,
        &format!("{what}. Try again in {minutes} minute{}.", if minutes == 1 { "" } else { "s" }),
    );
    if let Ok(v) = HeaderValue::from_str(&left.as_secs().max(1).to_string()) {
        r.headers_mut().insert(header::RETRY_AFTER, v);
    }
    r
}

pub async fn upload(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    Path(name): Path<String>,
    req: Request,
) -> Res {
    let headers = req.headers().clone();
    let ip = client_ip(peer, &headers, st.cfg.trust_proxy);
    if st.cfg.auth_token.is_some() {
        if let Some(left) = st.auth.blocked(ip) {
            return Err(too_many_attempts(left));
        }
    }
    let auth = check_auth(&headers, st.cfg.auth_token.as_deref());
    let mut attempts_left = None;
    if auth == Auth::Invalid {
        tracing::warn!(%ip, "invalid upload token");
        match st.auth.fail(ip) {
            Ok(left) => attempts_left = Some(left),
            Err(lock) => {
                if st.auth.globally_locked() {
                    tracing::warn!(%ip, "server-wide wrong-token budget used up, uploads paused for new clients");
                } else {
                    tracing::warn!(%ip, minutes = lock.as_secs() / 60, "too many invalid tokens, client locked");
                }
                return Err(too_many_attempts(lock));
            }
        }
    }
    if auth == Auth::Ok && st.cfg.auth_token.is_some() {
        st.auth.success(ip);
    }
    if auth != Auth::Ok {
        let from_web = headers
            .get(header::ACCEPT)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("application/json"));
        let left = attempts_left.unwrap_or(0);
        let tries = format!(
            "{left} attempt{} left before a {}-minute lockout.",
            if left == 1 { "" } else { "s" },
            st.cfg.auth_lockout.as_secs() / 60
        );
        let msg = match (auth, from_web) {
            (Auth::Missing, true) => "Upload token required. Enter it in the token field.".to_string(),
            (_, true) => format!("Wrong upload token. {tries}"),
            (Auth::Missing, false) => format!(
                "Error: this server requires an upload token.\n\n  curl --oauth2-bearer <token> --upload-file {} {}/{}\n",
                sanitize_filename(&name).unwrap_or_else(|| "<file>".into()),
                origin(&headers),
                encode_segment(&name)
            ),
            (_, false) => format!("Error: invalid upload token. {tries}\n"),
        };
        let mut r = (StatusCode::UNAUTHORIZED, msg).into_response();
        r.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        if let Some(left) = attempts_left {
            r.headers_mut().insert("x-auth-attempts-left", HeaderValue::from(left));
        }
        return Err(r);
    }
    let filename = sanitize_filename(&name).ok_or_else(|| err(StatusCode::BAD_REQUEST, "Invalid filename"))?;

    let max = st.cfg.max_upload_size;
    let too_large = || err(StatusCode::PAYLOAD_TOO_LARGE, &format!("File too large (max {})", human_size(max)));
    if let Some(len) = headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()?.parse::<u64>().ok()) {
        if len > max {
            return Err(too_large());
        }
    }

    let hdr = |k: &str| headers.get(k).and_then(|v| v.to_str().ok()).map(str::trim).filter(|v| !v.is_empty());
    let days = match hdr("max-days") {
        Some(v) => v
            .parse::<f64>()
            .ok()
            .filter(|d| *d > 0.0 && *d <= st.cfg.max_days)
            .ok_or_else(|| err(StatusCode::BAD_REQUEST, &format!("Max-Days must be > 0 and <= {}", st.cfg.max_days)))?,
        None => st.cfg.default_days,
    };
    let max_downloads = match hdr("max-downloads") {
        Some(v) => Some(
            v.parse::<i64>()
                .ok()
                .filter(|n| *n >= 1)
                .ok_or_else(|| err(StatusCode::BAD_REQUEST, "Max-Downloads must be a positive integer"))?,
        ),
        None => None,
    };

    // Stream the body to a temp file, never buffering it in memory.
    let tmp = st.cfg.data_dir.join("tmp").join(random_id(24));
    let result = async {
        let mut file = tokio::fs::File::create(&tmp).await.map_err(internal)?;
        let mut stream = req.into_body().into_data_stream();
        let mut size: u64 = 0;
        let mut head: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| err(StatusCode::BAD_REQUEST, "Upload interrupted"))?;
            size += chunk.len() as u64;
            if size > max {
                return Err(too_large());
            }
            if head.len() < 512 {
                head.extend_from_slice(&chunk[..chunk.len().min(512 - head.len())]);
            }
            file.write_all(&chunk).await.map_err(internal)?;
        }
        file.sync_all().await.map_err(internal)?;
        if size == 0 {
            return Err(err(StatusCode::BAD_REQUEST, "Empty file"));
        }
        Ok((size, head))
    }
    .await;
    let (size, head) = match result {
        Ok(v) => v,
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(e);
        }
    };

    // Detect type from content (not from the client), only to decide on previews.
    let content_type = infer::get(&head)
        .map(|t| t.mime_type())
        .filter(|m| previewable(m))
        .unwrap_or("application/octet-stream")
        .to_string();

    let delete_token = random_id(32);
    let created_at = now();
    let mut row = FileRow {
        id: String::new(),
        filename,
        size: size as i64,
        content_type,
        created_at,
        expires_at: created_at + (days * 86400.0).round().max(60.0) as i64,
        max_downloads,
        downloads: 0,
    };

    // Short ids collide eventually: retry, and grow the id when the space gets crowded.
    let mut len = st.cfg.id_length;
    let mut inserted = false;
    for attempt in 0..30 {
        if attempt > 0 && attempt % 5 == 0 {
            len += 1;
        }
        row.id = random_id(len);
        if st.db.insert(&row, &sha256_hex(&delete_token)).map_err(internal)? {
            inserted = true;
            break;
        }
    }
    if !inserted {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(internal("could not allocate a unique id"));
    }
    if let Err(e) = tokio::fs::rename(&tmp, st.file_path(&row.id)).await {
        let _ = st.db.delete(&row.id);
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(internal(e));
    }

    let origin = origin(&headers);
    let url = file_url(&origin, &row);
    let delete_url = format!("{url}/{delete_token}");
    let expires = row.expires_at.to_string();
    tracing::info!(id = %row.id, size, "uploaded");

    let mut resp = if headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("application/json"))
    {
        Json(json!({
            "url": url,
            "delete_url": delete_url,
            "id": row.id,
            "filename": row.filename,
            "size": row.size,
            "expires_at": row.expires_at,
            "max_downloads": row.max_downloads,
        }))
        .into_response()
    } else {
        (StatusCode::CREATED, format!("{url}\n")).into_response()
    };
    *resp.status_mut() = StatusCode::CREATED;
    let h = resp.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&delete_url) {
        h.insert("x-url-delete", v);
    }
    h.insert("x-expires-at", HeaderValue::from_str(&expires).unwrap());
    Ok(resp)
}

// ---------- download / info ----------

#[derive(Deserialize)]
pub struct GetQuery {
    dl: Option<String>,
    inline: Option<String>,
}

fn not_found(headers: &HeaderMap) -> Response {
    if wants_html(headers) {
        (StatusCode::NOT_FOUND, Html(NOT_FOUND_HTML)).into_response()
    } else {
        err(StatusCode::NOT_FOUND, "Not found (the file may have expired)")
    }
}

/// How long probing for missing links is counted, and blocked once over the limit.
pub const PROBE_WINDOW: std::time::Duration = std::time::Duration::from_secs(600);

/// Runs a lookup by link, counting "not found" answers per client so short ids
/// can't be scanned: too many misses and the client gets 429 for a while.
async fn probe_guarded(st: &AppState, ip: std::net::IpAddr, fut: impl std::future::Future<Output = Res>) -> Res {
    if let Some(left) = st.probes.blocked(ip) {
        return Err(too_many(left, "Too many requests for missing files"));
    }
    let res = fut.await;
    if matches!(&res, Err(r) if r.status() == StatusCode::NOT_FOUND) && st.probes.fail(ip).is_err() {
        tracing::warn!(%ip, "too many requests for missing files, client blocked");
    }
    res
}

pub async fn get_file(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    Path((id, filename)): Path<(String, String)>,
    Query(q): Query<GetQuery>,
    req: Request,
) -> Res {
    let ip = client_ip(peer, req.headers(), st.cfg.trust_proxy);
    probe_guarded(&st, ip, get_file_inner(&st, id, filename, q, req)).await
}

async fn get_file_inner(st: &AppState, id: String, filename: String, q: GetQuery, mut req: Request) -> Res {
    let st = st.clone();
    let headers = req.headers().clone();
    let method = req.method().clone();

    // Inline image preview: allowed only when downloads aren't limited,
    // otherwise previews would bypass the limit.
    if q.inline.is_some() {
        let f = st.db.get_active(&id, &filename).map_err(internal)?.ok_or_else(|| not_found(&headers))?;
        if !previewable(&f.content_type) || f.max_downloads.is_some() {
            return Err(not_found(&headers));
        }
        return serve(&st, &f, "inline", req).await;
    }

    // Browsers get an info page; curl/wget get the raw file.
    if q.dl.is_none() && wants_html(&headers) {
        let f = st.db.get_active(&id, &filename).map_err(internal)?.ok_or_else(|| not_found(&headers))?;
        // Text preview reveals the content, so like image previews it's only
        // offered when downloads aren't limited.
        let text = if f.max_downloads.is_none() && !previewable(&f.content_type) {
            text_preview(&st.file_path(&f.id)).await
        } else {
            None
        };
        return Ok(info_page(&origin(&headers), &f, text));
    }

    // HEAD never counts. Resumed downloads (a Range not starting at byte 0) don't
    // count either, except for files with a download limit: those are always
    // served whole and every GET counts, or ranges could read them for free.
    let resuming = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|r| !r.trim().starts_with("bytes=0-"));
    let current = st.db.get_active(&id, &filename).map_err(internal)?.ok_or_else(|| not_found(&headers))?;
    let f = if method == Method::HEAD {
        current
    } else if current.max_downloads.is_some() {
        req.headers_mut().remove(header::RANGE);
        req.headers_mut().remove(header::IF_RANGE);
        st.db.take_download(&id, &filename).map_err(internal)?.ok_or_else(|| not_found(&headers))?
    } else if resuming {
        current
    } else {
        st.db.take_download(&id, &filename).map_err(internal)?.ok_or_else(|| not_found(&headers))?
    };

    let resp = serve(&st, &f, "attachment", req).await?;
    if f.max_downloads.is_some_and(|m| f.downloads >= m) {
        // Last allowed download: the open handle keeps streaming after unlink.
        let _ = st.db.delete(&f.id);
        let _ = tokio::fs::remove_file(st.file_path(&f.id)).await;
        tracing::info!(id = %f.id, "download limit reached, deleted");
    }
    Ok(resp)
}

async fn serve(st: &AppState, f: &FileRow, disposition: &str, req: Request) -> Res {
    let mut resp = ServeFile::new_with_mime(st.file_path(&f.id), &f.content_type.parse().unwrap())
        .oneshot(req)
        .await
        .map_err(internal)?
        .map(Body::new);
    if resp.status() == StatusCode::NOT_FOUND {
        return Err(err(StatusCode::NOT_FOUND, "Not found"));
    }
    let h = resp.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&content_disposition(disposition, &f.filename)) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    h.remove(header::LAST_MODIFIED);
    Ok(resp)
}

/// How much of a text file the info page shows.
const TEXT_PREVIEW_BYTES: usize = 64 * 1024;
const TEXT_PREVIEW_LINES: usize = 1000;

pub struct TextPreview {
    text: String,
    truncated: bool,
}

/// Reads the start of a file and returns it if it looks like text: valid UTF-8
/// (a character cut at the end is fine) and no control characters besides
/// tab, newline, carriage return and form feed.
async fn text_preview(path: &std::path::Path) -> Option<TextPreview> {
    use tokio::io::AsyncReadExt;
    let file = tokio::fs::File::open(path).await.ok()?;
    let len = file.metadata().await.ok()?.len();
    let mut buf = Vec::with_capacity(TEXT_PREVIEW_BYTES.min(len as usize));
    file.take(TEXT_PREVIEW_BYTES as u64).read_to_end(&mut buf).await.ok()?;
    let mut truncated = (buf.len() as u64) < len;
    text_from_bytes(&buf, truncated).map(|mut text| {
        if let Some((cut, _)) = text.match_indices('\n').nth(TEXT_PREVIEW_LINES - 1) {
            text.truncate(cut);
            truncated = true;
        }
        TextPreview { text, truncated }
    })
}

fn text_from_bytes(buf: &[u8], truncated: bool) -> Option<String> {
    if buf.is_empty() {
        return None;
    }
    let text = match std::str::from_utf8(buf) {
        Ok(t) => t,
        // A multi-byte character split by the size cap: keep what's before it.
        Err(e) if truncated && e.error_len().is_none() => std::str::from_utf8(&buf[..e.valid_up_to()]).ok()?,
        Err(_) => return None,
    };
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    if text.chars().any(|c| c.is_control() && !matches!(c, '\t' | '\n' | '\r' | '\u{0C}')) {
        return None;
    }
    Some(text.replace("\r\n", "\n"))
}

/// Language hint for the syntax highlighter: the extension, or a well-known file name.
fn lang_hint(filename: &str) -> String {
    let lower = filename.to_ascii_lowercase();
    let hint = match lower.as_str() {
        "dockerfile" | "containerfile" => "dockerfile",
        "makefile" | "gnumakefile" => "makefile",
        "cmakelists.txt" => "cmake",
        "nginx.conf" => "nginx",
        "bashrc" | "bash_profile" | "zshrc" | "profile" => "bash",
        _ if lower.starts_with("dockerfile.") => "dockerfile",
        _ => lower.rsplit_once('.').map(|(_, e)| e).unwrap_or(""),
    };
    hint.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '+' || *c == '-').take(16).collect()
}

fn text_preview_html(p: &TextPreview, filename: &str) -> String {
    let body = p.text.strip_suffix('\n').unwrap_or(&p.text);
    let count = body.split('\n').count();
    let mut gutter = String::with_capacity(count * 5);
    for n in 1..=count {
        gutter.push_str(&n.to_string());
        gutter.push('\n');
    }
    let note = if p.truncated {
        r#"<div class="text-more">Preview shows the start of the file. Download it to see everything.</div>"#
    } else {
        ""
    };
    format!(
        r#"<div class="preview-box preview-text" role="region" aria-label="Preview of {}" tabindex="0"><pre class="gutter" aria-hidden="true">{gutter}</pre><pre class="src"><code data-lang="{}">{}</code></pre></div>{note}"#,
        html_escape(filename),
        html_escape(&lang_hint(filename)),
        html_escape(body)
    )
}

fn info_page(origin: &str, f: &FileRow, text: Option<TextPreview>) -> Response {
    let url = file_url(origin, f);
    let extension = f
        .filename
        .rsplit_once('.')
        .map(|(_, e)| e.chars().take(5).collect::<String>().to_uppercase())
        .filter(|e| !e.is_empty());
    let preview = if previewable(&f.content_type) && f.max_downloads.is_none() {
        format!(
            r#"<div class="preview-box"><img src="{}?inline=1" alt="Preview of {}"></div>"#,
            html_escape(&url),
            html_escape(&f.filename)
        )
    } else if let Some(t) = &text {
        text_preview_html(t, &f.filename)
    } else if let Some(left) = f.downloads_left() {
        // Limited downloads: no preview, and tell the recipient what that means.
        let (count, after) = if left == 1 {
            ("This file can be downloaded once.".to_string(), "Then the link stops working and the file is deleted.")
        } else {
            (format!("This file can be downloaded {left} more times."), "After that the link stops working and the file is deleted.")
        };
        format!(
            r#"<div class="limit-note" role="note"><span class="limit-icon"><svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path d="M12 3c1 4 5 5.5 5 10a5 5 0 01-10 0c0-2.2 1-3.6 2.2-4.6.2 1.6 1 2.6 2.1 2.9C11.2 8.6 11 6 12 3z"/></svg><b>{}</b></span><span class="limit-text"><strong>{}</strong> {} There's no preview, so opening this page doesn't use a download.</span></div>"#,
            html_escape(extension.as_deref().unwrap_or("FILE")),
            count,
            after
        )
    } else {
        format!(
            r#"<div class="preview-box"><div class="file-icon"><svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path d="M14 3H7a2 2 0 00-2 2v14a2 2 0 002 2h10a2 2 0 002-2V8z"/><path d="M14 3v5h5"/></svg><span>{}</span></div></div>"#,
            html_escape(extension.as_deref().unwrap_or("FILE"))
        )
    };
    let kind = match f.content_type.strip_prefix("image/") {
        Some(sub) if previewable(&f.content_type) => format!("{} image", sub.to_uppercase()),
        _ => match &extension {
            Some(e) => format!("{e} file"),
            None if text.is_some() => "Text file".into(),
            None => "File".into(),
        },
    };
    let downloads = match f.downloads_left() {
        Some(n) => format!("{n} download{} left", if n == 1 { "" } else { "s" }),
        None => "Unlimited downloads".into(),
    };
    let html = render(FILE_HTML, |key| match key {
        "filename" => html_escape(&f.filename),
        "size" => human_size(f.size as u64),
        "url" => html_escape(&url),
        "expires_at" => f.expires_at.to_string(),
        "downloads" => downloads.clone(),
        "preview" => preview.clone(),
        "id" => html_escape(&f.id),
        "kind" => html_escape(&kind),
        "version" => html_escape(version()),
        "scripts" if text.is_some() => format!(r#"<script src="/static/{HIGHLIGHT_JS_NAME}" defer></script>"#),
        _ => String::new(),
    });
    Html(html).into_response()
}

/// Single-pass `{{key}}` substitution, so user data is never re-scanned for placeholders.
fn render(tpl: &str, value: impl Fn(&str) -> String) -> String {
    let mut out = String::with_capacity(tpl.len() + 256);
    let mut rest = tpl;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find("}}") else { break };
        out.push_str(&value(&rest[start + 2..start + end]));
        rest = &rest[start + end + 2..];
    }
    out.push_str(rest);
    out
}

// ---------- qr / delete ----------

pub async fn qr(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    Path((id, filename, action)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Res {
    let ip = client_ip(peer, &headers, st.cfg.trust_proxy);
    probe_guarded(&st, ip, qr_inner(&st, id, filename, action, &headers)).await
}

async fn qr_inner(st: &AppState, id: String, filename: String, action: String, headers: &HeaderMap) -> Res {
    if action != "qr.svg" {
        // Someone opened the delete link in a browser: explain instead of a bare 404.
        if st.db.is_delete_link(&id, &filename, &sha256_hex(&action)).map_err(internal)? {
            let url = format!("{}/{}/{}/{}", origin(headers), id, encode_segment(&filename), action);
            let mut r = err(
                StatusCode::METHOD_NOT_ALLOWED,
                &format!("This is the delete link. Opening it does not delete the file.\nTo delete it, run:\n\n  curl -X DELETE {url}"),
            );
            r.headers_mut().insert(header::ALLOW, HeaderValue::from_static("DELETE"));
            return Ok(r);
        }
        return Err(err(StatusCode::NOT_FOUND, "Not found"));
    }
    let f = st.db.get_active(&id, &filename).map_err(internal)?.ok_or_else(|| not_found(headers))?;
    let code = qrcode::QrCode::new(file_url(&origin(headers), &f).as_bytes()).map_err(internal)?;
    let svg = code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(200, 200)
        .quiet_zone(true)
        .build();
    Ok(([(header::CONTENT_TYPE, "image/svg+xml")], svg).into_response())
}

pub async fn delete(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    Path((id, filename, token)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Res {
    let ip = client_ip(peer, &headers, st.cfg.trust_proxy);
    probe_guarded(&st, ip, async {
        if st.db.delete_with_hash(&id, &filename, &sha256_hex(&token)).map_err(internal)? {
            let _ = tokio::fs::remove_file(st.file_path(&id)).await;
            tracing::info!(%id, "deleted by owner");
            Ok(err(StatusCode::OK, "Deleted"))
        } else {
            Err(err(StatusCode::NOT_FOUND, "Not found"))
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_text() {
        assert_eq!(text_from_bytes(b"#!/bin/sh\r\necho hi\n", false).as_deref(), Some("#!/bin/sh\necho hi\n"));
        assert_eq!(text_from_bytes("xin chào\tbạn".as_bytes(), false).as_deref(), Some("xin chào\tbạn"));
        assert!(text_from_bytes(b"PK\x03\x04\x00\x00", false).is_none(), "binary");
        assert!(text_from_bytes(b"abc\x1b[31mred", false).is_none(), "escape codes");
        assert!(text_from_bytes(b"\xff\xfe", false).is_none(), "not utf-8");
        assert!(text_from_bytes(b"", false).is_none());
        // a 2-byte character cut in half by the size cap
        assert_eq!(text_from_bytes(&"ab\u{e0}".as_bytes()[..3], true).as_deref(), Some("ab"));
        assert!(text_from_bytes(&"ab\u{e0}".as_bytes()[..3], false).is_none());
    }

    #[test]
    fn text_preview_is_escaped() {
        let p = TextPreview { text: "<script>alert(1)</script>\n{{url}}\n".into(), truncated: false };
        let html = text_preview_html(&p, "x.html");
        assert!(!html.contains("<script>"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("{{url}}"), "user text is inserted as-is, never re-rendered");
        assert!(html.contains("<pre class=\"gutter\" aria-hidden=\"true\">1\n2\n</pre>"));
    }

    #[test]
    fn lang_hints() {
        assert_eq!(lang_hint("main.PY"), "py");
        assert_eq!(lang_hint("Dockerfile"), "dockerfile");
        assert_eq!(lang_hint("CMakeLists.txt"), "cmake");
        assert_eq!(lang_hint("notes"), "");
        assert_eq!(lang_hint("x.c++"), "c++");
        assert_eq!(lang_hint("a.\"><b>"), "b");
    }
}
