use std::cmp::max;
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::File;
use std::io::prelude::*;
use std::io::BufReader;
use std::iter::Iterator;
use std::path::{Path, PathBuf};
use std::{fs, io};

use lazy_static::lazy_static;
use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PreprocessSection {
    None,
    Interface,
    Implementation,
    /// Member/function declarations synthesized from a definition's head.
    /// Lowest priority, so source positions prefer the definition.
    Declaration,
}

const SECTION_PRIORITY: [PreprocessSection; 4] = [
    PreprocessSection::Implementation,
    PreprocessSection::Interface,
    PreprocessSection::None,
    PreprocessSection::Declaration,
];

#[derive(Debug, Serialize, Deserialize)]
pub struct LineMapping {
    section: PreprocessSection,
    src_line: u32,
    src_end_line: u32, // Exclusive
    dst_file: PathBuf,
    dst_line: u32,
}

impl LineMapping {
    fn contains(&self, line: u32) -> bool {
        line >= self.src_line && line <= self.src_end_line
    }

    fn overlaps(&self, start: u32, end: u32) -> bool {
        // x1 <= y2 && y1 <= x2
        start <= self.src_end_line && self.src_line <= end
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FileLineMappings {
    files: Vec<PathBuf>,
    none: Vec<LineMapping>,
    interface: Vec<LineMapping>,
    implementation: Vec<LineMapping>,
    #[serde(default)]
    declaration: Vec<LineMapping>,
    length: u32,
}

impl FileLineMappings {
    fn new() -> FileLineMappings {
        FileLineMappings {
            files: Vec::new(),
            none: Vec::new(),
            interface: Vec::new(),
            implementation: Vec::new(),
            declaration: Vec::new(),
            length: 0,
        }
    }

    fn from_mappings(mappings: Vec<LineMapping>) -> FileLineMappings {
        let mut m = Self::new();
        for mapping in mappings {
            m.push(mapping)
        }
        m
    }

    fn access(&mut self, section: PreprocessSection) -> &mut Vec<LineMapping> {
        match section {
            PreprocessSection::None => &mut self.none,
            PreprocessSection::Interface => &mut self.interface,
            PreprocessSection::Implementation => &mut self.implementation,
            PreprocessSection::Declaration => &mut self.declaration,
        }
    }

    fn get(&self, section: PreprocessSection) -> &Vec<LineMapping> {
        match section {
            PreprocessSection::None => &self.none,
            PreprocessSection::Interface => &self.interface,
            PreprocessSection::Implementation => &self.implementation,
            PreprocessSection::Declaration => &self.declaration,
        }
    }

    fn push(&mut self, mapping: LineMapping) {
        if mapping.src_end_line > self.length {
            self.length = mapping.src_end_line;
        }
        if !self.files.contains(&mapping.dst_file) {
            self.files.push(mapping.dst_file.clone());
        }
        self.access(mapping.section).push(mapping);
    }

    fn sort(&mut self) {
        self.none.sort_by_key(|l| l.src_line);
        self.interface.sort_by_key(|l| l.src_line);
        self.implementation.sort_by_key(|l| l.src_line);
        self.declaration.sort_by_key(|l| l.src_line);
    }

    /// Remove mappings that overlap an earlier mapping of the same section
    /// (the lookup relies on non-overlapping ranges). Requires sorted mappings.
    ///
    /// Overlaps happen e.g. for the `#line` directives of the reference tool
    /// when a member function is defined in an INTERFACE section: its
    /// definition and its synthesized declaration then both map the head
    /// lines in the same section. The earlier range (the definition, which
    /// includes the head) is kept. With `touching_ok`, a range may start on
    /// the last line of the previous one (the legacy `#line` mappings
    /// overlap by one line at their boundaries).
    fn remove_overlaps(&mut self, file: &Path, touching_ok: bool) {
        for section in SECTION_PRIORITY {
            let mappings = self.access(section);
            let mut kept: Vec<LineMapping> = Vec::with_capacity(mappings.len());
            for mapping in mappings.drain(..) {
                if let Some(last) = kept.last() {
                    let overlaps = if touching_ok {
                        mapping.src_line < last.src_end_line
                    } else {
                        mapping.src_line <= last.src_end_line
                    };
                    if overlaps {
                        warn!(
                            "Dropping mapping overlapping an earlier one in {}: {:?} overlaps {:?}",
                            file.display(),
                            mapping,
                            last
                        );
                        continue;
                    }
                }
                kept.push(mapping);
            }
            *self.access(section) = kept;
        }
    }

    fn length(&self) -> u32 {
        self.length
    }
}

type LineMappings = HashMap<PathBuf, FileLineMappings>;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum MapDirection {
    ToPreprocess,
    FromPreprocess,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FiascoSourceMapping {
    to_preprocess: LineMappings,
    from_preprocess: LineMappings,
}

#[derive(Debug)]
pub struct SourceLocation {
    pub path: PathBuf,
    pub line: u32,
    pub character: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedRange {
    pub section: PreprocessSection,
    pub source_file: PathBuf,
    pub source_line: u32,
    pub generated_file: PathBuf,
    pub generated_line: u32,
    pub line_count: u32,
}

impl FiascoSourceMapping {
    fn new() -> FiascoSourceMapping {
        FiascoSourceMapping {
            to_preprocess: LineMappings::new(),
            from_preprocess: LineMappings::new(),
        }
    }

    pub fn from_generated_ranges(
        ranges: impl IntoIterator<Item = GeneratedRange>,
    ) -> Result<Self, String> {
        let mut result = Self::new();
        result.insert_generated_ranges(ranges)?;
        Ok(result)
    }

    pub fn empty() -> Self {
        Self::new()
    }

    /// Replace all mappings of the given source and generated files (e.g. the
    /// inputs and outputs of one preprocess run) with `ranges`.
    pub fn replace_generated_ranges(
        &mut self,
        sources: &[PathBuf],
        generated: &[PathBuf],
        ranges: impl IntoIterator<Item = GeneratedRange>,
    ) -> Result<(), String> {
        for source in sources {
            self.to_preprocess.remove(source);
        }
        for file in generated {
            self.from_preprocess.remove(file);
        }
        self.insert_generated_ranges(ranges)
    }

    fn insert_generated_ranges(
        &mut self,
        ranges: impl IntoIterator<Item = GeneratedRange>,
    ) -> Result<(), String> {
        let mut touched = (HashSet::new(), HashSet::new());
        for range in ranges {
            let offset = range.line_count.checked_sub(1).ok_or("empty mapping range")?;
            let source_end =
                range.source_line.checked_add(offset).ok_or("source mapping overflow")?;
            let generated_end =
                range.generated_line.checked_add(offset).ok_or("generated mapping overflow")?;
            touched.0.insert(range.source_file.clone());
            touched.1.insert(range.generated_file.clone());
            self.to_preprocess
                .entry(range.source_file.clone())
                .or_insert_with(FileLineMappings::new)
                .push(LineMapping {
                    section: range.section,
                    src_line: range.source_line,
                    src_end_line: source_end,
                    dst_file: range.generated_file.clone(),
                    dst_line: range.generated_line,
                });
            self.from_preprocess
                .entry(range.generated_file)
                .or_insert_with(FileLineMappings::new)
                .push(LineMapping {
                    section: range.section,
                    src_line: range.generated_line,
                    src_end_line: generated_end,
                    dst_file: range.source_file,
                    dst_line: range.source_line,
                });
        }
        for (files, paths) in [(&mut self.to_preprocess, &touched.0), (&mut self.from_preprocess, &touched.1)] {
            for path in paths {
                let mappings = files.get_mut(path).unwrap();
                mappings.sort();
                mappings.remove_overlaps(path, false);
            }
        }
        Ok(())
    }

    fn sort(&mut self) {
        for mappings in self.to_preprocess.values_mut() {
            mappings.sort()
        }
        for mappings in self.from_preprocess.values_mut() {
            mappings.sort()
        }
    }

    fn remove_overlaps(&mut self) {
        for (path, mappings) in self.to_preprocess.iter_mut().chain(self.from_preprocess.iter_mut()) {
            mappings.remove_overlaps(path, true)
        }
    }

    fn find_mapping<'a>(
        line_mappings: &'a LineMappings,
        path: &str,
        line: u32,
        section: PreprocessSection,
    ) -> Option<&'a LineMapping> {
        let path = PathBuf::from(path);
        match line_mappings.get(&path) {
            None => None,
            Some(mappings) => {
                debug!("Line {}", line);
                // NOTE: This relies on the assumption that there are no overlapping mappings.
                let index = mappings.get(section).partition_point(|l| line >= l.src_line);
                // If the predicate is never true, this will return the index of the first element,
                // even if that starts after the given line number, so check.
                if index > 0 {
                    let mapping = &mappings.get(section)[index - 1];
                    debug!(
                        "Mapping Src Line {} -> Dst Line {} ({} -> {})",
                        mapping.src_line,
                        mapping.dst_line,
                        path.display(),
                        mapping.dst_file.display()
                    );
                    assert!(line >= mapping.src_line);
                    if mapping.contains(line) {
                        Some(mapping)
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
        }
    }

    fn iter_mappings<'a>(
        line_mappings: &'a LineMappings,
        path: &str,
        start: u32,
        end: u32,
        section: PreprocessSection,
    ) -> impl Iterator<Item = &'a LineMapping> {
        let path = PathBuf::from(path);
        match line_mappings.get(&path) {
            Some(mappings) => mappings.get(section).iter(),
            None => [].iter(),
        }
        .filter(move |mapping| mapping.overlaps(start, end))
    }

    fn get(&self, direction: MapDirection) -> &LineMappings {
        match direction {
            MapDirection::ToPreprocess => &self.to_preprocess,
            MapDirection::FromPreprocess => &self.from_preprocess,
        }
    }

    pub fn map(
        &self,
        direction: MapDirection,
        path: &str,
        line: u32,
        character: u32,
    ) -> Option<SourceLocation> {
        let line_mappings = self.get(direction);
        // TODO: Priority to use here? Might depend on use case...
        let mapping = SECTION_PRIORITY
            .iter()
            .find_map(|&section| Self::find_mapping(line_mappings, path, line, section));
        match mapping {
            None => {
                debug!("No mapping found for Line {} ({})", line, path);
                None
            }
            Some(mapping) => Some(SourceLocation {
                path: mapping.dst_file.clone(),
                line: mapping.dst_line + (line - mapping.src_line),
                character,
            }),
        }
    }

    pub fn map_files(&self, direction: MapDirection, path: &str) -> &[PathBuf] {
        match self.get(direction).get(&PathBuf::from(path)) {
            None => &[],
            Some(mappings) => &mappings.files,
        }
    }

    pub fn map_files_with_range(
        &self,
        direction: MapDirection,
        path: &str,
        start: u32,
        end: u32,
    ) -> HashSet<&Path> {
        let line_mappings = self.get(direction);
        SECTION_PRIORITY
            .iter()
            .flat_map(|&section| Self::iter_mappings(line_mappings, path, start, end, section))
            .map(|mapping| mapping.dst_file.as_ref())
            .collect()
    }

    pub fn file_length(&self, direction: MapDirection, path: &Path) -> Option<u32> {
        self.get(direction).get(path).map(FileLineMappings::length)
    }
}

lazy_static! {
    static ref NAME_REPLACE_RE: Regex = Regex::new(r"[+-.]").unwrap();
    static ref LINE_REF_RE: Regex = Regex::new(r#"^#line (\d+) "(.+)"$"#).unwrap();
    static ref INTERFACE_SEC_RE: Regex = Regex::new(r"^// INTERFACE").unwrap();
    static ref IMPLEMENTATION_SEC_RE: Regex = Regex::new(r"^// IMPLEMENTATION").unwrap();
}

fn extract_line_mappings<I>(name: &str, lines: I) -> Vec<LineMapping>
where
    I: IntoIterator<Item = (usize, io::Result<String>)>,
{
    let endif_pattern = format!("#endif // {}", NAME_REPLACE_RE.replace(name, "_"));
    let mut cur_section = PreprocessSection::None;
    let mut mappings: Vec<LineMapping> = Vec::new();
    let mut ln = 0;
    let mut ln_offset = 0;
    for (l, r) in lines {
        ln = l;
        if let Ok(line) = r {
            if let Some(cap) = LINE_REF_RE.captures(line.as_str()) {
                if let Some(m) = mappings.last_mut() {
                    m.src_end_line = max(m.src_line, l as u32 - ln_offset - 1)
                };
                mappings.push(LineMapping {
                    section: cur_section,
                    src_line: l as u32 + 1, // Adjust for the #line comment itself
                    src_end_line: 0,        // Set later
                    dst_file: PathBuf::from(&cap[2]),
                    dst_line: cap[1].parse::<u32>().unwrap_or(1) - 1,
                });
                ln_offset = 0;
            } else if line.starts_with("// INTERFACE") {
                cur_section = PreprocessSection::Interface;
                if let Some(m) = mappings.last_mut() {
                    m.src_end_line = l as u32
                };
                // Do not include the preprocess generated comments into the mapping.
                ln_offset = 5;
            } else if line.starts_with("// IMPLEMENTATION") {
                cur_section = PreprocessSection::Implementation;
                if let Some(m) = mappings.last_mut() {
                    m.src_end_line = l as u32
                };
                // Do not include the preprocess generated comments into the mapping.
                ln_offset = 5;
            } else if line.starts_with("private: // EXTENSION") {
                // Do not include the following three preprocess generated lines into the mapping,
                // to avoid artificial overlaps with other mappings.
                ln_offset = 3;
            } else if line.starts_with(&endif_pattern) {
                // Reached the end of the file, do not include the #endif generated by preprocess
                // into the mapping, to avoid artificial overlaps with other mappings.
                ln -= 1;
                break;
            }
        }
    }
    if let Some(m) = mappings.last_mut() {
        m.src_end_line = max(m.src_line, ln as u32 - ln_offset)
    };
    mappings
}

fn extract_line_mappings_for_file(path: &Path, source_mapping: &mut FiascoSourceMapping) {
    let file = File::open(path);
    if file.is_err() {
        return;
    }

    let reader = BufReader::new(file.unwrap());
    let file_name = path.file_name().and_then(OsStr::to_str).unwrap();
    let mappings = extract_line_mappings(file_name, reader.lines().enumerate());
    for mapping in &mappings {
        if !source_mapping.to_preprocess.contains_key(&mapping.dst_file) {
            source_mapping.to_preprocess.insert(mapping.dst_file.clone(), FileLineMappings::new());
        }
        source_mapping.to_preprocess.get_mut(&mapping.dst_file).unwrap().push(LineMapping {
            section: mapping.section,
            src_line: mapping.dst_line,
            src_end_line: mapping.dst_line + (mapping.src_end_line - mapping.src_line),
            dst_file: path.to_path_buf(),
            dst_line: mapping.src_line,
        })
    }
    source_mapping
        .from_preprocess
        .insert(path.to_path_buf(), FileLineMappings::from_mappings(mappings));
}

lazy_static! {
    static ref STAMP_RE: Regex = Regex::new(r"^auto/stamp-(.+).ready:\s*(.+)$").unwrap();
}

pub fn load_modules(build_dir: &str) -> HashMap<String, Vec<String>> {
    let file = File::open(Path::new(build_dir).join(".Modules.deps")).unwrap();
    let reader = BufReader::new(file);
    reader
        .lines()
        .filter_map(Result::ok)
        .filter_map(|line| {
            STAMP_RE.captures(&line).map(|cap| {
                (cap[1].to_owned(), cap[2].split_whitespace().map(str::to_owned).collect())
            })
        })
        .collect()
}

pub fn load_source_mapping(build_dir: &Path) -> FiascoSourceMapping {
    // let cdb_file = Path::new(json_compilation_db::DEFAULT_FILE_NAME);
    // let entries = json_compilation_db::from_file(cdb_file).unwrap_or(vec![]);

    // TODO: There are also files without specific prefixes...
    let mut source_mapping = FiascoSourceMapping::new();
    let paths = fs::read_dir(build_dir.join("auto")).unwrap();
    for path in paths {
        let p = path.unwrap();
        extract_line_mappings_for_file(&p.path(), &mut source_mapping);
    }
    /*
    for entry in &entries {
        // Preprocessed file are located in the auto directory
        if entry.file.parent().map(|f| f.ends_with("auto")).unwrap_or(false) {
            extract_line_mappings_for_file(&entry.file, &mut source_mapping);
            extract_line_mappings_for_file(&entry.file.with_extension("h"),
                                           &mut source_mapping);
            if let Some(stem) = entry.file.file_stem() {
                let mut file_name = stem.to_owned();
                file_name.push("_i.h");
                extract_line_mappings_for_file(&entry.file.with_file_name(file_name),
                                               &mut source_mapping);
            }
        }
    }
    */
    source_mapping.sort();
    source_mapping.remove_overlaps();
    source_mapping
}

#[cfg(test)]
mod tests {
    use std::io::BufWriter;
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn generated_ranges_map_both_directions_and_validate_bounds() {
        let range = || GeneratedRange {
            section: PreprocessSection::Interface,
            source_file: "source.cpp".into(),
            source_line: 4,
            generated_file: "generated.h".into(),
            generated_line: 10,
            line_count: 2,
        };
        let mapping = FiascoSourceMapping::from_generated_ranges([range()]).unwrap();
        let generated = mapping.map(MapDirection::ToPreprocess, "source.cpp", 5, 3).unwrap();
        assert_eq!(
            (generated.path, generated.line, generated.character),
            ("generated.h".into(), 11, 3)
        );
        let source = mapping.map(MapDirection::FromPreprocess, "generated.h", 10, 7).unwrap();
        assert_eq!((source.path, source.line, source.character), ("source.cpp".into(), 4, 7));
        assert!(mapping.map(MapDirection::ToPreprocess, "source.cpp", 6, 0).is_none());
        // Overlapping ranges are dropped instead of failing.
        let duplicate = FiascoSourceMapping::from_generated_ranges([range(), range()]).unwrap();
        assert_eq!(duplicate.to_preprocess[Path::new("source.cpp")].interface.len(), 1);
        let mut empty = range();
        empty.line_count = 0;
        assert!(FiascoSourceMapping::from_generated_ranges([empty]).is_err());
    }

    /// Perl `preprocess` output for a member function defined in an INTERFACE
    /// section: its definition and declaration overlap in the same section.
    #[test]
    fn loads_overlapping_line_directives() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("auto")).unwrap();
        let header = "// AUTOMATICALLY GENERATED -- DO NOT EDIT!         -*- c++ -*-\n\n#pragma once\n\
            #line 2 \"src.cpp\"\n\n#include \"types.h\"\n\n//\n// INTERFACE definition follows\n//\n\n\
            #line 4 \"src.cpp\"\n\nclass Cpubits\n{\npublic:\n  int x;\n\npublic:  \n\
            #line 13 \"src.cpp\"\n  static inline bool has_16bit_asids();\n};\n\n\
            #line 11 \"src.cpp\"\n\n\ninline bool Cpubits::has_16bit_asids()\n{\n  return true;\n}\n";
        fs::write(dir.path().join("auto/cpubits.h"), header).unwrap();
        let mapping = load_source_mapping(dir.path());
        let generated = dir.path().join("auto/cpubits.h");
        let lines: Vec<&str> = header.lines().collect();
        // The head line of the definition (source line 13) maps to the definition.
        let target = mapping.map(MapDirection::ToPreprocess, "src.cpp", 12, 0).unwrap();
        assert_eq!(lines[target.line as usize], "inline bool Cpubits::has_16bit_asids()");
        // The declaration still maps back to the source.
        let declaration = lines.iter().position(|l| l.contains("static inline bool")).unwrap() as u32;
        let source = mapping.map(MapDirection::FromPreprocess, generated.to_str().unwrap(), declaration, 2).unwrap();
        assert_eq!((source.path, source.line), ("src.cpp".into(), 12));
    }

    /// The Fiasco build directory shared by all tests, created from the
    /// `tests/fixtures/fiasco` submodule by `tests/fiasco_build.py --complete`.
    fn build_dir() -> PathBuf {
        std::env::var_os("FIASCO_BUILD_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/build"))
    }

    #[test]
    #[ignore = "requires a complete Fiasco build directory (tests/fiasco_build.py --complete)"]
    fn load_mods() {
        let modules = load_modules(build_dir().to_str().unwrap());
        let _ = env_logger::try_init();
        println!("{:?}", modules);
        assert!(!modules.is_empty());
    }

    #[test]
    #[ignore = "requires a complete Fiasco build directory (tests/fiasco_build.py --complete)"]
    fn load() {
        let source_mapping = load_source_mapping(&build_dir());
        serde_json::to_writer_pretty(
            BufWriter::new(File::create(build_dir().join("source_to_preprocess.json")).unwrap()),
            &source_mapping.to_preprocess,
        )
        .unwrap();
        serde_json::to_writer_pretty(
            BufWriter::new(File::create(build_dir().join("preprocess_to_source.json")).unwrap()),
            &source_mapping.from_preprocess,
        )
        .unwrap();

        let _ = env_logger::try_init();
        let source = source_mapping
            .to_preprocess
            .keys()
            .find(|path| path.ends_with("kern/ram_quota.cpp"))
            .expect("kern/ram_quota.cpp is mapped");
        let line = fs::read_to_string(source)
            .unwrap()
            .lines()
            .position(|line| line.starts_with("class Ram_quota"))
            .expect("class Ram_quota is declared") as u32;
        let mut mapped = source_mapping
            .map(MapDirection::ToPreprocess, source.to_str().unwrap(), line, 0)
            .expect("Valid mapping.");
        mapped.line += 1;
        mapped.character += 1;
        println!("{:?}", mapped);
    }
}
