use crate::lexer::Span;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub file: String,
    pub span: Span,
    pub msg: String,
    pub severity: Severity,
}

impl Diagnostic {
    pub fn error(file: &str, span: Span, msg: impl Into<String>) -> Diagnostic {
        Diagnostic { file: file.to_string(), span, msg: msg.into(), severity: Severity::Error }
    }
    pub fn warning(file: &str, span: Span, msg: impl Into<String>) -> Diagnostic {
        Diagnostic { file: file.to_string(), span, msg: msg.into(), severity: Severity::Warning }
    }
}

#[derive(Default)]
pub struct SourceMap {
    pub files: HashMap<String, String>,
}

pub fn render(d: &Diagnostic, sources: &SourceMap) -> String {
    let kind = match d.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
    };
    let mut out = format!("{}: {}\n  --> {}:{}:{}\n", kind, d.msg, d.file, d.span.line, d.span.col);
    if let Some(src) = sources.files.get(&d.file) {
        if d.span.line >= 1 {
            if let Some(line) = src.lines().nth(d.span.line as usize - 1) {
                let num = d.span.line.to_string();
                out.push_str(&format!("{} | {}\n", num, line));
                let pad: String = line.chars().take(d.span.col.saturating_sub(1) as usize).map(|c| if c == '\t' { '\t' } else { ' ' }).collect();
                out.push_str(&format!("{} | {}^\n", " ".repeat(num.len()), pad));
            }
        }
    }
    out
}
