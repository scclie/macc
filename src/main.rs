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
            let body = page(cfg, mapped.as_deref(), &user, None, &csrf);
            http::respond(
                &mut stream,
                "200 OK",
                "text/html",
                &body,
                &format!("Set-Cookie: macc_csrf={csrf}; Path=/; HttpOnly; SameSite=Strict\r\n"),
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

            let password = match http::form_value(&req.body, "password") {
                Some(p) => p,
                None => {
                    http::respond(&mut stream, "400 Bad Request", "text/plain", "password is required", "")?;
                    return Ok(());
                }
            };
            if let Some(e) = validate_password(&password, cfg.password_min) {
                let body = page(cfg, mapping.get(&user).as_deref(), &user, Some(("error", e)), "");
                http::respond(&mut stream, "200 OK", "text/html", &body, "")?;
                return Ok(());
            }

            let room = match matrix::resolve_admin_room(cfg) {
                Ok(r) => r,
                Err(e) => {
                    let body = page(cfg, None, &user, Some(("error", e)), "");
                    http::respond(&mut stream, "502 Bad Gateway", "text/html", &body, "")?;
                    return Ok(());
                }
            };

            if creating {
                if let Some(existing) = mapping.get(&user) {
                    let body = page(cfg, Some(&existing), &existing, Some(("error", format!("you already have @{existing}, use reset"))), "");
                    http::respond(&mut stream, "200 OK", "text/html", &body, "")?;
                    return Ok(());
                }
                let nick = match http::form_value(&req.body, "nick") {
                    Some(n) => n,
                    None => {
                        let body = page(cfg, None, &user, Some(("error", "nick is required".into())), "");
                        http::respond(&mut stream, "200 OK", "text/html", &body, "")?;
                        return Ok(());
                    }
                };
                if let Some(e) = validate_nick(&nick) {
                    let body = page(cfg, None, &user, Some(("error", e)), "");
                    http::respond(&mut stream, "200 OK", "text/html", &body, "")?;
                    return Ok(());
                }
                if matrix::profile_exists(cfg, &matrix::mxid(cfg, &nick)).unwrap_or(false) {
                    let body = page(cfg, None, &nick, Some(("error", format!("{nick} is taken"))), "");
                    http::respond(&mut stream, "200 OK", "text/html", &body, "")?;
                    return Ok(());
                }
                let cmd = format!("!admin users create {} {}", nick, password);
                match matrix::run_admin_command(cfg, &room, &cmd) {
                    Ok(_) => {
                        if let Err(e) = mapping.claim(&user, &nick) {
                            let body = page(cfg, None, &nick, Some(("error", format!("account created but mapping write failed: {e}"))), "");
                            http::respond(&mut stream, "200 OK", "text/html", &body, "")?;
                            return Ok(());
                        }
                        let body = page(cfg, Some(&nick), &nick, Some(("ok", format!("account @{nick}:{} created. {}", cfg.domain, if cfg.web_url.is_empty() { String::from("log in.") } else { format!("log in at {}.", cfg.web_url) }))), "");
                        http::respond(&mut stream, "200 OK", "text/html", &body, "")?;
                    }
                    Err(e) => {
                        let body = page(cfg, None, &nick, Some(("error", e)), "");
                        http::respond(&mut stream, "200 OK", "text/html", &body, "")?;
                    }
                }
                return Ok(());
            }

            let nick = match mapping.get(&user) {
                Some(n) => n,
                None => {
                    let body = page(cfg, None, &user, Some(("error", "no account yet, create one first".into())), "");
                    http::respond(&mut stream, "200 OK", "text/html", &body, "")?;
                    return Ok(());
                }
            };
            let target = matrix::mxid(cfg, &nick);
            let cmd = format!("!admin users reset-password --convert-to-local-account {target} {password}");
            match matrix::run_admin_command(cfg, &room, &cmd) {
                Ok(_) => {
                    let body = page(cfg, Some(&nick), &nick, Some(("ok", format!("password for @{nick}:{} updated", cfg.domain))), "");
                    http::respond(&mut stream, "200 OK", "text/html", &body, "")?;
                }
                Err(e) => {
                    let body = page(cfg, Some(&nick), &nick, Some(("error", e)), "");
                    http::respond(&mut stream, "200 OK", "text/html", &body, "")?;
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

    let bare = user.rsplit('@').next().unwrap_or(user);
    let lp = bare.to_lowercase();
    let ok = !lp.is_empty()
        && lp.len() <= 255
        && lp.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'));
    if !ok {
        return Err(format!("username '{lp}' is not a valid matrix localpart"));
    }
    Ok(lp)
}

fn validate_password(p: &str, min: usize) -> Option<String> {
    if p.len() < min {
        return Some(format!("password must be at least {min} characters"));
    }
    if p.len() > 128 {
        return Some("password too long".into());
    }
    let ok = p.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(
                c,
                '!' | '@' | '#' | '$' | '%' | '^' | '&' | '*' | '(' | ')' | '_' | '=' | '+' | '-' | '.' | ',' | ';' | ':' | '?' | '~'
            )
    });
    if !ok {
        return Some("password has characters that are not safe inside a matrix admin command".into());
    }
    None
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

fn page(cfg: &Cfg, existing_nick: Option<&str>, default_nick: &str, msg: Option<(&str, String)>, csrf: &str) -> String {
    let message = msg
        .map(|(kind, text)| format!("<p class=\"{}\">{}</p>", kind, http::html_escape(&text)))
        .unwrap_or_default();
    let web_url = if cfg.web_url.is_empty() {
        String::new()
    } else {
        format!("log in at <a href=\"{}\">{}</a> after creating.", http::html_escape(&cfg.web_url), http::html_escape(&cfg.web_url))
    };
    let shown_nick = existing_nick.unwrap_or(default_nick);
    HTML
        .replace("{%DOMAIN%}", &http::html_escape(&cfg.domain))
        .replace("{%WEB_URL%}", &web_url)
        .replace("{%LP%}", &http::html_escape(shown_nick))
        .replace("{%CREATENICK%}", &http::html_escape(default_nick))
        .replace("{%CSRF%}", &http::html_escape(csrf))
        .replace("{%EXISTS_CREATE%}", if existing_nick.is_some() { "hidden" } else { "" })
        .replace("{%EXISTS_RESET%}", if existing_nick.is_none() { "hidden" } else { "" })
        .replace("{%MSG%}", &message)
}