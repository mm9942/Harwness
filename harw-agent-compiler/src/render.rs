//! Rendering diagnostics with a source excerpt and a caret.
//!
//! ```text
//! error[HARW-BUILD-004]: `critic` admits `shell.exec`, which its base role `analyst` (generic worker) does not
//!   --> /home/u/.harw/agents/critic/definition.toml:14:3
//!    |
//! 14 |   "shell.exec",
//!    |   ^^^^^^^^^^^^
//!    = at: tools.admitted
//!    = help: a definition may only narrow the rights of the built-in role it extends
//! ```

use harw_agent_dsl::diagnostics::{Diagnostic, SourceFile, SourceSpan};

/// Renders one diagnostic; `files` supply the excerpt.
#[must_use]
pub fn render_diagnostic(diagnostic: &Diagnostic, files: &[SourceFile]) -> String {
    let mut out = format!(
        "{}[{}]: {}",
        diagnostic.severity, diagnostic.code, diagnostic.message
    );
    let mut gutter = 2;
    if let Some(span) = &diagnostic.span {
        out.push_str(&format!("\n  --> {span}"));
        if let Some(line) = source_line(files, span) {
            let number = span.line.to_string();
            gutter = number.len() + 1;
            let pad = " ".repeat(gutter);
            let column = usize::try_from(span.column.saturating_sub(1)).unwrap_or(0);
            let prefix: String = line.chars().take(column).map(|c| if c == '\t' { '\t' } else { ' ' }).collect();
            let width = token_width(&line, column);
            out.push_str(&format!("\n{pad}|"));
            out.push_str(&format!("\n{number} | {line}"));
            out.push_str(&format!("\n{pad}| {prefix}{}", "^".repeat(width)));
        }
    }
    let pad = " ".repeat(gutter);
    if let Some(path) = &diagnostic.path {
        out.push_str(&format!("\n{pad}= at: {path}"));
    }
    if let Some(help) = &diagnostic.help {
        out.push_str(&format!("\n{pad}= help: {help}"));
    }
    out
}

/// Renders every diagnostic, separated by blank lines, plus a summary line.
#[must_use]
pub fn render_diagnostics(diagnostics: &[Diagnostic], files: &[SourceFile]) -> String {
    let mut blocks: Vec<String> = diagnostics
        .iter()
        .map(|diagnostic| render_diagnostic(diagnostic, files))
        .collect();
    let errors = diagnostics.iter().filter(|d| d.is_error()).count();
    let warnings = diagnostics.len() - errors;
    if !diagnostics.is_empty() {
        blocks.push(format!("{errors} error(s), {warnings} warning(s)/note(s)"));
    }
    blocks.join("\n\n")
}

/// The line of `span` from the matching source file (or the file on disk).
fn source_line(files: &[SourceFile], span: &SourceSpan) -> Option<String> {
    let index = usize::try_from(span.line.checked_sub(1)?).ok()?;
    let from_files = files
        .iter()
        .find(|file| file.label() == span.file)
        .and_then(|file| file.text.lines().nth(index).map(str::to_owned));
    from_files.or_else(|| {
        std::fs::read_to_string(&span.file)
            .ok()
            .and_then(|text| text.lines().nth(index).map(str::to_owned))
    })
}

/// Width of the token starting at `column` (characters): a quoted string up
/// to its closing quote, else up to whitespace, `,`, `]` or `}`; at least 1.
fn token_width(line: &str, column: usize) -> usize {
    let rest: Vec<char> = line.chars().skip(column).collect();
    let width = match rest.first() {
        Some('"') => {
            let mut escaped = false;
            let mut end = None;
            for (position, c) in rest.iter().enumerate().skip(1) {
                if escaped {
                    escaped = false;
                } else if *c == '\\' {
                    escaped = true;
                } else if *c == '"' {
                    end = Some(position + 1);
                    break;
                }
            }
            end.unwrap_or(rest.len())
        }
        Some(_) => rest
            .iter()
            .take_while(|c| !c.is_whitespace() && !matches!(c, ',' | ']' | '}'))
            .count(),
        None => 1,
    };
    width.max(1)
}

#[cfg(test)]
mod tests {
    use harw_agent_dsl::diagnostics::codes;
    use harw_agent_dsl::layers::DefinitionLayer;

    use super::*;

    #[test]
    fn test_render_shows_file_line_caret_and_help() {
        let text = "schema = \"harwness.agent/v1\"\n[tools]\nadmitted = [\"fs.read\", \"fs read\"]\n";
        let file = SourceFile::new(DefinitionLayer::UserGlobal, "agents/x/definition.toml", text);
        let line = "admitted = [\"fs.read\", \"fs read\"]";
        let column = line.find("\"fs read\"").unwrap_or_default();
        let diagnostic = Diagnostic::new(&codes::TOOL_INVALID_NAME, "invalid tool name `fs read`")
            .with_path("tools.admitted[1]")
            .with_span(Some(SourceSpan {
                file: "agents/x/definition.toml".to_owned(),
                line: 3,
                column: u32::try_from(column + 1).unwrap_or_default(),
            }));
        let rendered = render_diagnostic(&diagnostic, &[file]);
        let expected = format!(
            "error[HARW-TOOL-003]: invalid tool name `fs read`\n  --> agents/x/definition.toml:3:{}\n  |\n3 | {line}\n  | {}{}\n  = at: tools.admitted[1]\n  = help: tool names are non-empty and contain no whitespace or control characters",
            column + 1,
            " ".repeat(column),
            "^".repeat("\"fs read\"".len())
        );
        assert_eq!(rendered, expected);
    }

    #[test]
    fn test_token_width() {
        assert_eq!(token_width("x = \"a\\\"b\",", 4), 6);
        assert_eq!(token_width("max_depth = 12,", 12), 2);
        assert_eq!(token_width("", 0), 1);
    }
}
