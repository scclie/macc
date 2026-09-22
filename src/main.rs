// macc: self-service matrix account portal.
//
// a kanidm user (authenticated by oauth2-proxy in front of this thing, on
// 127.0.0.1 only) creates their own matrix account or resets its password.
// there is no matrix admin http api, so we do what everyone with a
// continuwuity/conduit does: push `!admin users` commands into the #admins
// room over the plain client-server api and read the reply back.
//
// zero dependencies, plain std::net, one request at a time. it's a door
// handle, not a web framework.

mod config;
mod http;
mod matrix;
mod mapping;

use config::Cfg;
use std::net::{TcpListener, TcpStream};

const HTML: &str = include_str!("../www/index.html");

fn main() {
    let cfg = Cfg::from_env();
    if cfg.admin_token.is_empty() {
        eprintln!("ADMIN_TOKEN is required");
        std::process::exit(1);
    }

    let mapping = mapping::Mapping::new(&cfg.state_dir);
    let listener = TcpListener::bind(format!("127.0.0.1:{}", cfg.port)).expect("bind");
    eprintln!("macc listening on 127.0.0.1:{}", cfg.port);

    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                if let Err(e) = handle(&cfg, &mapping, s) {
                    eprintln!("request error: {e}");
                }
            }
            Err(e) => eprintln!("accept error: {e}"),
        }
    }
}

fn read_flash_cookie(req: &crate::http::Req) -> (Option<(String, String)>, Option<String>) {
    let cookies = req.headers.get("cookie").map(|c| c.as_str()).unwrap_or("");
    let mut msg = None;
    let mut password = None;
    for part in cookies.split(';') {
        let part = part.trim();
        if let Some((k, v)) = part.split_once('=') {
            match k {
                "macc_flash_msg" => {
                    if !v.is_empty() {
                        // format: "ok:message" or "error:message"
                        if let Some((kind, text)) = v.split_once(':') {
                            msg = Some((kind.to_string(), text.to_string()));
                        }
                    }
                }
                "macc_flash_password" => {
                    if !v.is_empty() {
                        password = Some(v.to_string());
                    }
                }
                _ => {}
            }
        }
    }
    (msg, password)
}

fn redirect_with_flash(stream: &mut std::net::TcpStream, msg_kind: &str, msg_text: &str, password: Option<&str>) -> std::io::Result<()> {
    let mut headers = String::new();
    headers.push_str(&format!("Set-Cookie: macc_flash_msg={}:{}; Path=/; HttpOnly; SameSite=Strict\r\n", msg_kind, msg_text));
    if let Some(pw) = password {
        headers.push_str(&format!("Set-Cookie: macc_flash_password={}; Path=/; HttpOnly; SameSite=Strict\r\n", pw));
    }
    http::respond(stream, "303 See Other", "text/plain", "", &format!("{}Location: /\r\n", headers))
}

fn handle(cfg: &Cfg, mapping: &mapping::Mapping, mut stream: TcpStream) -> std::io::Result<()> {
    let req = match http::read_request(&mut stream) {
        Ok(r) => r,
        Err(e) => {
            http::respond(&mut stream, "400 Bad Request", "text/plain", &e.to_string(), "")?;
            return Ok(());
        }
    };

    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/healthz") => http::respond(&mut stream, "200 OK", "text/plain", "ok", ""),
        ("GET", "/") => {
            let user = localpart_from_headers(cfg, &req.headers).unwrap_or_default();
            let mapped = mapping.get(&user);
            let csrf = http::random_token();
            
            // read flash cookie (one-time message from POST)
            let (flash_msg, flash_password) = read_flash_cookie(&req);
            
            let body = page(cfg, mapped.as_deref(), &user, flash_msg.as_ref().map(|(k,v)| (k.as_str(), v.clone())), flash_password.as_deref().unwrap_or(""), &csrf);
            let mut headers = format!("Set-Cookie: macc_csrf={csrf}; Path=/; HttpOnly; SameSite=Strict\r\n");
            // clear flash cookies
            headers.push_str("Set-Cookie: macc_flash_msg=; Path=/; Max-Age=0; HttpOnly; SameSite=Strict\r\n");
            headers.push_str("Set-Cookie: macc_flash_password=; Path=/; Max-Age=0; HttpOnly; SameSite=Strict\r\n");
            http::respond(
                &mut stream,
                "200 OK",
                "text/html",
                &body,
                &headers,
            )
        }
        ("POST", "/create") | ("POST", "/reset") => {
            let creating = req.path == "/create";
            let user = match localpart_from_headers(cfg, &req.headers) {
                Ok(v) => v,
                Err(e) => {
                    http::respond(&mut stream, "401 Unauthorized", "text/plain", &e, "")?;
                    return Ok(());
                }
            };

            let form_csrf = match http::form_value(&req.body, "csrf") {
                Some(v) => v,
                None => {
                    http::respond(&mut stream, "403 Forbidden", "text/plain", "csrf field missing", "")?;
                    return Ok(());
                }
            };
            let cookie_ok = req
                .headers
                .get("cookie")
                .is_some_and(|c| c.split(';').any(|p| p.trim() == format!("macc_csrf={form_csrf}")));
            if !cookie_ok {
                http::respond(&mut stream, "403 Forbidden", "text/plain", "csrf check failed", "")?;
                return Ok(());
            }

            let room = match matrix::resolve_admin_room(cfg) {
                Ok(r) => r,
                Err(e) => {
                    redirect_with_flash(&mut stream, "error", &e, None)?;
                    return Ok(());
                }
            };

            if creating {
                if let Some(existing) = mapping.get(&user) {
                    redirect_with_flash(&mut stream, "error", &format!("you already have @{existing}, use reset"), None)?;
                    return Ok(());
                }
                let nick = match http::form_value(&req.body, "nick") {
                    Some(n) => n,
                    None => {
                        redirect_with_flash(&mut stream, "error", "nick is required", None)?;
                        return Ok(());
                    }
                };
                if let Some(e) = validate_nick(&nick) {
                    redirect_with_flash(&mut stream, "error", &e, None)?;
                    return Ok(());
                }
                if matrix::profile_exists(cfg, &matrix::mxid(cfg, &nick)).unwrap_or(false) {
                    redirect_with_flash(&mut stream, "error", &format!("{nick} is taken"), None)?;
                    return Ok(());
                }
                let cmd = format!("!admin users create {}", nick);
                let target = matrix::mxid(cfg, &nick);
                match matrix::run_admin_command(cfg, &room, &cmd, &target) {
                    Ok(reply) => {
                        let password = matrix::extract_password(&reply, &target).unwrap_or_else(|| "unknown".to_string());
                        if let Err(e) = mapping.claim(&user, &nick) {
                            redirect_with_flash(&mut stream, "error", &format!("account created but mapping write failed: {e}"), Some(&password))?;
                            return Ok(());
                        }
                        redirect_with_flash(&mut stream, "ok", &format!("account @{nick}:{} created", cfg.domain), Some(&password))?;
                    }
                    Err(e) => {
                        redirect_with_flash(&mut stream, "error", &e, None)?;
                    }
                }
                return Ok(());
            }

            let nick = match mapping.get(&user) {
                Some(n) => n,
                None => {
                    redirect_with_flash(&mut stream, "error", "no account yet, create one first", None)?;
                    return Ok(());
                }
            };
            let target = matrix::mxid(cfg, &nick);
            let cmd = format!("!admin users reset-password --convert-to-local-account {target}");
            match matrix::run_admin_command(cfg, &room, &cmd, &target) {
                Ok(reply) => {
                    let password = matrix::extract_password(&reply, &target).unwrap_or_else(|| "unknown".to_string());
                    redirect_with_flash(&mut stream, "ok", &format!("password for @{nick}:{} reset", cfg.domain), Some(&password))?;
                }
                Err(e) => {
                    redirect_with_flash(&mut stream, "error", &e, None)?;
                }
            }
            Ok(())
        }
        _ => http::respond(&mut stream, "404 Not Found", "text/plain", "nope", ""),
    }
}

// identity comes from oauth2-proxy headers, never from the client

fn localpart_from_headers(cfg: &Cfg, headers: &std::collections::HashMap<String, String>) -> Result<String, String> {
    let user = headers
        .get("x-auth-request-preferred-username")
        .or(headers.get("x-auth-request-user"))
        .or(headers.get("x-forwarded-user"))
        .ok_or("no authenticated user header (is oauth2-proxy in front?)")?;

    if let Some(g) = &cfg.allowed_group {
        if let Some(groups) = headers.get("x-auth-request-groups") {
            if !groups.split(',').any(|x| x.trim() == g.as_str()) {
                return Err(format!("not a member of group '{g}'"));
            }
        }
    }

    let bare = user.rsplit('@').next_back().unwrap_or(user);
    let lp = bare.to_lowercase();
    let ok = !lp.is_empty()
        && lp.len() <= 255
        && lp.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'));
    if !ok {
        return Err(format!("username '{lp}' is not a valid matrix localpart"));
    }
    Ok(lp)
}

fn validate_nick(n: &str) -> Option<String> {
    let ok = !n.is_empty()
        && n.len() <= 255
        && n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'));
    if !ok {
        return Some("nick must be 1-255 chars of a-z 0-9 . _ -".into());
    }
    None
}

#[cfg(test)]
mod validate_nick_tests {
    use super::*;

    #[test]
    fn nick_empty_is_rejected() {
        assert!(validate_nick("").is_some());
    }

    #[test]
    fn nick_uppercase_is_rejected() {
        assert!(validate_nick("Test").is_some());
    }

    #[test]
    fn nick_short_lowercase_is_accepted() {
        assert!(validate_nick(&"a".repeat(20)).is_none());
    }

    #[test]
    fn nick_over_255_is_rejected() {
        assert!(validate_nick(&"a".repeat(256)).is_some());
    }

    #[test]
    fn nick_with_allowed_chars_is_accepted() {
        assert!(validate_nick("ab_cd.ef-gh1").is_none());
    }
}

#[cfg(test)]
mod localpart_tests {
    use std::collections::HashMap;

    fn lp_from(h: &[(&str, &str)]) -> Result<String, String> {
        let mut m = HashMap::new();
        for (k, v) in h {
            m.insert(k.to_string(), v.to_string());
        }
        let cfg = super::Cfg {
            port: String::new(),
            hs: String::new(),
            domain: String::new(),
            web_url: String::new(),
            admin_token: String::new(),
            admin_user: String::new(),
            admin_room: String::new(),
            allowed_group: None,
            state_dir: String::new(),
        };
        super::localpart_from_headers(&cfg, &m)
    }

    #[test]
    fn preferred_username_with_domain_yields_localpart() {
        assert_eq!(lp_from(&[("x-auth-request-preferred-username", "alice@id.example.org")]).unwrap(), "alice");
    }

    #[test]
    fn email_header_yields_localpart_before_at() {
        assert_eq!(lp_from(&[("x-auth-request-user", "alice@example.com")]).unwrap(), "alice");
    }

    #[test]
    fn bare_username_passes_through() {
        assert_eq!(lp_from(&[("x-auth-request-user", "alice")]).unwrap(), "alice");
    }
}

fn page(cfg: &Cfg, existing_nick: Option<&str>, default_nick: &str, msg: Option<(&str, String)>, password: &str, csrf: &str) -> String {
    let message = msg
        .map(|(kind, text)| format!("<p class=\"{}\">{}</p>", kind, http::html_escape(&text)))
        .unwrap_or_default();
    let web_url = if cfg.web_url.is_empty() {
        String::new()
    } else {
        format!("log in at <a href=\"{}\">{}</a> after creating.", http::html_escape(&cfg.web_url), http::html_escape(&cfg.web_url))
    };
    let password_block = if password.is_empty() || password == "unknown" {
        String::new()
    } else {
        format!("<p><strong>your password:</strong> <code style=\"background:#3b4252;padding:0.3em;font-size:1.1em;\">{}</code></p><p><small>⚠️ change this in your matrix client after logging in.</small></p>", http::html_escape(password))
    };
    let shown_nick = existing_nick.unwrap_or(default_nick);
    HTML
        .replace("{%DOMAIN%}", &http::html_escape(&cfg.domain))
        .replace("{%WEB_URL%}", &web_url)
        .replace("{%LP%}", &http::html_escape(shown_nick))
        .replace("{%CREATENICK%}", &http::html_escape(default_nick))
        .replace("{%CSRF%}", &http::html_escape(csrf))
        .replace("{%PASSWORD%}", &password_block)
        .replace("{%EXISTS_CREATE%}", if existing_nick.is_some() { "hidden" } else { "" })
        .replace("{%EXISTS_RESET%}", if existing_nick.is_none() { "hidden" } else { "" })
        .replace("{%MSG%}", &message)
}
