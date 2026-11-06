//! Abuse limits: per-client lockouts (wrong tokens, probing for links) and a
//! server-wide cap on wrong tokens, so short tokens and short ids can't be
//! brute-forced, not even from many addresses.

use axum::http::HeaderMap;
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv6Addr, SocketAddr},
    sync::Mutex,
    time::{Duration, Instant},
};

const MAX_TRACKED: usize = 100_000;
const TRUSTED_FOR: Duration = Duration::from_secs(24 * 3600);

/// The unit we rate limit: IPv4 addresses as-is, IPv6 by /64 prefix (one
/// subscriber usually owns a whole /64, so per-address limits are trivially dodged).
pub fn client_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => {
                let s = v6.segments();
                IpAddr::V6(Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
            }
        },
    }
}

/// Counts failures per client in a window; `max` failures lock the client for `window`.
pub struct Limiter {
    max: u32,
    window: Duration,
    entries: Mutex<HashMap<IpAddr, Entry>>,
}

struct Entry {
    fails: u32,
    since: Instant,
    locked_until: Option<Instant>,
}

impl Limiter {
    /// `max == 0` disables the limiter.
    pub fn new(max: u32, window: Duration) -> Self {
        Self { max, window, entries: Mutex::new(HashMap::new()) }
    }

    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<IpAddr, Entry>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Remaining lockout if this client is currently blocked.
    pub fn blocked(&self, ip: IpAddr) -> Option<Duration> {
        if self.max == 0 {
            return None;
        }
        let now = Instant::now();
        self.map()
            .get(&client_key(ip))
            .and_then(|e| e.locked_until)
            .and_then(|until| until.checked_duration_since(now))
    }

    /// Records a failure. `Ok(tries left)`, or `Err(lockout)` if this one triggered a lockout.
    pub fn fail(&self, ip: IpAddr) -> Result<u32, Duration> {
        if self.max == 0 {
            return Ok(u32::MAX);
        }
        let key = client_key(ip);
        let now = Instant::now();
        let mut map = self.map();
        if map.len() >= MAX_TRACKED && !map.contains_key(&key) {
            Self::prune(&mut map, now, self.window);
        }
        let e = map.entry(key).or_insert(Entry { fails: 0, since: now, locked_until: None });
        if now.duration_since(e.since) > self.window {
            *e = Entry { fails: 0, since: now, locked_until: None };
        }
        e.fails += 1;
        if e.fails >= self.max {
            e.locked_until = Some(now + self.window);
            return Err(self.window);
        }
        Ok(self.max - e.fails)
    }

    pub fn reset(&self, ip: IpAddr) {
        self.map().remove(&client_key(ip));
    }

    /// Drops entries that are neither locked nor inside the counting window.
    pub fn sweep(&self) {
        let mut map = self.map();
        Self::prune(&mut map, Instant::now(), self.window);
    }

    fn prune(map: &mut HashMap<IpAddr, Entry>, now: Instant, window: Duration) {
        map.retain(|_, e| e.locked_until.is_some_and(|u| u > now) || now.duration_since(e.since) <= window);
    }
}

/// Wrong-token protection: a per-client limiter plus a server-wide budget.
/// When the budget runs out, uploads pause for everyone except clients that
/// uploaded successfully in the last 24 hours, so a distributed guessing
/// attack stops without locking out regular users.
pub struct AuthGuard {
    per_client: Limiter,
    global_max: u32,
    window: Duration,
    global: Mutex<GlobalState>,
    trusted: Mutex<HashMap<IpAddr, Instant>>,
}

struct GlobalState {
    fails: u32,
    since: Instant,
    locked_until: Option<Instant>,
}

impl AuthGuard {
    pub fn new(max_per_client: u32, global_max: u32, window: Duration) -> Self {
        Self {
            per_client: Limiter::new(max_per_client, window),
            global_max,
            window,
            global: Mutex::new(GlobalState { fails: 0, since: Instant::now(), locked_until: None }),
            trusted: Mutex::new(HashMap::new()),
        }
    }

    fn is_trusted(&self, ip: IpAddr) -> bool {
        let trusted = self.trusted.lock().unwrap_or_else(|e| e.into_inner());
        trusted.get(&client_key(ip)).is_some_and(|t| t.elapsed() < TRUSTED_FOR)
    }

    fn global_lock_left(&self) -> Option<Duration> {
        let g = self.global.lock().unwrap_or_else(|e| e.into_inner());
        g.locked_until.and_then(|u| u.checked_duration_since(Instant::now()))
    }

    /// Remaining lockout if uploads from this client are blocked right now.
    pub fn blocked(&self, ip: IpAddr) -> Option<Duration> {
        if let Some(left) = self.per_client.blocked(ip) {
            return Some(left);
        }
        self.global_lock_left().filter(|_| !self.is_trusted(ip))
    }

    /// True while the server-wide budget is exhausted (for logging).
    pub fn globally_locked(&self) -> bool {
        self.global_lock_left().is_some()
    }

    /// Records a wrong token. `Ok(tries left)` or `Err(lockout)`.
    pub fn fail(&self, ip: IpAddr) -> Result<u32, Duration> {
        if self.global_max > 0 {
            let now = Instant::now();
            let mut g = self.global.lock().unwrap_or_else(|e| e.into_inner());
            if now.duration_since(g.since) > self.window {
                *g = GlobalState { fails: 0, since: now, locked_until: g.locked_until };
            }
            g.fails += 1;
            if g.fails >= self.global_max {
                g.locked_until = Some(now + self.window);
                g.fails = 0;
                g.since = now;
            }
        }
        let result = self.per_client.fail(ip);
        if result.is_ok() {
            if let Some(left) = self.global_lock_left().filter(|_| !self.is_trusted(ip)) {
                return Err(left);
            }
        }
        result
    }

    /// A correct token: clear this client's failures and trust it for a day.
    pub fn success(&self, ip: IpAddr) {
        self.per_client.reset(ip);
        let mut trusted = self.trusted.lock().unwrap_or_else(|e| e.into_inner());
        if trusted.len() >= 10_000 {
            trusted.retain(|_, t| t.elapsed() < TRUSTED_FOR);
        }
        if trusted.len() < 10_000 {
            trusted.insert(client_key(ip), Instant::now());
        }
    }

    pub fn sweep(&self) {
        self.per_client.sweep();
        self.trusted.lock().unwrap_or_else(|e| e.into_inner()).retain(|_, t| t.elapsed() < TRUSTED_FOR);
    }
}

/// Behind a proxy without FSEND_TRUST_PROXY every client shares the proxy's IP,
/// so one client's lockout blocks everyone. Say so in the log, once.
fn warn_proxy_once(peer: SocketAddr) {
    static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        tracing::warn!(
            proxy = %peer.ip(),
            "requests arrive through a reverse proxy (X-Forwarded-For is set) but FSEND_TRUST_PROXY is off: \
             all clients share the proxy's IP, so rate limits and lockouts hit everyone at once. \
             Set FSEND_TRUST_PROXY=true."
        );
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
    } else if headers.contains_key("x-forwarded-for") || headers.contains_key("x-real-ip") {
        warn_proxy_once(peer);
    }
    peer.ip()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn locks_after_max_fails_and_resets() {
        let l = Limiter::new(3, Duration::from_secs(60));
        assert_eq!(l.fail(ip("1.2.3.4")), Ok(2));
        assert_eq!(l.fail(ip("1.2.3.4")), Ok(1));
        assert!(l.blocked(ip("1.2.3.4")).is_none());
        assert!(l.fail(ip("1.2.3.4")).is_err());
        assert!(l.blocked(ip("1.2.3.4")).is_some());
        assert!(l.blocked(ip("5.6.7.8")).is_none());

        let l = Limiter::new(3, Duration::from_secs(60));
        let _ = l.fail(ip("1.2.3.4"));
        let _ = l.fail(ip("1.2.3.4"));
        l.reset(ip("1.2.3.4"));
        assert_eq!(l.fail(ip("1.2.3.4")), Ok(2));

        let off = Limiter::new(0, Duration::from_secs(60));
        for _ in 0..100 {
            assert!(off.fail(ip("1.2.3.4")).is_ok());
        }
        assert!(off.blocked(ip("1.2.3.4")).is_none());
    }

    #[test]
    fn ipv6_is_limited_per_64() {
        let l = Limiter::new(2, Duration::from_secs(60));
        let _ = l.fail(ip("2001:db8:1:2::1"));
        assert!(l.fail(ip("2001:db8:1:2:ffff::9")).is_err(), "same /64 shares one budget");
        assert!(l.blocked(ip("2001:db8:1:2::abcd")).is_some());
        assert!(l.blocked(ip("2001:db8:1:3::1")).is_none(), "other /64 unaffected");
        assert_eq!(client_key(ip("::ffff:1.2.3.4")), ip("1.2.3.4"));
    }

    #[test]
    fn global_budget_pauses_untrusted_clients_only() {
        let g = AuthGuard::new(5, 3, Duration::from_secs(60));
        g.success(ip("10.0.0.1")); // a regular user
        assert!(g.fail(ip("1.1.1.1")).is_ok());
        assert!(g.fail(ip("2.2.2.2")).is_ok());
        assert!(g.fail(ip("3.3.3.3")).is_err(), "third failure server-wide trips the budget");
        assert!(g.globally_locked());
        assert!(g.blocked(ip("4.4.4.4")).is_some(), "new clients are paused");
        assert!(g.blocked(ip("10.0.0.1")).is_none(), "recently successful clients keep working");
    }

    #[test]
    fn proxy_ip() {
        let peer: SocketAddr = "10.0.0.1:5000".parse().unwrap();
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", HeaderValue::from_static("6.6.6.6, 9.9.9.9"));
        assert_eq!(client_ip(peer, &h, false), ip("10.0.0.1"));
        assert_eq!(client_ip(peer, &h, true), ip("9.9.9.9"));
    }
}
