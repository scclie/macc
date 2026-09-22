// plain http/1.1, connection: close. enough for a door handle.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;

pub const MAX_REQ: usize = 1 << 16;

pub struct Req {
    pub method: String,
    pub path: String,
    pub body: String,
    pub headers: HashMap<String, String>,
}

pub fn read_request(stream: &mut TcpStream) -> std::io::Result<Req> {
    let mut buf = [0u8; MAX_REQ];
    let mut n = 0usize;
    loop {
        let got = stream.read(&mut buf[n..])?;
        if got == 0 {
            break;
        }
        n += got;
        if n >= MAX_REQ {
            return Err(std::io::Error::other("request too large"));
        }
        if let Some(head_end) = find(&buf[..n], b"\r\n\r\n") {
            let head_len = head_end + 4;
            let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
            let mut lines = head.lines();
            let reqline = lines.next().unwrap_or_default().to_string();
            let mut parts = reqline.split_whitespace();
            let method = parts.next().unwrap_or_default().to_string();
            let path = parts.next().unwrap_or_default().to_string();
            let mut headers = HashMap::new();
            for l in lines {
                if let Some((k, v)) = l.split_once(':') {
                    headers.insert(k.trim().to_lowercase(), v.trim().to_string());
                }
            }
            let cl: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
            if n >= head_len + cl {
                let body = String::from_utf8_lossy(&buf[head_len..head_len + cl]).to_string();
                return Ok(Req { method, path, body, headers });
            }
        }
    }
    Err(std::io::Error::other("incomplete request"))
}

pub fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

pub fn respond(stream: &mut TcpStream, status: &str, ctype: &str, body: &str, extra: &str) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\nReferrer-Policy: no-referrer\r\nCache-Control: no-store\r\n{extra}\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    stream.flush()
}

pub fn form_value(body: &str, key: &str) -> Option<String> {
    for pair in body.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == key {
                return Some(url_decode(v));
            }
        }
    }
    None
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

pub fn random_token() -> String {
    let mut b = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut b);
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}