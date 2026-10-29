use crate::project::PermCfg;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Perms {
    pub root: PathBuf,
    pub cfg: PermCfg,
}

fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Canonicalize the longest existing prefix of the path, then re-append the rest
/// (symlinks in existing parts are resolved, so they cannot escape the root).
fn resolve_real(p: &Path) -> PathBuf {
    let mut existing = p.to_path_buf();
    let mut rest: Vec<std::ffi::OsString> = vec![];
    loop {
        if let Ok(c) = existing.canonicalize() {
            let mut out = c;
            for r in rest.iter().rev() {
                out.push(r);
            }
            return out;
        }
        match (existing.file_name().map(|s| s.to_os_string()), existing.parent().map(|p| p.to_path_buf())) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                existing = parent;
            }
            _ => return p.to_path_buf(),
        }
    }
}

impl Perms {
    pub fn new(root: PathBuf, cfg: PermCfg) -> Perms {
        Perms { root, cfg }
    }

    pub fn all_denied(root: PathBuf) -> Perms {
        Perms {
            root,
            cfg: PermCfg { filesystem: "deny".into(), network: "deny".into(), process: "deny".into(), environment: "deny".into(), network_allow: vec![], process_allow: vec![], env_allow: vec![] },
        }
    }

    pub fn resolve_path(&self, p: &str) -> PathBuf {
        let path = Path::new(p);
        if path.is_absolute() { lexical_normalize(path) } else { lexical_normalize(&self.root.join(path)) }
    }

    /// Returns the checked absolute path or a permission error message.
    pub fn fs_path(&self, p: &str, write: bool) -> Result<PathBuf, String> {
        let mode = self.cfg.filesystem.as_str();
        if mode == "deny" {
            return Err(format!("filesystem access is denied by [permissions].filesystem (path: {})", p));
        }
        if write && mode == "project-readonly" {
            return Err(format!("filesystem is read-only (permissions.filesystem = \"project-readonly\"); cannot modify {}", p));
        }
        let abs = self.resolve_path(p);
        if mode == "allow" {
            return Ok(abs);
        }
        let real = resolve_real(&abs);
        let root = resolve_real(&self.root);
        if real.starts_with(&root) {
            Ok(abs)
        } else {
            Err(format!("path '{}' is outside the project root; blocked by [permissions].filesystem = \"{}\"", p, mode))
        }
    }

    pub fn network_host(&self, url: &str) -> Result<(), String> {
        match self.cfg.network.as_str() {
            "allow" => Ok(()),
            "deny" => Err(format!("network access is denied by [permissions].network (url: {})", url)),
            _ => {
                let host = url_host(url).ok_or_else(|| format!("invalid url '{}'", url))?;
                if self.cfg.network_allow.iter().any(|a| host_matches(a, &host)) {
                    Ok(())
                } else {
                    Err(format!("host '{}' is not in [permissions].network_allow", host))
                }
            }
        }
    }

    pub fn process_cmd(&self, cmd: &str) -> Result<(), String> {
        match self.cfg.process.as_str() {
            "allow" => Ok(()),
            "deny" => Err(format!("process execution is denied by [permissions].process (command: {})", cmd)),
            _ => {
                if self.cfg.process_allow.iter().any(|a| a == cmd) {
                    Ok(())
                } else {
                    Err(format!("command '{}' is not in [permissions].process_allow", cmd))
                }
            }
        }
    }

    pub fn env_var(&self, name: &str) -> Result<(), String> {
        match self.cfg.environment.as_str() {
            "allow" => Ok(()),
            "deny" => Err(format!("environment access is denied by [permissions].environment (variable: {})", name)),
            _ => {
                if self.cfg.env_allow.iter().any(|a| a == name) {
                    Ok(())
                } else {
                    Err(format!("environment variable '{}' is not in [permissions].env_allow", name))
                }
            }
        }
    }
}

pub fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let auth = rest.split(['/', '?', '#']).next()?;
    let auth = auth.rsplit('@').next()?;
    let host = if let Some(stripped) = auth.strip_prefix('[') { stripped.split(']').next()?.to_string() } else { auth.split(':').next()?.to_string() };
    if host.is_empty() { None } else { Some(host.to_lowercase()) }
}

fn host_matches(pattern: &str, host: &str) -> bool {
    let p = pattern.to_lowercase();
    if let Some(suffix) = p.strip_prefix("*.") { host.ends_with(&format!(".{}", suffix)) } else { p == host }
}
