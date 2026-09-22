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
    let (status, body) = http_call(cfg, "GET", &format!("/_matrix/client/v3/profile/{target}"), &cfg.admin_token, None);
    match status {
        200 => {
            // continuwuity returns 200 with empty body for non-existent profiles
            let trimmed = body.trim();
            Ok(!trimmed.is_empty() && trimmed != "{}")
        }
        404 => Ok(false),
        0 => Err("profile check: connect/fetch failed".into()),
        s => Err(format!("profile check: http {s}")),
    }
}

pub fn run_admin_command(cfg: &Cfg, room: &str, command: &str, expected_mxid: &str) -> Result<String, String> {
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

    let cmd_event_id = json_str(&b, "event_id").ok_or("no event_id in PUT response")?;

    for _ in 0..15 {
        std::thread::sleep(std::time::Duration::from_millis(800));
        if let Some(reply) = reply_to_event(cfg, room, &cmd_event_id, expected_mxid) {
            if is_success(&reply) {
                return Ok(reply);
            }
            return Err(reply);
        }
    }
    Err("no server reply within timeout; check the admin room".into())
}

fn reply_to_event(cfg: &Cfg, room: &str, target_event_id: &str, expected_mxid: &str) -> Option<String> {
    let (status, body) = http_call(
        cfg,
        "GET",
        &format!("/_matrix/client/v3/rooms/{room}/messages?dir=b&limit=20"),
        &cfg.admin_token,
        None,
    );
    if status != 200 {
        return None;
    }

    // split body into individual message objects by finding "type":"m.room.message"
    let mut idx = 0usize;
    while let Some(rel) = body[idx..].find("\"type\":\"m.room.message\"") {
        let type_pos = idx + rel;

        // find the start of this message object (go back to find "content":{)
        let msg_start = body[..type_pos].rfind("\"content\":{").unwrap_or(type_pos);

        // find the end of this message object (next "type":"m.room.message" AFTER current)
        let msg_end = body[type_pos + 1..].find("\"type\":\"m.room.message\"")
            .map(|next| type_pos + 1 + next)
            .unwrap_or(body.len());

        let msg = &body[msg_start..msg_end];

        let sender = json_str(msg, "sender");
        let text = json_str(msg, "body");

        // find in_reply_to event_id in the full message
        let reply_to = msg.find("\"m.in_reply_to\"")
            .and_then(|pos| {
                let segment = &msg[pos..];
                segment.find("\"event_id\":\"").map(|pos2| {
                    let val_start = pos2 + "\"event_id\":\"".len();
                    let val_end = segment[val_start..].find('"').map(|e| val_start + e).unwrap_or(val_start);
                    segment[val_start..val_end].to_string()
                })
            });

        if let (Some(s), Some(t), Some(reply_eid)) = (sender, text, reply_to.as_deref()) {
            if reply_eid == target_event_id && s != cfg.admin_user && t.contains(expected_mxid) {
                return Some(t);
            }
        }

        idx = type_pos + 1;
        if idx >= body.len() {
            break;
        }
    }
    None
}

fn is_success(reply: &str) -> bool {
    let lower = reply.to_ascii_lowercase();
    lower.contains("successfully") || lower.contains("created new user") || lower.contains("created user")
}

pub fn extract_password(reply: &str, expected_mxid: &str) -> Option<String> {
    let mxid_pos = reply.find(expected_mxid)?;
    let start = reply[mxid_pos..].find('`')? + mxid_pos + 1;
    let end = reply[start..].find('`')? + start;
    Some(reply[start..end].to_string())
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

fn is_chunked(head: &str) -> bool {
    head.lines().any(|l| {
        l.split_once(':')
            .map(|(k, v)| k.trim().eq_ignore_ascii_case("transfer-encoding") && v.trim().eq_ignore_ascii_case("chunked"))
            .unwrap_or(false)
    })
}

fn decode_chunked(data: &[u8]) -> Option<String> {
    let mut out = String::new();
    let mut pos = 0;
    let bytes = data;
    while pos < bytes.len() {
        let line_end = bytes[pos..].windows(2).position(|w| w == b"\r\n")? + pos;
        let size_str = std::str::from_utf8(&bytes[pos..line_end]).ok()?;
        let size = usize::from_str_radix(size_str.trim(), 16).ok()?;
        if size == 0 {
            return Some(out);
        }
        let chunk_start = line_end + 2;
        let chunk_end = chunk_start + size;
        if chunk_end > bytes.len() {
            return None;
        }
        out.push_str(&String::from_utf8_lossy(&bytes[chunk_start..chunk_end]));
        pos = chunk_end + 2;
    }
    Some(out)
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
            let chunked = is_chunked(&head);
            if chunked {
                let body_start = he + 4;
                let body_data = &resp[body_start..n];
                if let Some(decoded) = decode_chunked(body_data) {
                    return (status, decoded);
                }
            } else {
                let cl = content_length(&head);
                if n >= he + 4 + cl {
                    return (status, String::from_utf8_lossy(&resp[he + 4..he + 4 + cl]).to_string());
                }
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
    use super::{content_length, is_chunked, decode_chunked, extract_password, is_success};

    #[test]
    fn content_length_is_case_insensitive() {
        assert_eq!(content_length("Content-Length: 27\r\nConnection: close"), 27);
        assert_eq!(content_length("content-length: 27\r\n"), 27);
        assert_eq!(content_length("CONTENT-LENGTH: 27\r\n"), 27);
        assert_eq!(content_length("Connection: close\r\n\r\n"), 0);
        assert_eq!(content_length("Content-Length: nope\r\n"), 0);
    }

    #[test]
    fn is_chunked_detects_transfer_encoding() {
        assert!(is_chunked("Transfer-Encoding: chunked\r\n"));
        assert!(is_chunked("transfer-encoding: Chunked\r\n"));
        assert!(!is_chunked("Content-Length: 100\r\n"));
        assert!(!is_chunked(""));
    }

    #[test]
    fn decode_chunked_handles_single_chunk() {
        let data = b"3b\r\n{\"room_id\":\"!abc123:example.org\",\"servers\":[\"example.org\"]}\r\n0\r\n\r\n";
        assert_eq!(decode_chunked(data), Some("{\"room_id\":\"!abc123:example.org\",\"servers\":[\"example.org\"]}".to_string()));
    }

    #[test]
    fn decode_chunked_handles_multiple_chunks() {
        let data = b"5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
        assert_eq!(decode_chunked(data), Some("hello world".to_string()));
    }

    #[test]
    fn extract_password_from_reset_reply() {
        let reply = "Successfully reset the password for user @alice:example.org: `PKYdRVM0Yt7Gbv39uhDkPDqFX`";
        assert_eq!(extract_password(reply, "@alice:example.org"), Some("PKYdRVM0Yt7Gbv39uhDkPDqFX".to_string()));
    }

    #[test]
    fn extract_password_from_create_reply() {
        let reply = "Successfully created user @bob:example.org with password `abc123xyz`";
        assert_eq!(extract_password(reply, "@bob:example.org"), Some("abc123xyz".to_string()));
    }

    #[test]
    fn extract_password_returns_none_if_no_backticks() {
        let reply = "Successfully reset the password";
        assert_eq!(extract_password(reply, "@alice:example.org"), None);
    }

    #[test]
    fn extract_password_ignores_backticks_before_mxid() {
        let reply = "Some text with `fake` before @alice:example.org: `realpassword`";
        assert_eq!(extract_password(reply, "@alice:example.org"), Some("realpassword".to_string()));
    }

    #[test]
    fn is_success_detects_successful_responses() {
        assert!(is_success("Successfully reset the password for user @alice:example.org: `abc123`"));
        assert!(is_success("Successfully created user @bob:example.org with password `xyz789`"));
        assert!(is_success("| level | span | message |\n| ------: | :-----: | :------- |\n|  INFO |   command    | Created new user account for @charlie:example.org |\n\nCreated user @charlie:example.org with password `abc123`"));
        assert!(!is_success("Command failed with error: This account does not exist."));
        assert!(!is_success("Some other error"));
    }
}
