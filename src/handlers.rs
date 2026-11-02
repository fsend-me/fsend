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

pub async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
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
        return Ok(info_page(&origin(&headers), &f));
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

fn info_page(origin: &str, f: &FileRow) -> Response {
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
