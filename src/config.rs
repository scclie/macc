// runtime config, all from the environment so the nix module stays boring.
// deploy-specific values (your homeserver, your domain) live in the
// deployment config, never in this repo.

pub struct Cfg {
    pub port: String,
    pub hs: String, // homeserver client api base, e.g. http://10.0.0.19:6167
    pub domain: String, // matrix server_name, e.g. example.org
    pub web_url: String, // where users actually log in (element/cinny), for the page footer
    pub admin_token: String, // access token of the admin bot
    pub admin_user: String, // mxid of that bot (replies to admin commands come from it)
    pub admin_room: String, // room alias of the admin room, e.g. #admins:example.org
    pub allowed_group: Option<String>, // oidc group required (oauth2-proxy enforces too)
    pub password_min: usize,
    pub state_dir: String,
}

impl Cfg {
    pub fn from_env() -> Self {
        let cfg = Cfg {
            port: env_or("PORT", "8787"),
            hs: env_or("HS", "").trim_end_matches('/').to_string(),
            domain: env_or("HS_DOMAIN", ""),
            web_url: env_or("WEB_URL", ""),
            admin_token: env_or("ADMIN_TOKEN", ""),
            admin_user: env_or("ADMIN_USER", ""),
            admin_room: env_or("ADMIN_ROOM", ""),
            allowed_group: std::env::var("ALLOWED_GROUP").ok().filter(|s| !s.is_empty()),
            password_min: env_or("PASSWORD_MIN", "10").parse().unwrap_or(10),
            state_dir: env_or("STATE_DIR", "/var/lib/macc"),
        };

        let mut missing: Vec<&'static str> = Vec::new();
        if cfg.hs.is_empty() {
            missing.push("HS");
        }
        if cfg.domain.is_empty() {
            missing.push("HS_DOMAIN");
        }
        if cfg.admin_token.is_empty() {
            missing.push("ADMIN_TOKEN");
        }
        if cfg.admin_user.is_empty() {
            missing.push("ADMIN_USER");
        }
        if cfg.admin_room.is_empty() {
            missing.push("ADMIN_ROOM");
        }
        if !missing.is_empty() {
            eprintln!(
                "macc: missing required env vars: {}\n  HS=                 homeserver client api base, e.g. http://10.0.0.19:6167\n  HS_DOMAIN=          matrix server_name, e.g. example.org\n  ADMIN_TOKEN=        access token of the admin bot\n  ADMIN_USER=         mxid of that bot, e.g. @maccbot:example.org\n  ADMIN_ROOM=         admin room alias, e.g. #admins:example.org\n  WEB_URL=            (optional) element/cinny url for the page footer\n  ALLOWED_GROUP=      (optional) oidc group required to use the portal\n  PORT=               (default 8787)\n  PASSWORD_MIN=       (default 10)\n  STATE_DIR=          (optional, default /var/lib/macc)",
                missing.join(", ")
            );
            std::process::exit(1);
        }
        cfg
    }
}

fn env_or(k: &str, d: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| d.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_dir_from_env_with_default() {
        std::env::set_var("STATE_DIR", "/tmp/macc-test");
        std::env::set_var("HS", "http://fail:1");
        std::env::set_var("HS_DOMAIN", "x");
        std::env::set_var("ADMIN_TOKEN", "x");
        std::env::set_var("ADMIN_USER", "@b:x");
        std::env::set_var("ADMIN_ROOM", "#a:x");
        let cfg = Cfg::from_env();
        assert_eq!(cfg.state_dir, "/tmp/macc-test");
        std::env::remove_var("STATE_DIR");
        let cfg = Cfg::from_env();
        assert_eq!(cfg.state_dir, "/var/lib/macc");
    }
}