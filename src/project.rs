use crate::ast::*;
use crate::diag::*;
use crate::lexer::Span;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

// ---------------- configuration ----------------

fn d_true() -> bool {
    true
}
fn d_version() -> String {
    "0.1.0".into()
}
fn d_entry() -> String {
    "src/main.cls".into()
}
fn d_protocol() -> String {
    "2025-11-25".into()
}
fn d_transport() -> String {
    "stdio".into()
}
fn d_host() -> String {
    "127.0.0.1".into()
}
fn d_port() -> u16 {
    3000
}
fn d_path() -> String {
    "/mcp".into()
}
fn d_project() -> String {
    "project".into()
}
fn d_deny() -> String {
    "deny".into()
}
fn d_allowlisted() -> String {
    "allowlisted".into()
}
fn d_exec() -> f64 {
    30.0
}
fn d_conc() -> usize {
    16
}
fn d_in() -> usize {
    1_048_576
}
fn d_out() -> usize {
    4_194_304
}
fn d_depth() -> usize {
    400
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectCfg {
    pub name: String,
    #[serde(default = "d_version")]
    pub version: String,
    #[serde(default = "d_entry")]
    pub entry: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub instructions: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompilerCfg {
    #[serde(default = "d_true")]
    pub strict_types: bool,
    #[serde(default)]
    pub warnings_as_errors: bool,
}
impl Default for CompilerCfg {
    fn default() -> Self {
        CompilerCfg { strict_types: true, warnings_as_errors: false }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpCfg {
    #[serde(default = "d_protocol")]
    pub protocol: String,
    #[serde(default = "d_transport")]
    pub transport: String,
    #[serde(default = "d_host")]
    pub host: String,
    #[serde(default = "d_port")]
    pub port: u16,
    #[serde(default = "d_path")]
    pub path: String,
    #[serde(default)]
    pub allowed_origins: Vec<String>,
}
impl Default for McpCfg {
    fn default() -> Self {
        McpCfg { protocol: d_protocol(), transport: d_transport(), host: d_host(), port: d_port(), path: d_path(), allowed_origins: vec![] }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermCfg {
    #[serde(default = "d_project")]
    pub filesystem: String,
    #[serde(default = "d_deny")]
    pub network: String,
    #[serde(default = "d_deny")]
    pub process: String,
    #[serde(default = "d_allowlisted")]
    pub environment: String,
    #[serde(default)]
    pub network_allow: Vec<String>,
    #[serde(default)]
    pub process_allow: Vec<String>,
    #[serde(default)]
    pub env_allow: Vec<String>,
}
impl Default for PermCfg {
    fn default() -> Self {
        PermCfg {
            filesystem: d_project(),
            network: d_deny(),
            process: d_deny(),
            environment: d_allowlisted(),
            network_allow: vec![],
            process_allow: vec![],
            env_allow: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsCfg {
    #[serde(default = "d_exec")]
    pub max_execution_seconds: f64,
    #[serde(default = "d_conc")]
    pub max_concurrent_requests: usize,
    #[serde(default = "d_in")]
    pub max_input_bytes: usize,
    #[serde(default = "d_out")]
    pub max_output_bytes: usize,
    #[serde(default = "d_depth")]
    pub max_call_depth: usize,
}
impl Default for LimitsCfg {
    fn default() -> Self {
        LimitsCfg {
            max_execution_seconds: d_exec(),
            max_concurrent_requests: d_conc(),
            max_input_bytes: d_in(),
            max_output_bytes: d_out(),
            max_call_depth: d_depth(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub project: ProjectCfg,
    #[serde(default)]
    pub compiler: CompilerCfg,
    #[serde(default)]
    pub mcp: McpCfg,
    #[serde(default)]
    pub permissions: PermCfg,
    #[serde(default)]
    pub limits: LimitsCfg,
}

pub const SUPPORTED_PROTOCOLS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

impl Config {
    pub fn parse(text: &str) -> Result<Config, String> {
        let cfg: Config = toml::from_str(text).map_err(|e| e.to_string())?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<(), String> {
        let one_of = |what: &str, v: &str, allowed: &[&str]| -> Result<(), String> {
            if allowed.contains(&v) {
                Ok(())
            } else {
                Err(format!("invalid {} '{}': expected one of {}", what, v, allowed.join(", ")))
            }
        };
        if self.project.name.trim().is_empty() {
            return Err("project.name must not be empty".into());
        }
        one_of("mcp.protocol", &self.mcp.protocol, &SUPPORTED_PROTOCOLS)?;
        one_of("mcp.transport", &self.mcp.transport, &["stdio", "http"])?;
        one_of("permissions.filesystem", &self.permissions.filesystem, &["deny", "project-readonly", "project", "allow"])?;
        one_of("permissions.network", &self.permissions.network, &["deny", "allowlisted", "allow"])?;
        one_of("permissions.process", &self.permissions.process, &["deny", "allowlisted", "allow"])?;
        one_of("permissions.environment", &self.permissions.environment, &["deny", "allowlisted", "allow"])?;
        if !self.mcp.path.starts_with('/') {
            return Err("mcp.path must start with '/'".into());
        }
        if self.limits.max_execution_seconds <= 0.0 {
            return Err("limits.max_execution_seconds must be positive".into());
        }
        if self.limits.max_concurrent_requests == 0 {
            return Err("limits.max_concurrent_requests must be at least 1".into());
        }
        if self.limits.max_call_depth < 8 {
            return Err("limits.max_call_depth must be at least 8".into());
        }
        Ok(())
    }
}

pub struct Project {
    pub root: PathBuf,
    pub config: Config,
}

pub const CONFIG_NAMES: [&str; 2] = ["Claudescript.toml", "claudescript.toml"];

pub fn find_config(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        for n in CONFIG_NAMES {
            let p = d.join(n);
            if p.is_file() {
                return Some(p);
            }
        }
        dir = d.parent();
    }
    None
}

pub fn load_project(start: &Path) -> Result<Project, String> {
    let cfg_path = find_config(start).ok_or_else(|| format!("no Claudescript.toml found in {} or any parent directory", start.display()))?;
    let text = std::fs::read_to_string(&cfg_path).map_err(|e| format!("cannot read {}: {}", cfg_path.display(), e))?;
    let config = Config::parse(&text).map_err(|e| format!("{}: {}", cfg_path.display(), e))?;
    let root = cfg_path.parent().unwrap().canonicalize().map_err(|e| e.to_string())?;
    Ok(Project { root, config })
}

// ---------------- module loading ----------------

pub struct Source {
    pub key: String,
    pub display: String,
    pub module: Module,
    /// Resolved dependencies by import specifier.
    pub deps: HashMap<String, usize>,
    pub is_std: bool,
    /// For a header declaring functions/constants: its implementation module.
    pub impl_id: Option<usize>,
}

pub struct Loaded {
    pub modules: Vec<Source>,
    pub sources: SourceMap,
    /// Indices of the modules that were requested as roots.
    pub roots: Vec<usize>,
}

pub const STD_MODULES: &[(&str, &str)] = &[
    ("std/prelude", include_str!("../std/prelude.clsh")),
    ("std/fs", include_str!("../std/fs.clsh")),
    ("std/path", include_str!("../std/path.clsh")),
    ("std/http", include_str!("../std/http.clsh")),
    ("std/process", include_str!("../std/process.clsh")),
    ("std/env", include_str!("../std/env.clsh")),
    ("std/time", include_str!("../std/time.clsh")),
    ("std/mcp", include_str!("../std/mcp.clsh")),
    ("std/encoding", include_str!("../std/encoding.clsh")),
    ("std/crypto", include_str!("../std/crypto.clsh")),
    ("std/regex", include_str!("../std/regex.clsh")),
];

fn has_declares(m: &Module) -> bool {
    m.body.iter().any(|s| matches!(s.kind, StmtKind::DeclareFunc { .. } | StmtKind::DeclareConst { .. }))
}

fn normalize(p: &Path) -> PathBuf {
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

struct Loader<'a> {
    root: &'a Path,
    modules: Vec<Source>,
    by_key: HashMap<String, usize>,
    in_progress: Vec<String>,
    sources: SourceMap,
    diags: Vec<Diagnostic>,
}

impl<'a> Loader<'a> {
    fn rel_display(&self, p: &Path) -> String {
        p.strip_prefix(self.root).unwrap_or(p).to_string_lossy().replace('\\', "/")
    }

    fn load_std(&mut self, name: &str) -> Option<usize> {
        if let Some(&id) = self.by_key.get(name) {
            return Some(id);
        }
        let text = STD_MODULES.iter().find(|(n, _)| *n == name).map(|(_, t)| *t)?;
        let path = format!("{}.clsh", name);
        let module = match crate::parser::parse_module(&path, text) {
            Ok(m) => m,
            Err(d) => {
                self.diags.push(Diagnostic::error(&path, d.span, d.msg));
                return None;
            }
        };
        self.sources.files.insert(path.clone(), text.to_string());
        let id = self.modules.len();
        self.modules.push(Source { key: name.to_string(), display: path, module, deps: HashMap::new(), is_std: true, impl_id: None });
        self.by_key.insert(name.to_string(), id);
        Some(id)
    }

    fn resolve_path(&self, from: &Path, spec: &str) -> Result<PathBuf, String> {
        if !(spec.starts_with("./") || spec.starts_with("../")) {
            return Err(format!("cannot resolve import '{}': use a relative path ('./x.cls') or a 'std/...' module", spec));
        }
        let dir = from.parent().unwrap_or(self.root);
        let base = normalize(&dir.join(spec));
        let candidates: Vec<PathBuf> = if base.extension().map_or(false, |e| e == "cls" || e == "clsh") {
            vec![base.clone()]
        } else {
            // a header (.clsh) is the public interface of a module; prefer it over the implementation
            let mut a = base.clone().into_os_string();
            a.push(".clsh");
            let mut b = base.clone().into_os_string();
            b.push(".cls");
            vec![PathBuf::from(a), PathBuf::from(b)]
        };
        for c in &candidates {
            if c.is_file() {
                let canon = c.canonicalize().map_err(|e| e.to_string())?;
                if !canon.starts_with(self.root) {
                    return Err(format!("import '{}' escapes the project directory", spec));
                }
                return Ok(canon);
            }
        }
        Err(format!("cannot find module '{}' (looked for {})", spec, candidates.iter().map(|c| self.rel_display(c)).collect::<Vec<_>>().join(", ")))
    }

    fn visit_file(&mut self, path: &Path, importer: Option<(&str, Span)>) -> Option<usize> {
        let key = path.to_string_lossy().to_string();
        if let Some(&id) = self.by_key.get(&key) {
            return Some(id);
        }
        let display = self.rel_display(path);
        if self.in_progress.contains(&key) {
            let chain: Vec<String> = self.in_progress.iter().skip_while(|k| **k != key).map(|k| self.rel_display(Path::new(k))).collect();
            let (file, span) = importer.unwrap_or((&display, Span::default()));
            self.diags.push(Diagnostic::error(file, span, format!("import cycle: {} -> {}", chain.join(" -> "), display)));
            return None;
        }
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                let (file, span) = importer.unwrap_or((&display, Span::default()));
                self.diags.push(Diagnostic::error(file, span, format!("cannot read {}: {}", display, e)));
                return None;
            }
        };
        self.sources.files.insert(display.clone(), text.clone());
        let module = match crate::parser::parse_module(&display, &text) {
            Ok(m) => m,
            Err(d) => {
                self.diags.push(Diagnostic::error(&display, d.span, d.msg));
                return None;
            }
        };
        self.in_progress.push(key.clone());
        let mut deps = HashMap::new();
        let mut failed = false;
        let specs: Vec<(String, Span)> = module
            .body
            .iter()
            .filter_map(|s| match &s.kind {
                StmtKind::Import { from, .. } => Some((from.clone(), s.span)),
                _ => None,
            })
            .collect();
        for (spec, span) in specs {
            if deps.contains_key(&spec) {
                continue;
            }
            let dep = if spec.starts_with("std/") {
                match self.load_std(&spec) {
                    Some(id) => Some(id),
                    None => {
                        if !self.diags.iter().any(|d| d.file == display && d.span == span) {
                            self.diags.push(Diagnostic::error(&display, span, format!("unknown standard module '{}'", spec)));
                        }
                        None
                    }
                }
            } else {
                match self.resolve_path(path, &spec) {
                    Ok(p) => self.visit_file(&p, Some((&display, span))),
                    Err(msg) => {
                        self.diags.push(Diagnostic::error(&display, span, msg));
                        None
                    }
                }
            };
            match dep {
                Some(id) => {
                    deps.insert(spec.clone(), id);
                    // A header that declares values needs its implementation (sibling .cls).
                    if !self.modules[id].is_std && self.modules[id].module.is_header && has_declares(&self.modules[id].module) && self.modules[id].impl_id.is_none() {
                        self.link_impl(id, &key, &display, span);
                    }
                }
                None => failed = true,
            }
        }
        self.in_progress.pop();
        if failed {
            return None;
        }
        let id = self.modules.len();
        let is_header = module.is_header;
        self.modules.push(Source { key: key.clone(), display, module, deps, is_std: false, impl_id: None });
        self.by_key.insert(key.clone(), id);
        let _ = is_header;
        Some(id)
    }

    fn link_impl(&mut self, header_id: usize, importer_key: &str, importer_display: &str, span: Span) {
        let hkey = self.modules[header_id].key.clone();
        let impl_path = PathBuf::from(hkey.trim_end_matches('h'));
        if impl_path.to_string_lossy() == importer_key {
            // implementation file importing its own header: link lazily
            return;
        }
        if !impl_path.is_file() {
            self.diags.push(Diagnostic::error(
                importer_display,
                span,
                format!("header '{}' declares functions but its implementation '{}' was not found", self.modules[header_id].display, self.rel_display(&impl_path)),
            ));
            return;
        }
        if let Some(id) = self.visit_file(&impl_path, Some((importer_display, span))) {
            self.modules[header_id].impl_id = Some(id);
        }
    }
}

pub fn load_program(project: &Project, roots: &[PathBuf]) -> Result<Loaded, (Vec<Diagnostic>, SourceMap)> {
    let mut l = Loader { root: &project.root, modules: Vec::new(), by_key: HashMap::new(), in_progress: Vec::new(), sources: SourceMap::default(), diags: Vec::new() };
    l.load_std("std/prelude");
    let mut root_ids = Vec::new();
    for r in roots {
        let canon = r.canonicalize().unwrap_or_else(|_| r.clone());
        if let Some(id) = l.visit_file(&canon, None) {
            root_ids.push(id);
        }
    }
    // second chance for implementation links skipped due to self-import
    for id in 0..l.modules.len() {
        let m = &l.modules[id];
        if !m.is_std && m.module.is_header && has_declares(&m.module) && m.impl_id.is_none() {
            let impl_path = PathBuf::from(m.key.trim_end_matches('h'));
            if let Some(&iid) = l.by_key.get(&impl_path.to_string_lossy().to_string()) {
                l.modules[id].impl_id = Some(iid);
            }
        }
    }
    if l.diags.iter().any(|d| d.severity == Severity::Error) {
        return Err((l.diags, l.sources));
    }
    Ok(Loaded { modules: l.modules, sources: l.sources, roots: root_ids })
}

/// Load a single in-memory module (for tests): stdlib + one anonymous module.
pub fn load_source(project: &Project, name: &str, text: &str) -> Result<Loaded, (Vec<Diagnostic>, SourceMap)> {
    let mut l = Loader { root: &project.root, modules: Vec::new(), by_key: HashMap::new(), in_progress: Vec::new(), sources: SourceMap::default(), diags: Vec::new() };
    l.load_std("std/prelude");
    let module = match crate::parser::parse_module(name, text) {
        Ok(m) => m,
        Err(d) => {
            l.diags.push(Diagnostic::error(name, d.span, d.msg));
            return Err((l.diags, l.sources));
        }
    };
    l.sources.files.insert(name.to_string(), text.to_string());
    let mut deps = HashMap::new();
    for s in &module.body {
        if let StmtKind::Import { from, .. } = &s.kind {
            if from.starts_with("std/") {
                match l.load_std(from) {
                    Some(id) => {
                        deps.insert(from.clone(), id);
                    }
                    None => l.diags.push(Diagnostic::error(name, s.span, format!("unknown standard module '{}'", from))),
                }
            } else {
                l.diags.push(Diagnostic::error(name, s.span, "only std imports are available in single-source mode"));
            }
        }
    }
    if !l.diags.is_empty() {
        return Err((l.diags, l.sources));
    }
    let id = l.modules.len();
    l.modules.push(Source { key: name.to_string(), display: name.to_string(), module, deps, is_std: false, impl_id: None });
    Ok(Loaded { modules: l.modules, sources: l.sources, roots: vec![id] })
}

pub fn discover_cls_files(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if p.is_dir() {
                if name.starts_with('.') || name == "dist" || name == "target" || name == "node_modules" {
                    continue;
                }
                walk(&p, out);
            } else if p.extension().map_or(false, |x| x == "cls") {
                out.push(p);
            }
        }
    }
    let mut v = Vec::new();
    walk(root, &mut v);
    v
}

#[allow(dead_code)]
pub fn rc_str(s: &str) -> Rc<str> {
    Rc::from(s)
}
