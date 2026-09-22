// tiny matrix client against the plain client-server api. continuwuity
// has no admin http api, so account management goes through admin
// commands sent into the #admins room.

use crate::config::Cfg;
use crate::http;
use std::io::{Read, Write};
use std::net::TcpStream;

pub fn mxid(cfg: &Cfg, lp: &str) -> String {
    format!("@{lp}:{}", cfg.domain)
}

pub fn resolve_admin_room(cfg: &Cfg) -> Result<String, String> {
    let alias_enc = cfg.admin_room.replace('#', "%23");
    let (status, body) = http_call(cfg, "GET", &format!("/_matrix/client/v3/directory/room/{alias_enc}"), &cfg.admin_token, None);
    if status != 200 {
        return Err(format!("resolve admin room {}: http {status}: {}", cfg.admin_room, trunc(&body, 200)));
    }
    json_str(&body, "room_id").ok_or_else(|| "no room_id in response".to_string())
}

pub fn profile_exists(cfg: &Cfg, target: &str) -> Result<bool, String> {
    let (status, _) = http_call(cfg, "GET", &format!("/_matrix/client/v3/profile/{target}"), &cfg.admin_token, None);
    match status {
        200 => Ok(true),
        404 => Ok(false),
        0 => Err("profile check: connect/fetch failed".into()),
        s => Err(format!("profile check: http {s}")),
    }
}

pub fn run_admin_command(cfg: &Cfg, room: &str, command: &str) -> Result<String, String> {
    // password/localpart are validated to a charset without " or \, so no
    // json escaping needed here
    let payload = format!("{{\"msgtype\":\"m.text\",\"body\":\"{command}\"}}");
    let txn = format!(
        "macc{}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    );
    let (status, b) = http_call(
        cfg,
        "PUT",
        &format!("/_matrix/client/v3/rooms/{room}/send/m.room.message/{txn}"),
        &cfg.admin_token,
        Some(&payload),
    );
    if status != 200 {
        return Err(format!("send admin command: http {status}: {}", trunc(&b, 200)));
    }

    // poll the admin room for a fresh reply from the admin bot
    for _ in 0..12 {
        std::thread::sleep(std::time::Duration::from_millis(800));
        if let Some(reply) = last_admin_message(cfg, room) {
            if is_success(reply.as_str()) {
                return Ok(reply);
            }
            return Err(reply);
        }
    }
    Err("no admin reply within timeout; check the admin room".into())
}

fn is_success(reply: &str) -> bool {
    reply.to_ascii_lowercase().contains("successfully")
}

fn last_admin_message(cfg: &Cfg, room: &str) -> Option<String> {
    let (status, body) = http_call(
        cfg,
        "GET",
        &format!("/_matrix/client/v3/rooms/{room}/messages?dir=b&limit=12"),
        &cfg.admin_token,
        None,
    );
    if status != 200 {
        return None;
    }
    let mut idx = 0usize;
    while let Some(rel) = body[idx..].find("\"type\":\"m.room.message\"") {
        let start = idx + rel;
        let seg = &body[start..];
        if json_str(seg, "sender").as_deref() == Some(cfg.admin_user.as_str()) {
            if let Some(text) = json_str(seg, "body") {
                return Some(text);
            }
        }
        idx = start + 1;
        if idx >= body.len() {
            break;
        }
    }
    None
}

// ---------------------------------------------------------------------------
// http call against the homeserver

struct Uri {
    host: String,
    port: u16,
    path: String,
}

fn split_uri(s: &str) -> Option<Uri> {
    let rest = s.strip_prefix("http://")?;
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().ok()?),
        None => (hostport.to_string(), 80),
    };
    Some(Uri { host, port, path: path.to_string() })
}

fn content_length(head: &str) -> usize {
    head.lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            if k.trim().eq_ignore_ascii_case("content-length") {
                v.trim().parse().ok()
            } else {
                None
            }
        })
        .unwrap_or(0)
}

fn http_call(cfg: &Cfg, method: &str, path: &str, token: &str, body: Option<&str>) -> (u16, String) {
    let url = format!("{}{}", cfg.hs, path);
    let uri = match split_uri(&url) {
        Some(u) => u,
        None => return (0, "bad url".into()),
    };
    let addr = format!("{}:{}", uri.host, uri.port);
    let mut stream = match TcpStream::connect(&addr) {
        Ok(s) => s,
        Err(e) => return (0, format!("connect: {e}")),
    };
    stream.set_read_timeout(Some(std::time::Duration::from_secs(15))).ok();

    let mut req = format!("{method} {} HTTP/1.1\r\n", uri.path);
    req.push_str(&format!("Host: {}\r\n", addr));
    req.push_str("Accept: application/json\r\n");
    if !token.is_empty() {
        req.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    let payload = body.unwrap_or("");
    req.push_str(&format!("Content-Length: {}\r\n", payload.len()));
    if method == "POST" || method == "PUT" {
        req.push_str("Content-Type: application/json\r\n");
    }
    req.push_str("\r\n");
    req.push_str(payload);

    if stream.write_all(req.as_bytes()).is_err() {
        return (0, "write".into());
    }

    let mut resp = [0u8; http::MAX_REQ];
    let mut n = 0usize;
    let mut status: u16 = 0;
    loop {
        let got = match stream.read(&mut resp[n..]) {
            Ok(g) => g,
            Err(_) => break,
        };
        if got == 0 {
            break;
        }
        n += got;
        if n >= http::MAX_REQ {
            break;
        }
        if status == 0 {
            let text = String::from_utf8_lossy(&resp[..n]);
            if let Some(end) = text.find("\r\n") {
                status = text[..end].split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            }
        }
        if let Some(he) = http::find(&resp[..n], b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&resp[..he]);
            let cl = content_length(&head);
            if n >= he + 4 + cl {
                return (status, String::from_utf8_lossy(&resp[he + 4..he + 4 + cl]).to_string());
            }
        }
    }
    (status, String::from_utf8_lossy(&resp[..n]).to_string())
}

// ---------------------------------------------------------------------------
// tiny json string-field extractor. we only ever ask for strings, and
// continuwuity's replies are well-formed, so this is fine and dependency-free.

fn json_str(s: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":\"");
    let start = s.find(&pat)? + pat.len();
    let mut out = String::new();
    let mut chars = s[start..].chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(e) = chars.next() {
                    match e {
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        'r' => out.push('\r'),
                        'u' => {
                            let mut h = String::new();
                            for _ in 0..4 {
                                if let Some(x) = chars.next() {
                                    h.push(x);
                                }
                            }
                            if let Ok(v) = u32::from_str_radix(&h, 16) {
                                if let Some(ch) = char::from_u32(v) {
                                    out.push(ch);
                                }
                            }
                        }
                        e => out.push(e),
                    }
                }
            }
            '"' => break,
            c => out.push(c),
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

fn trunc(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n])
    }
}

#[cfg(test)]
mod tests {
    use super::content_length;

    #[test]
    fn content_length_is_case_insensitive() {
        assert_eq!(content_length("Content-Length: 27\r\nConnection: close"), 27);
        assert_eq!(content_length("content-length: 27\r\n"), 27);
        assert_eq!(content_length("CONTENT-LENGTH: 27\r\n"), 27);
        assert_eq!(content_length("Connection: close\r\n\r\n"), 0);
        assert_eq!(content_length("Content-Length: nope\r\n"), 0);
    }
}
