//! Command line interface compatible with Fiasco's `preprocess` tool:
//! `fiasco-lsp preprocess [-dilLstvwW] -c base [-C ext] [-e tags] [-h head]
//! [-H ext] [-o base] [-p prefix] files...`
//!
//! When the environment variable `FIASCO_LSP_JOB_DIR` is set, each invocation
//! additionally records its working directory and arguments there, so the
//! language server can later re-run the same job in memory. In that mode no
//! `#line` directives are generated (the files on disk must match what the
//! language server keeps in memory) and errors do not fail the build.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{eyre, Result};
use serde::{Deserialize, Serialize};

use super::generator::{generate_from_paths, Options, OutputKind, Severity};

pub const JOB_DIR_ENV: &str = "FIASCO_LSP_JOB_DIR";

#[derive(Debug, Clone, Default)]
pub struct Invocation {
    pub options: Options,
    pub inputs: Vec<PathBuf>,
    /// `-l`: no `#line` directives at all.
    pub no_line_directives: bool,
    /// `-L`: no `#line` directives in headers.
    pub no_header_line_directives: bool,
    /// `-W`: warnings are errors.
    pub warnings_are_errors: bool,
}

/// A recorded preprocess invocation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecordedJob {
    pub cwd: PathBuf,
    pub args: Vec<String>,
}

/// Parse arguments with the semantics of Perl's `getopts('e:o:c:p:h:H:C:ivlLsdwtW')`.
pub fn parse_args(args: &[String]) -> Result<Invocation> {
    let mut invocation = Invocation::default();
    let options = &mut invocation.options;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            index += 1;
            break;
        }
        let Some(flags) = arg.strip_prefix('-').filter(|f| !f.is_empty()) else { break };
        index += 1;
        for (offset, flag) in flags.char_indices() {
            if "eocphHC".contains(flag) {
                let rest = &flags[offset + flag.len_utf8()..];
                let value = if !rest.is_empty() {
                    rest.to_owned()
                } else {
                    index += 1;
                    args.get(index - 1).cloned().ok_or_else(|| eyre!("option -{flag} needs a value"))?
                };
                match flag {
                    'e' => {
                        options.tags = Some(value.split_whitespace().map(str::to_owned).collect::<HashSet<_>>())
                    }
                    'o' => options.out_base = Some(value),
                    'c' => options.base = value,
                    'p' => options.prefix = value,
                    'h' => options.public_header = Some(value),
                    'H' => options.header_ext = value,
                    'C' => options.source_ext = value,
                    _ => unreachable!(),
                }
                break;
            }
            match flag {
                'i' => options.inlines = true,
                'l' => invocation.no_line_directives = true,
                'L' => invocation.no_header_line_directives = true,
                's' => options.single_source = true,
                't' => options.truncate_empty = true,
                'w' => options.warn_needs = true,
                'W' => invocation.warnings_are_errors = true,
                'v' | 'd' => {}
                _ => return Err(eyre!("unknown option -{flag}")),
            }
        }
    }
    // An empty -e list still enables explicit section selection.
    if options.tags.as_ref().is_some_and(|t| t.is_empty())
        && !args.iter().any(|a| a.starts_with("-e"))
    {
        options.tags = None;
    }
    if options.base.is_empty() {
        return Err(eyre!("Need to specify option -c Classfile_basename"));
    }
    invocation.inputs = args[index..].iter().map(PathBuf::from).collect();
    Ok(invocation)
}

fn record(job_dir: &Path, args: &[String], invocation: &Invocation) -> Result<()> {
    let job = RecordedJob { cwd: std::env::current_dir()?, args: args.to_vec() };
    let name = invocation.options.public_header.as_deref().unwrap_or(&invocation.options.base);
    let name: String =
        name.chars().map(|c| if c.is_alphanumeric() || "-_.".contains(c) { c } else { '_' }).collect();
    fs::create_dir_all(job_dir)?;
    fs::write(job_dir.join(format!("{name}.json")), serde_json::to_vec_pretty(&job)?)?;
    Ok(())
}

/// Entry point of `fiasco-lsp preprocess ...`; returns the process exit code.
pub fn main(args: &[String]) -> Result<i32> {
    let invocation = parse_args(args)?;
    let job_dir = std::env::var_os(JOB_DIR_ENV).map(PathBuf::from);
    if let Some(dir) = &job_dir {
        record(dir, args, &invocation)?;
    }
    let generated = generate_from_paths(&invocation.inputs, &invocation.options)?;

    let mut errors = 0;
    let mut warnings = 0;
    for d in &generated.diagnostics {
        let kind = match d.severity {
            Severity::Error => {
                errors += 1;
                "error"
            }
            Severity::Warning => {
                warnings += 1;
                "warning"
            }
        };
        eprintln!("{}:{}: {kind}: {}", d.file.display(), d.block_line + 1, d.message);
    }
    if errors > 0 && job_dir.is_none() {
        return Ok(128);
    }

    for output in &generated.outputs {
        let header = output.kind != OutputKind::Implementation;
        let line_directives = job_dir.is_none()
            && !invocation.no_line_directives
            && !(header && invocation.no_header_line_directives);
        let text = if output.truncated {
            String::new()
        } else if line_directives {
            output.document.render_with_line_directives()
        } else {
            output.document.text.clone()
        };
        let path = &output.document.path;
        fs::write(path, text).map_err(|e| eyre!("Cannot open {} for writing: {e}", path.display()))?;
    }

    if warnings > 0 && job_dir.is_none() {
        eprintln!("warning: found {warnings} warning(s)");
        if invocation.warnings_are_errors {
            return Ok(128);
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_fiasco_command_line() {
        let invocation = parse_args(&args(&[
            "-i", "-t", "-p", "auto/new_", "-e", "ia32 amd64", "-s", "-h", "thread", "-c", "thread",
            "a.cpp", "b-x.cpp",
        ]))
        .unwrap();
        let o = &invocation.options;
        assert!(o.inlines && o.truncate_empty && o.single_source);
        assert_eq!(o.prefix, "auto/new_");
        assert_eq!(o.public_header.as_deref(), Some("thread"));
        assert_eq!(o.tags.as_ref().unwrap().len(), 2);
        assert_eq!(invocation.inputs, [PathBuf::from("a.cpp"), PathBuf::from("b-x.cpp")]);

        let bundled = parse_args(&args(&["-il", "-cfoo", "x.cpp"])).unwrap();
        assert!(bundled.options.inlines && bundled.no_line_directives);
        assert_eq!(bundled.options.base, "foo");
        assert!(parse_args(&args(&["x.cpp"])).is_err());
    }
}
