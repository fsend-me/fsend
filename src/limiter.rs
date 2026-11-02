//! Per-IP lockout after repeated wrong upload tokens, so short tokens can't be brute-forced.

use axum::http::HeaderMap;
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::Mutex,
    time::{Duration, Instant},
};

const MAX_TRACKED: usize = 100_000;

pub struct AuthLimiter {
    max_fails: u32,
    window: Duration,
    entries: Mutex<HashMap<IpAddr, Entry>>,
}

struct Entry {
    fails: u32,
    since: Instant,
    locked_until: Option<Instant>,
}

impl AuthLimiter {
    pub fn new(max_fails: u32, window: Duration) -> Self {
        Self { max_fails, window, entries: Mutex::new(HashMap::new()) }
    }

    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<IpAddr, Entry>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Returns the remaining lockout if this IP is currently blocked.
    pub fn blocked(&self, ip: IpAddr) -> Option<Duration> {
        let now = Instant::now();
        self.map()
            .get(&ip)
            .and_then(|e| e.locked_until)
            .and_then(|until| until.checked_duration_since(now))
    }

    /// Records a wrong token. `Ok(attempts left)`, or `Err(lockout)` if this attempt triggered one.
    pub fn fail(&self, ip: IpAddr) -> Result<u32, Duration> {
        let now = Instant::now();
        let mut map = self.map();
        if map.len() >= MAX_TRACKED && !map.contains_key(&ip) {
            Self::prune(&mut map, now, self.window);
        }
        let e = map.entry(ip).or_insert(Entry { fails: 0, since: now, locked_until: None });
        if now.duration_since(e.since) > self.window {
            *e = Entry { fails: 0, since: now, locked_until: None };
        }
        e.fails += 1;
        if e.fails >= self.max_fails {
            e.locked_until = Some(now + self.window);
            return Err(self.window);
        }
        Ok(self.max_fails - e.fails)
    }

    pub fn success(&self, ip: IpAddr) {
        self.map().remove(&ip);
    }

    /// Drops entries that are neither locked nor inside the counting window.
    pub fn sweep(&self) {
        let mut map = self.map();
        Self::prune(&mut map, Instant::now(), self.window);
    }

    fn prune(map: &mut HashMap<IpAddr, Entry>, now: Instant, window: Duration) {
        map.retain(|_, e| {
            e.locked_until.is_some_and(|u| u > now) || now.duration_since(e.since) <= window
        });
    }
}

/// Client IP: the socket peer, or with `trust_proxy` the last `X-Forwarded-For`
/// hop (the one appended by our own proxy; earlier entries can be forged).
pub fn client_ip(peer: SocketAddr, headers: &HeaderMap, trust_proxy: bool) -> IpAddr {
    if trust_proxy {
        let forwarded = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit(',').next())
            .and_then(|v| v.trim().parse().ok())
            .or_else(|| headers.get("x-real-ip").and_then(|v| v.to_str().ok()?.trim().parse().ok()));
        if let Some(ip) = forwarded {
            return ip;
        }
    }
    peer.ip()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn locks_after_max_fails_and_resets_on_success() {
        let l = AuthLimiter::new(3, Duration::from_secs(60));
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        let other: IpAddr = "5.6.7.8".parse().unwrap();
        assert_eq!(l.fail(ip), Ok(2));
        assert_eq!(l.fail(ip), Ok(1));
        assert!(l.blocked(ip).is_none());
        assert!(l.fail(ip).is_err());
        assert!(l.blocked(ip).is_some());
        assert!(l.blocked(other).is_none());

        let l = AuthLimiter::new(3, Duration::from_secs(60));
        let _ = l.fail(ip);
        let _ = l.fail(ip);
        l.success(ip);
        assert_eq!(l.fail(ip), Ok(2));
    }

    #[test]
    fn proxy_ip() {
        let peer: SocketAddr = "10.0.0.1:5000".parse().unwrap();
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", HeaderValue::from_static("6.6.6.6, 9.9.9.9"));
        assert_eq!(client_ip(peer, &h, false), "10.0.0.1".parse::<IpAddr>().unwrap());
        assert_eq!(client_ip(peer, &h, true), "9.9.9.9".parse::<IpAddr>().unwrap());
    }
}
