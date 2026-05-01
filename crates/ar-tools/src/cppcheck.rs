//! cppcheck runner. Parses cppcheck's `--template` text output for
//! C/C++ static analysis (memory leaks, null derefs, undefined
//! behaviour, style/portability issues).
//!
//! Routes on `.c` / `.cc` / `.cpp` / `.cxx` / `.c++` / `.h` / `.hpp` /
//! `.hxx`. cppcheck doesn't have a stable JSON reporter, but its
//! `--template` flag accepts a printf-style format string we can
//! shape into a parser-friendly text line.
//!
//! Format we request:
//! `{file}:{line}:{column}:{severity}:{id}:{message}`
//!
//! Severity tokens map: error → Error, warning/portability/performance
//! → Warning, style/information → Note.

use crate::finding::{Finding, Severity};
use crate::runner::{run_in_sandbox, LinterRunner, RunnerError};
use ar_sandbox::Sandbox;
use async_trait::async_trait;
use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;

const TOOL: &str = "cppcheck";
const TEMPLATE: &str = "{file}:{line}:{column}:{severity}:{id}:{message}";

fn line_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // The first 5 colons separate fixed-shape fields; the
        // message can itself contain colons, so anchor the message
        // group on the rest of the line.
        Regex::new(
            r"^(?P<path>[^:]+):(?P<line>\d+):(?P<col>\d+):(?P<sev>\w+):(?P<id>[^:]+):(?P<msg>.+)$",
        )
        .expect("cppcheck template regex compiles")
    })
}

pub fn parse_cppcheck_output(text: &str) -> Result<Vec<Finding>, RunnerError> {
    let re = line_regex();
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim_end();
        if line.is_empty() {
            continue;
        }
        let Some(caps) = re.captures(line) else {
            continue; // banner / progress / summary
        };
        let path = caps["path"].to_string();
        let line_no: u32 = caps["line"].parse().unwrap_or(0).max(1);
        let id = caps["id"].to_string();
        let message = caps["msg"].to_string();
        let severity = severity_from(&caps["sev"]);
        out.push(Finding {
            source_tool: TOOL.into(),
            rule_id: Some(id),
            path,
            line_start: line_no,
            line_end: line_no,
            severity,
            message,
        });
    }
    Ok(out)
}

fn severity_from(level: &str) -> Severity {
    match level.to_ascii_lowercase().as_str() {
        "error" => Severity::Error,
        "warning" | "portability" | "performance" => Severity::Warning,
        _ => Severity::Note,
    }
}

pub struct CppcheckRunner {
    /// Files to scan, repo-relative. Empty means "scan no files".
    pub files: Vec<String>,
}

#[async_trait]
impl LinterRunner for CppcheckRunner {
    fn name(&self) -> &str {
        TOOL
    }

    async fn run(
        &self,
        sandbox: &dyn Sandbox,
        repo_dir: &Path,
    ) -> Result<Vec<Finding>, RunnerError> {
        if self.files.is_empty() {
            return Ok(vec![]);
        }
        // -q             quiet (no progress chatter)
        // --enable=all   include style/portability/performance/info
        // --inline-suppr respect /*cppcheck-suppress*/ in source
        // --template     pin the output shape we parse
        // --error-exitcode=0  read findings from stdout cleanly
        let mut args = vec![
            "-q".into(),
            "--enable=all".into(),
            "--inline-suppr".into(),
            format!("--template={TEMPLATE}"),
            "--error-exitcode=0".into(),
        ];
        args.extend(self.files.iter().cloned());
        let output = run_in_sandbox(sandbox, repo_dir, "cppcheck", args, vec![]).await?;
        // cppcheck writes findings to stderr by convention, but
        // --output-file=- lets us read both. We read both streams
        // and concatenate so we don't miss anything.
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        if combined.trim().is_empty() {
            return Ok(vec![]);
        }
        parse_cppcheck_output(&combined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_cppcheck_output() {
        let text = "\
src/main.cpp:42:5:error:nullPointer:Null pointer dereference: ptr
src/buf.c:7:1:warning:uninitvar:Uninitialized variable: x
src/util.cpp:1:1:style:unusedFunction:The function 'helper' is never used.
include/lib.h:3:1:performance:passedByValue:Function parameter 'config' should be passed by const reference.
";
        let f = parse_cppcheck_output(text).expect("ok");
        assert_eq!(f.len(), 4);
        assert_eq!(f[0].path, "src/main.cpp");
        assert_eq!(f[0].line_start, 42);
        assert_eq!(f[0].rule_id.as_deref(), Some("nullPointer"));
        assert_eq!(f[0].severity, Severity::Error);
        assert!(f[0].message.contains("Null pointer"));
        assert_eq!(f[1].severity, Severity::Warning); // warning
        assert_eq!(f[2].severity, Severity::Note); // style
        assert_eq!(f[3].severity, Severity::Warning); // performance
    }

    #[test]
    fn message_with_colons_is_preserved() {
        let text = "x.c:1:1:error:e:Foo: bar: baz\n";
        let f = parse_cppcheck_output(text).expect("ok");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].message, "Foo: bar: baz");
    }

    #[test]
    fn empty_input_yields_zero_findings() {
        let f = parse_cppcheck_output("").expect("ok");
        assert!(f.is_empty());
    }

    #[test]
    fn banner_lines_are_skipped() {
        let text = "\
Checking src/main.cpp ...
src/main.cpp:1:1:error:syntax:bad
done.
";
        let f = parse_cppcheck_output(text).expect("ok");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].path, "src/main.cpp");
    }

    #[test]
    fn unknown_severity_falls_back_to_note() {
        let text = "x.c:1:1:debug:d:m\n";
        let f = parse_cppcheck_output(text).expect("ok");
        assert_eq!(f[0].severity, Severity::Note);
    }

    #[test]
    fn line_zero_coerces_to_one() {
        let text = "x.c:0:1:error:e:m\n";
        let f = parse_cppcheck_output(text).expect("ok");
        assert_eq!(f[0].line_start, 1);
    }
}
