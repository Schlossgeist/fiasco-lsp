//! Standalone mode: preprocess Fiasco sources in-process instead of relying on
//! the output of the external `preprocess` tool.
//!
//! Fiasco's build system is still used to find out *what* to preprocess: it is
//! run with `PREPROCESS` pointing to this binary (see `preprocess::cli`), which
//! records every invocation (module, flags, tags, input files) and generates
//! the initial files in `auto/`, so `compile_commands.json` can be created.
//! Afterwards, each edit of a source file regenerates its module in memory.
//! Generated files are kept identical on disk, so clangd also sees up-to-date
//! versions of generated headers that are not open.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use color_eyre::eyre::{eyre, Result, WrapErr};
use lsp_types::{Position, PositionEncodingKind, TextDocumentContentChangeEvent};
use ropey::Rope;
use tempfile::TempDir;

use crate::preprocess::cli::{parse_args, Invocation, RecordedJob, JOB_DIR_ENV};
use crate::preprocess::generator::{self, Diagnostic};
use crate::preprocess::SourceFile;
use crate::source_mapping::{FiascoSourceMapping, GeneratedRange};

/// One preprocess invocation: a Fiasco module.
pub struct Job {
    pub cwd: PathBuf,
    pub invocation: Invocation,
    /// Canonical paths of the input files.
    pub inputs: Vec<PathBuf>,
    /// Absolute paths of the outputs of the last generation.
    pub outputs: Vec<PathBuf>,
}

/// Result of (re)generating a job.
pub struct Regenerated {
    pub job: usize,
    /// Generated files whose content changed, with the new content.
    pub changed: Vec<PathBuf>,
    pub diagnostics: Vec<Diagnostic>,
}

pub struct Standalone {
    pub jobs: Vec<Job>,
    job_of_source: HashMap<PathBuf, usize>,
    /// Editor contents of open source files.
    overlays: HashMap<PathBuf, Rope>,
    /// Current content of all generated files.
    generated: HashMap<PathBuf, String>,
    /// Preprocess diagnostics per source file.
    pub diagnostics: HashMap<PathBuf, Vec<Diagnostic>>,
    _temp: TempDir,
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_owned())
}

impl Standalone {
    /// Run Fiasco's build system in `build_dir` with this binary as
    /// preprocessor, creating `.Modules.deps`, the generated sources and
    /// `compile_commands.json`, and record all preprocess jobs.
    pub fn from_build(build_dir: &Path) -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let exe = std::env::current_exe()?;
        let wrapper = temp.path().join("preprocess");
        let quoted = exe.to_string_lossy().replace('\'', r"'\''");
        fs::write(&wrapper, format!("#!/bin/sh\nexec '{quoted}' preprocess \"$@\"\n"))?;
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755))?;
        let job_dir = temp.path().join("jobs");

        // Force all modules to be preprocessed (and thereby recorded).
        if let Ok(entries) = fs::read_dir(build_dir.join("auto")) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("stamp-") && name.ends_with(".ready") {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
        let mut preprocess_arg = std::ffi::OsString::from("PREPROCESS=");
        preprocess_arg.push(&wrapper);
        let output = Command::new("make")
            .arg(format!("-j{}", std::thread::available_parallelism().map_or(1, |n| n.get())))
            .arg(preprocess_arg)
            .args([".Modules.deps", "compile_commands.json"])
            .current_dir(build_dir)
            .env(JOB_DIR_ENV, &job_dir)
            .output()
            .wrap_err("Unable to run make")?;
        if !output.status.success() {
            return Err(eyre!(
                "make failed:\n{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        let mut jobs = Vec::new();
        for entry in fs::read_dir(&job_dir).wrap_err("No preprocess jobs were recorded")? {
            let path = entry?.path();
            let job: RecordedJob = serde_json::from_slice(&fs::read(&path)?)?;
            jobs.push(Job::new(job)?);
        }
        jobs.sort_by(|a, b| a.inputs.cmp(&b.inputs));
        info!("Recorded {} preprocess jobs", jobs.len());
        Ok(Self::new(jobs, temp))
    }

    fn new(jobs: Vec<Job>, temp: TempDir) -> Self {
        let mut job_of_source = HashMap::new();
        for (index, job) in jobs.iter().enumerate() {
            for input in &job.inputs {
                job_of_source.insert(input.clone(), index);
            }
        }
        Self {
            jobs,
            job_of_source,
            overlays: HashMap::new(),
            generated: HashMap::new(),
            diagnostics: HashMap::new(),
            _temp: temp,
        }
    }

    /// The job a source file belongs to.
    pub fn job_of(&self, path: &Path) -> Option<usize> {
        self.job_of_source.get(path).or_else(|| self.job_of_source.get(&canonical(path))).copied()
    }

    /// Canonical source path as used in the mapping.
    pub fn source_path(&self, path: &Path) -> Option<PathBuf> {
        if self.job_of_source.contains_key(path) {
            return Some(path.to_owned());
        }
        let path = canonical(path);
        self.job_of_source.contains_key(&path).then_some(path)
    }

    pub fn generated_text(&self, path: &Path) -> Option<&str> {
        self.generated.get(path).map(String::as_str)
    }

    pub fn is_generated(&self, path: &Path) -> bool {
        self.generated.contains_key(path)
    }

    /// Generate all jobs and build the complete source mapping.
    pub fn generate_all(&mut self) -> Result<FiascoSourceMapping> {
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
        let indices: Vec<usize> = (0..self.jobs.len()).collect();
        let chunk_size = indices.len().div_ceil(threads).max(1);
        let this = &*self;
        let results: Vec<_> = std::thread::scope(|scope| {
            let handles: Vec<_> = indices
                .chunks(chunk_size)
                .map(|chunk| scope.spawn(move || chunk.iter().map(|&j| (j, this.run(j))).collect::<Vec<_>>()))
                .collect();
            handles.into_iter().flat_map(|h| h.join().expect("generator thread panicked")).collect()
        });
        let mut ranges = Vec::new();
        for (job, result) in results {
            match result {
                Ok(output) => {
                    let (job_ranges, _) = self.store(job, output)?;
                    ranges.extend(job_ranges);
                }
                Err(e) => error!("Preprocessing {:?} failed: {e:?}", self.jobs[job].inputs),
            }
        }
        FiascoSourceMapping::from_generated_ranges(ranges).map_err(|e| eyre!(e))
    }

    fn source_text(&self, path: &Path) -> Result<String> {
        match self.overlays.get(path) {
            Some(rope) => Ok(rope.to_string()),
            None => fs::read_to_string(path).wrap_err_with(|| format!("Cannot read {}", path.display())),
        }
    }

    fn run(&self, job: usize) -> Result<generator::Generated> {
        let job = &self.jobs[job];
        let files = job
            .inputs
            .iter()
            .map(|path| SourceFile::new(path.clone(), &self.source_text(path)?))
            .collect::<Result<Vec<_>>>()?;
        Ok(generator::generate(&files, &job.options()))
    }

    /// Store a generation result: update contents on disk and in memory.
    fn store(
        &mut self,
        job: usize,
        output: generator::Generated,
    ) -> Result<(Vec<GeneratedRange>, Vec<PathBuf>)> {
        let mut ranges = Vec::new();
        let mut changed = Vec::new();
        let mut outputs = Vec::new();
        for out in output.outputs {
            let path = out.document.path.clone();
            let text = if out.truncated { String::new() } else { out.document.text };
            outputs.push(path.clone());
            ranges.extend(out.document.mappings);
            if self.generated.get(&path) != Some(&text) {
                if fs::read_to_string(&path).ok().as_ref() != Some(&text) {
                    fs::write(&path, &text).wrap_err_with(|| format!("Cannot write {}", path.display()))?;
                }
                changed.push(path.clone());
                self.generated.insert(path, text);
            }
        }
        for input in &self.jobs[job].inputs {
            self.diagnostics.insert(input.clone(), Vec::new());
        }
        for diagnostic in output.diagnostics {
            self.diagnostics.entry(diagnostic.file.clone()).or_default().push(diagnostic);
        }
        self.jobs[job].outputs = outputs;
        Ok((ranges, changed))
    }

    /// Regenerate a job and update `mapping` accordingly.
    pub fn regenerate(&mut self, job: usize, mapping: &mut FiascoSourceMapping) -> Result<Regenerated> {
        let output = self.run(job)?;
        let diagnostics = output.diagnostics.clone();
        let previous_outputs = self.jobs[job].outputs.clone();
        let (ranges, changed) = self.store(job, output)?;
        let mut generated = previous_outputs;
        generated.extend(self.jobs[job].outputs.iter().cloned());
        mapping
            .replace_generated_ranges(&self.jobs[job].inputs, &generated, ranges)
            .map_err(|e| eyre!(e))?;
        Ok(Regenerated { job, changed, diagnostics })
    }

    pub fn open(&mut self, path: &Path, text: &str) {
        self.overlays.insert(path.to_owned(), Rope::from_str(text));
    }

    pub fn close(&mut self, path: &Path) {
        self.overlays.remove(path);
    }

    /// Apply editor changes to an open source file.
    pub fn change(
        &mut self,
        path: &Path,
        changes: Vec<TextDocumentContentChangeEvent>,
        encoding: &PositionEncodingKind,
    ) -> Result<()> {
        let rope = self.overlays.get_mut(path).ok_or_else(|| eyre!("{} is not open", path.display()))?;
        for change in changes {
            match change.range {
                None => *rope = Rope::from_str(&change.text),
                Some(range) => {
                    let start = char_index(rope, range.start, encoding)?;
                    let end = char_index(rope, range.end, encoding)?;
                    rope.remove(start..end.max(start));
                    rope.insert(start, &change.text);
                }
            }
        }
        Ok(())
    }
}

/// Convert an LSP position into a char index of `rope`.
fn char_index(rope: &Rope, position: Position, encoding: &PositionEncodingKind) -> Result<usize> {
    let line = position.line as usize;
    if line >= rope.len_lines() {
        return Ok(rope.len_chars());
    }
    let line_start = rope.line_to_char(line);
    let line_text = rope.line(line);
    let character = position.character as usize;
    let offset = if *encoding == PositionEncodingKind::UTF8 {
        line_text.byte_to_char(character.min(line_text.len_bytes()))
    } else if *encoding == PositionEncodingKind::UTF32 {
        character.min(line_text.len_chars())
    } else {
        line_text.utf16_cu_to_char(character.min(line_text.len_utf16_cu()))
    };
    Ok(line_start + offset)
}

impl Job {
    fn new(recorded: RecordedJob) -> Result<Self> {
        let invocation = parse_args(&recorded.args)?;
        let inputs = invocation.inputs.iter().map(|p| canonical(&recorded.cwd.join(p))).collect();
        Ok(Self { cwd: recorded.cwd, invocation, inputs, outputs: Vec::new() })
    }

    /// Options for in-process generation. Fiasco generates into `auto/new_*`
    /// and renames afterwards; generate directly to the final absolute paths.
    fn options(&self) -> generator::Options {
        let mut options = self.invocation.options.clone();
        let prefix = options.prefix.strip_suffix("new_").unwrap_or(&options.prefix);
        options.prefix = format!("{}/{prefix}", self.cwd.display());
        if let Some(out) = &options.out_base {
            options.out_base = Some(format!("{}/{out}", self.cwd.display()));
        }
        options
    }
}

/// Files that must be opened in the language server for a source file: all
/// outputs of its job that contain lines of it.
pub fn mapped_outputs(mapping: &FiascoSourceMapping, source: &Path) -> HashSet<PathBuf> {
    use crate::source_mapping::MapDirection::ToPreprocess;
    mapping.map_files(ToPreprocess, &source.to_string_lossy()).iter().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::Range;

    #[test]
    fn applies_incremental_utf16_changes() {
        let mut standalone = Standalone::new(Vec::new(), tempfile::tempdir().unwrap());
        let path = Path::new("/x.cpp");
        standalone.open(path, "a😀b\nline two\n");
        let change = |l1, c1, l2, c2, text: &str| TextDocumentContentChangeEvent {
            range: Some(Range::new(Position::new(l1, c1), Position::new(l2, c2))),
            range_length: None,
            text: text.into(),
        };
        // The emoji is two UTF-16 code units.
        standalone.change(path, vec![change(0, 3, 0, 4, "B"), change(1, 0, 1, 4, "LINE")], &PositionEncodingKind::UTF16).unwrap();
        assert_eq!(standalone.source_text(path).unwrap(), "a😀B\nLINE two\n");
        standalone.change(path, vec![change(0, 5, 0, 6, "b")], &PositionEncodingKind::UTF8).unwrap();
        assert_eq!(standalone.source_text(path).unwrap(), "a😀b\nLINE two\n");
    }

    #[test]
    fn regenerates_jobs_and_updates_mapping() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().to_owned();
        fs::create_dir(cwd.join("auto")).unwrap();
        let source = cwd.join("foo.cpp");
        fs::write(&source, "INTERFACE:\nclass Foo {};\nIMPLEMENTATION:\nPUBLIC int Foo::get() { return 1; }\n").unwrap();
        let recorded = RecordedJob {
            cwd: cwd.clone(),
            args: ["-p", "auto/new_", "-s", "-h", "foo", "-c", "foo", "foo.cpp"].map(String::from).to_vec(),
        };
        let mut standalone = Standalone::new(vec![Job::new(recorded).unwrap()], tempfile::tempdir().unwrap());
        let mut mapping = standalone.generate_all().unwrap();
        let source = canonical(&source);
        let header = canonical(&cwd).join("auto/foo.h");
        let text = fs::read_to_string(&header).unwrap();
        assert!(text.contains("       int      get();"), "{text}");
        assert_eq!(mapped_outputs(&mapping, &source).len(), 2);

        // Insert a line: mapping and generated files follow.
        standalone.open(&source, &fs::read_to_string(&source).unwrap());
        let insert = TextDocumentContentChangeEvent {
            range: Some(lsp_types::Range::new(Position::new(3, 0), Position::new(3, 0))),
            range_length: None,
            text: "PUBLIC void Foo::set() {}\n".into(),
        };
        standalone.change(&source, vec![insert], &PositionEncodingKind::UTF16).unwrap();
        let job = standalone.job_of(&source).unwrap();
        let result = standalone.regenerate(job, &mut mapping).unwrap();
        assert!(result.changed.contains(&header));
        let text = standalone.generated_text(&header).unwrap();
        assert!(text.contains("       void      set();") && text.contains("       int      get();"), "{text}");
        use crate::source_mapping::MapDirection::*;
        let target = mapping.map(ToPreprocess, &source.to_string_lossy(), 4, 16).unwrap();
        let line = standalone.generated_text(&target.path).unwrap().lines().nth(target.line as usize).unwrap();
        assert_eq!(&line[16..19], "get");
    }
}
