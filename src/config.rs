use std::{env, path::PathBuf, time::Duration};

#[derive(Clone, Debug)]
pub struct Config {
    pub listen: String,
    pub data_dir: PathBuf,
    pub auth_token: Option<String>,
    pub id_length: usize,
    pub max_upload_size: u64,
    pub default_days: f64,
    pub max_days: f64,
    pub cleanup_interval: Duration,
    pub trust_proxy: bool,
    pub auth_max_fails: u32,
    pub auth_lockout: Duration,
    pub auth_global_max_fails: u32,
    pub not_found_max: u32,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let cfg = Config {
            listen: var("FSEND_LISTEN").unwrap_or_else(|| "0.0.0.0:8080".into()),
            data_dir: var("FSEND_DATA_DIR").unwrap_or_else(|| "./data".into()).into(),
            auth_token: var("FSEND_TOKEN"),
            id_length: parse("FSEND_ID_LENGTH", 3)?,
            max_upload_size: match var("FSEND_MAX_UPLOAD_SIZE") {
                Some(v) => parse_size(&v).ok_or(format!("invalid FSEND_MAX_UPLOAD_SIZE: {v}"))?,
                None => 10 * 1024 * 1024 * 1024,
            },
            default_days: parse("FSEND_DEFAULT_DAYS", 3.0)?,
            max_days: parse("FSEND_MAX_DAYS", 30.0)?,
            cleanup_interval: Duration::from_secs(parse("FSEND_CLEANUP_INTERVAL", 600)?),
            trust_proxy: parse("FSEND_TRUST_PROXY", false)?,
            auth_max_fails: parse("FSEND_AUTH_MAX_FAILS", 5)?,
            auth_lockout: Duration::from_secs(60 * parse::<u64>("FSEND_AUTH_LOCKOUT_MINUTES", 15)?),
            auth_global_max_fails: parse("FSEND_AUTH_GLOBAL_MAX_FAILS", 100)?,
            not_found_max: parse("FSEND_NOT_FOUND_MAX", 30)?,
        };
        if !(1..=32).contains(&cfg.id_length) {
            return Err("FSEND_ID_LENGTH must be between 1 and 32".into());
        }
        if cfg.default_days <= 0.0 || cfg.max_days < cfg.default_days {
            return Err("require 0 < FSEND_DEFAULT_DAYS <= FSEND_MAX_DAYS".into());
        }
        if cfg.auth_max_fails == 0 || cfg.auth_lockout.is_zero() {
            return Err("FSEND_AUTH_MAX_FAILS and FSEND_AUTH_LOCKOUT_MINUTES must be > 0".into());
        }
        if cfg.cleanup_interval.is_zero() {
            return Err("FSEND_CLEANUP_INTERVAL must be > 0".into());
        }
        Ok(cfg)
    }
}

fn var(key: &str) -> Option<String> {
    env::var(key).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn parse<T: std::str::FromStr>(key: &str, default: T) -> Result<T, String> {
    match var(key) {
        Some(v) => v.parse().map_err(|_| format!("invalid {key}: {v}")),
        None => Ok(default),
    }
}

/// Parses sizes like `10G`, `10GB`, `500M`, `1024`.
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim().to_ascii_uppercase();
    let s = s.strip_suffix("IB").or_else(|| s.strip_suffix('B')).unwrap_or(&s);
    let (num, mult) = match s.chars().last()? {
        'K' => (&s[..s.len() - 1], 1u64 << 10),
        'M' => (&s[..s.len() - 1], 1 << 20),
        'G' => (&s[..s.len() - 1], 1 << 30),
        'T' => (&s[..s.len() - 1], 1 << 40),
        _ => (s, 1),
    };
    let n: f64 = num.trim().parse().ok()?;
    (n > 0.0).then(|| (n * mult as f64) as u64)
}

#[cfg(test)]
mod tests {
    use super::parse_size;

    #[test]
    fn sizes() {
        assert_eq!(parse_size("10G"), Some(10 << 30));
        assert_eq!(parse_size("10GB"), Some(10 << 30));
        assert_eq!(parse_size("1.5k"), Some(1536));
        assert_eq!(parse_size("2048"), Some(2048));
        assert_eq!(parse_size("abc"), None);
        assert_eq!(parse_size("0"), None);
    }
}
