use std::collections::HashMap;
use std::fs;
use std::path::Path;

pub struct Mapping {
    pub path: String,
}

impl Mapping {
    pub fn new(state_dir: &str) -> Self {
        Mapping {
            path: format!("{}/mapping.json", state_dir.trim_end_matches('/')),
        }
    }

    fn read(&self) -> Result<HashMap<String, String>, String> {
        match fs::read_to_string(&self.path) {
            Ok(text) => parse_obj(&text).ok_or_else(|| format!("mapping file corrupt at {}", self.path)),
            Err(_) => Ok(HashMap::new()),
        }
    }

    pub fn get(&self, user: &str) -> Option<String> {
        self.read().ok().and_then(|m| m.get(&user.to_lowercase()).cloned())
    }

    pub fn claim(&self, user: &str, localpart: &str) -> Result<(), String> {
        let mut map = self.read()?;
        map.insert(user.to_lowercase(), localpart.to_lowercase());
        write_atomic(&self.path, &render_obj(&map))
    }
}

fn parse_obj(text: &str) -> Option<HashMap<String, String>> {
    let text = text.trim();
    let body = text.strip_prefix('{')?.strip_suffix('}')?.trim();
    if body.is_empty() {
        return Some(HashMap::new());
    }
    let mut map = HashMap::new();
    for pair in body.split(',') {
        let (k, v) = pair.split_once(':')?;
        map.insert(unquote(k.trim())?, unquote(v.trim())?);
    }
    Some(map)
}

fn unquote(s: &str) -> Option<String> {
    let s = s.strip_prefix('"')?.strip_suffix('"')?;
    Some(s.replace("\\\"", "\"").replace("\\\\", "\\"))
}

fn render_obj(map: &HashMap<String, String>) -> String {
    let mut parts: Vec<String> = map
        .iter()
        .map(|(k, v)| format!("\"{}\":\"{}\"", k, v))
        .collect();
    parts.sort();
    format!("{{{}}}", parts.join(","))
}

fn write_atomic(path: &str, content: &str) -> Result<(), String> {
    let dir = Path::new(path).parent().ok_or("bad path")?;
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let tmp = format!("{path}.tmp");
    fs::write(&tmp, content).map_err(|e| e.to_string())?;
    fs::File::open(&tmp).map_err(|e| e.to_string())?.sync_all().map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    fs::File::open(dir).map_err(|e| e.to_string())?.sync_all().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapping_roundtrip() {
        let dir = std::env::temp_dir().join("macc-mapping-test");
        let _ = fs::remove_dir_all(&dir);
        let m = Mapping::new(dir.to_str().unwrap());
        assert_eq!(m.get("alice"), None);
        m.claim("Alice", "alice123").unwrap();
        assert_eq!(m.get("alice").as_deref(), Some("alice123"));
        let reloaded = Mapping::new(dir.to_str().unwrap());
        assert_eq!(reloaded.get("alice").as_deref(), Some("alice123"));
    }

    #[test]
    fn mapping_claim_overwrites_same_user() {
        let dir = std::env::temp_dir().join("macc-mapping-test2");
        let _ = fs::remove_dir_all(&dir);
        let m = Mapping::new(dir.to_str().unwrap());
        m.claim("alice", "one").unwrap();
        m.claim("alice", "two").unwrap();
        assert_eq!(m.get("alice").as_deref(), Some("two"));
    }
}