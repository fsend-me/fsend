mod cleanup;
mod config;
mod db;
mod handlers;
mod limiter;
mod util;

use axum::{
    extract::DefaultBodyLimit,
    http::{header, HeaderValue},
    routing::{get, put},
    Router,
};
use std::{net::SocketAddr, path::PathBuf, sync::Arc};
use tower_http::{set_header::SetResponseHeaderLayer, trace::TraceLayer};

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<config::Config>,
    pub db: Arc<db::Db>,
    pub auth: Arc<limiter::AuthGuard>,
    pub probes: Arc<limiter::Limiter>,
}

impl AppState {
    pub fn file_path(&self, id: &str) -> PathBuf {
        self.cfg.data_dir.join("files").join(id)
    }
}

pub fn router(st: AppState) -> Router {
    let sec = |name: &'static str, value: &'static str| {
        SetResponseHeaderLayer::if_not_present(header::HeaderName::from_static(name), HeaderValue::from_static(value))
    };
    Router::new()
        .route("/", get(handlers::index))
        .route("/healthz", get(handlers::healthz))
        .route("/install.sh", get(handlers::install_sh))
        .route("/api/config", get(handlers::config))
        .route("/static/{name}", get(handlers::asset))
        .route("/static/fonts/{name}", get(handlers::font))
        .route("/{filename}", put(handlers::upload).layer(DefaultBodyLimit::disable()))
        .route("/{id}/{filename}", get(handlers::get_file))
        .route("/{id}/{filename}/{action}", get(handlers::qr).delete(handlers::delete))
        .layer(sec("x-content-type-options", "nosniff"))
        .layer(sec("x-frame-options", "DENY"))
        .layer(sec("referrer-policy", "no-referrer"))
        .layer(sec(
            "content-security-policy",
            "default-src 'self'; img-src 'self' data: blob:; style-src 'self'; script-src 'self'; font-src 'self'; \
             object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'",
        ))
        .layer(TraceLayer::new_for_http())
        .with_state(st)
}

/// Creates the data folders and proves we can write there, with an actionable error if not.
fn check_data_dir(dir: &std::path::Path) -> Result<(), String> {
    let probe = dir.join("tmp").join(".write-test");
    let result = ["files", "tmp"]
        .iter()
        .try_for_each(|d| std::fs::create_dir_all(dir.join(d)))
        .and_then(|_| std::fs::write(&probe, b"ok"))
        .and_then(|_| std::fs::remove_file(&probe));
    let Err(e) = result else { return Ok(()) };
    let mut msg = format!("error: cannot write to data directory {}: {e}", dir.display());
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        let (uid, gid) = current_ids();
        msg.push_str(&format!(
            "\n\nfsend runs as UID:GID {uid}:{gid}, which cannot write to this folder.\n\
             If it is a bind mount (e.g. ./data:/data), fix it on the host with either:\n\
             \n  sudo chown -R {uid}:{gid} ./data\n\
             \nor run the container as the folder's owner, e.g. in docker-compose.yml:\n\
             \n  user: \"1000:1000\"   # values from `id -u` and `id -g` on the host\n"
        ));
    }
    Err(msg)
}

/// Effective UID/GID from /proc (Linux); "?" elsewhere.
fn current_ids() -> (String, String) {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let field = |name: &str| {
        status
            .lines()
            .find_map(|l| l.strip_prefix(name))
            .and_then(|v| v.split_whitespace().nth(1))
            .unwrap_or("?")
            .to_string()
    };
    (field("Uid:"), field("Gid:"))
}

/// `fsend --healthcheck`: lets the distroless container check itself without curl.
fn healthcheck(listen: &str) -> std::process::ExitCode {
    use std::io::{Read, Write};
    let port = listen.rsplit(':').next().unwrap_or("8080");
    let ok = (|| -> std::io::Result<bool> {
        let mut s = std::net::TcpStream::connect(format!("127.0.0.1:{port}"))?;
        s.set_read_timeout(Some(std::time::Duration::from_secs(3)))?;
        s.write_all(b"GET /healthz HTTP/1.0\r\nHost: localhost\r\n\r\n")?;
        let mut buf = String::new();
        s.read_to_string(&mut buf)?;
        Ok(buf.starts_with("HTTP/1.0 200") || buf.starts_with("HTTP/1.1 200"))
    })()
    .unwrap_or(false);
    if ok { std::process::ExitCode::SUCCESS } else { std::process::ExitCode::FAILURE }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "fsend=info,tower_http=warn".into()),
        )
        .init();

    let cfg = match config::Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("config error: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    if std::env::args().any(|a| a == "--healthcheck") {
        return healthcheck(&cfg.listen);
    }

    if let Err(msg) = check_data_dir(&cfg.data_dir) {
        eprintln!("{msg}");
        return std::process::ExitCode::FAILURE;
    }
    let db = match db::Db::open(&cfg.data_dir.join("fsend.db")) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("error: cannot open database {}: {e}", cfg.data_dir.join("fsend.db").display());
            return std::process::ExitCode::FAILURE;
        }
    };
    if cfg.auth_token.is_none() {
        tracing::warn!("FSEND_TOKEN is not set: anyone can upload!");
    }
    tracing::info!(
        listen = %cfg.listen,
        id_length = cfg.id_length,
        max_upload = %util::human_size(cfg.max_upload_size),
        default_days = cfg.default_days,
        max_days = cfg.max_days,
        trust_proxy = cfg.trust_proxy,
        "starting fsend"
    );

    let auth = limiter::AuthGuard::new(cfg.auth_max_fails, cfg.auth_global_max_fails, cfg.auth_lockout);
    let probes = limiter::Limiter::new(cfg.not_found_max, handlers::PROBE_WINDOW);
    let st = AppState { cfg: Arc::new(cfg), db: Arc::new(db), auth: Arc::new(auth), probes: Arc::new(probes) };
    tokio::spawn(cleanup::run(st.clone()));

    let listener = tokio::net::TcpListener::bind(&st.cfg.listen).await.expect("cannot bind");
    axum::serve(listener, router(st).into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async {
            let ctrl_c = tokio::signal::ctrl_c();
            let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
            tokio::select! { _ = ctrl_c => {}, _ = term.recv() => {} }
        })
        .await
        .unwrap();
    std::process::ExitCode::SUCCESS
}
