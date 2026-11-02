use axum::http::{header, HeaderMap};
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use rand::RngExt;
use sha2::{Digest, Sha256};

const BASE62: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";

pub fn random_id(len: usize) -> String {
    let mut rng = rand::rng();
    (0..len).map(|_| BASE62[rng.random_range(0..BASE62.len())] as char).collect()
}

pub fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    // Compare hashes so length differences don't leak through timing.
    let (a, b) = (Sha256::digest(a), Sha256::digest(b));
    a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[derive(Debug, PartialEq)]
pub enum Auth {
    Ok,
    Missing,
    Invalid,
}

/// Accepts `Authorization: Bearer <token>` (`curl --oauth2-bearer TOKEN`) or HTTP
/// Basic auth with the token as password (`curl -u :TOKEN`).
pub fn check_auth(headers: &HeaderMap, token: Option<&str>) -> Auth {
    let Some(token) = token else { return Auth::Ok };
    let Some(value) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) else {
        return Auth::Missing;
    };
    let provided = if let Some(t) = value.strip_prefix("Bearer ") {
        t.trim().to_string()
    } else if let Some(b) = value.strip_prefix("Basic ") {
        match base64_decode(b.trim()).and_then(|v| String::from_utf8(v).ok()) {
            Some(s) => s.split_once(':').map(|(_, p)| p.to_string()).unwrap_or(s),
            None => return Auth::Invalid,
        }
    } else {
        return Auth::Invalid;
    };
    if provided.is_empty() {
        Auth::Missing
    } else if constant_time_eq(provided.as_bytes(), token.as_bytes()) {
        Auth::Ok
    } else {
        Auth::Invalid
    }
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut buf, mut bits) = (0u32, 0u8);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return None,
        } as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

/// Strips any path components and control characters from a filename.
pub fn sanitize_filename(name: &str) -> Option<String> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base.chars().filter(|c| !c.is_control()).collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim();
    if cleaned.is_empty() {
        return None;
    }
    let mut out = String::new();
    for c in cleaned.chars() {
        if out.len() + c.len_utf8() > 200 {
            break;
        }
        out.push(c);
    }
    Some(out)
}

const PATH_SEGMENT: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'_').remove(b'.').remove(b'~');

pub fn encode_segment(s: &str) -> String {
    utf8_percent_encode(s, PATH_SEGMENT).to_string()
}

pub fn content_disposition(kind: &str, filename: &str) -> String {
    let ascii: String = filename
        .chars()
        .map(|c| if c.is_ascii_graphic() && c != '"' && c != '\\' || c == ' ' { c } else { '_' })
        .collect();
    format!("{kind}; filename=\"{ascii}\"; filename*=UTF-8''{}", encode_segment(filename))
}

/// Builds the public origin from request headers, so any domain works.
pub fn origin(headers: &HeaderMap) -> String {
    let get = |k: &str| {
        headers
            .get(k)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let proto = get("x-forwarded-proto")
        .filter(|p| p == "http" || p == "https")
        .unwrap_or_else(|| "http".into());
    let host = get("x-forwarded-host")
        .or_else(|| get("host"))
        .filter(|h| h.chars().all(|c| c.is_ascii_alphanumeric() || "-.:[]".contains(c)))
        .unwrap_or_else(|| "localhost".into());
    format!("{proto}://{host}")
}

pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

pub fn human_size(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n} B") } else { format!("{v:.1} {}", UNITS[i]) }
}

/// Image types that are safe to render inline (no SVG: it can carry scripts).
pub fn previewable(content_type: &str) -> bool {
    matches!(
        content_type,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/avif" | "image/bmp"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn sanitize() {
        assert_eq!(sanitize_filename("../../etc/passwd").as_deref(), Some("passwd"));
        assert_eq!(sanitize_filename("a\\b\\c.txt").as_deref(), Some("c.txt"));
        assert_eq!(sanitize_filename(".."), None);
        assert_eq!(sanitize_filename(".hidden").as_deref(), Some("hidden"));
        assert_eq!(sanitize_filename("ảnh đẹp.png").as_deref(), Some("ảnh đẹp.png"));
        assert_eq!(sanitize_filename("a\nb").as_deref(), Some("ab"));
    }

    #[test]
    fn auth() {
        let mut h = HeaderMap::new();
        assert_eq!(check_auth(&h, None), Auth::Ok);
        assert_eq!(check_auth(&h, Some("secret")), Auth::Missing);
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer secret"));
        assert_eq!(check_auth(&h, Some("secret")), Auth::Ok);
        assert_eq!(check_auth(&h, Some("other")), Auth::Invalid);
        // curl -u :secret
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Basic OnNlY3JldA=="));
        assert_eq!(check_auth(&h, Some("secret")), Auth::Ok);
        // curl -u : (empty password)
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Basic Og=="));
        assert_eq!(check_auth(&h, Some("secret")), Auth::Missing);
    }

    #[test]
    fn origin_from_headers() {
        let mut h = HeaderMap::new();
        h.insert("host", HeaderValue::from_static("fsend.me"));
        assert_eq!(origin(&h), "http://fsend.me");
        h.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        assert_eq!(origin(&h), "https://fsend.me");
        h.insert("host", HeaderValue::from_static("evil\"<x>"));
        assert_eq!(origin(&h), "https://localhost");
    }
}
