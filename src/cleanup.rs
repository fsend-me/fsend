use crate::AppState;
use std::time::{Duration, SystemTime};

/// Periodically deletes expired files and stray data.
pub async fn run(st: AppState) {
    let mut tick = tokio::time::interval(st.cfg.cleanup_interval);
    loop {
        tick.tick().await;
        if let Err(e) = sweep(&st).await {
            tracing::error!("cleanup failed: {e}");
        }
    }
}

pub async fn sweep(st: &AppState) -> Result<(), Box<dyn std::error::Error>> {
    st.limiter.sweep();
    let dead = st.db.purge_dead()?;
    for id in &dead {
        let _ = tokio::fs::remove_file(st.file_path(id)).await;
    }
    if !dead.is_empty() {
        tracing::info!(count = dead.len(), "removed expired files");
    }

    // Files on disk without a DB row (e.g. crash mid-upload), older than 1h.
    remove_stale(st, "files", Duration::from_secs(3600), true).await?;
    // Abandoned temp uploads, older than 1 day.
    remove_stale(st, "tmp", Duration::from_secs(86400), false).await?;
    Ok(())
}

async fn remove_stale(st: &AppState, dir: &str, age: Duration, check_db: bool) -> std::io::Result<()> {
    let mut rd = tokio::fs::read_dir(st.cfg.data_dir.join(dir)).await?;
    while let Some(entry) = rd.next_entry().await? {
        let meta = entry.metadata().await?;
        let old = meta
            .modified()
            .ok()
            .and_then(|m| SystemTime::now().duration_since(m).ok())
            .is_some_and(|d| d > age);
        if !old {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if check_db && st.db.exists(&name).unwrap_or(true) {
            continue;
        }
        let _ = tokio::fs::remove_file(entry.path()).await;
        tracing::info!(file = %name, dir, "removed stale file");
    }
    Ok(())
}
