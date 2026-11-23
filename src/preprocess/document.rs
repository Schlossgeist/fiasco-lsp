use std::path::{Path, PathBuf};

use crate::source_mapping::{GeneratedRange, PreprocessSection};

/// Origin of a generated line: an unchanged-column copy of a source line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineOrigin<'a> {
    pub section: PreprocessSection,
    pub file: &'a Path,
    pub line: u32,
}

/// A generated output file and the mapping of its lines to source lines.
///
/// Mapped lines have the same column layout as their source line (FPP
/// keywords are blanked instead of removed), so positions translate 1:1.
#[derive(Debug, Clone)]
pub struct GeneratedDocument {
    pub path: PathBuf,
    pub text: String,
    pub mappings: Vec<GeneratedRange>,
    lines: u32,
    /// Banner that is emitted only if more content follows (see `print_head`
    /// in the reference tool).
    pending_head: String,
}

impl GeneratedDocument {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            text: String::new(),
            mappings: Vec::new(),
            lines: 0,
            pending_head: String::new(),
        }
    }

    pub fn line_count(&self) -> u32 {
        self.lines
    }

    pub fn head(&mut self, text: &str) {
        self.pending_head.push_str(text);
    }

    pub fn clear_head(&mut self) {
        self.pending_head.clear();
    }

    fn flush_head(&mut self) {
        if !self.pending_head.is_empty() {
            let head = std::mem::take(&mut self.pending_head);
            self.push_raw(&head);
        }
    }

    fn push_raw(&mut self, text: &str) {
        for line in text.split_inclusive('\n') {
            self.text.push_str(line.strip_suffix('\n').unwrap_or(line));
            self.text.push('\n');
            self.lines += 1;
        }
    }

    /// Append synthesized text (always terminated by a newline).
    pub fn append_generated(&mut self, text: &str) {
        self.flush_head();
        self.push_raw(text);
    }

    /// Append one line (without terminator) that is a column-preserving copy
    /// of a source line, or an unmapped line if `origin` is `None`.
    pub fn append_line(&mut self, line: &str, origin: Option<LineOrigin<'_>>) {
        self.flush_head();
        debug_assert!(!line.contains('\n'));
        if let Some(origin) = origin {
            let generated_line = self.lines;
            match self.mappings.last_mut() {
                Some(last)
                    if last.section == origin.section
                        && last.source_file == origin.file
                        && last.source_line + last.line_count == origin.line
                        && last.generated_line + last.line_count == generated_line =>
                {
                    last.line_count += 1
                }
                _ => self.mappings.push(GeneratedRange {
                    section: origin.section,
                    source_file: origin.file.to_owned(),
                    source_line: origin.line,
                    generated_file: self.path.clone(),
                    generated_line,
                    line_count: 1,
                }),
            }
        }
        self.text.push_str(line);
        self.text.push('\n');
        self.lines += 1;
    }

    /// Render the text with `#line` directives before every mapped run, like
    /// the reference tool does for compiler diagnostics. The mapping of the
    /// rendered text is not tracked.
    pub fn render_with_line_directives(&self) -> String {
        let mut out = String::with_capacity(self.text.len() * 2);
        let mut ranges = self.mappings.iter().peekable();
        for (index, line) in self.text.split_inclusive('\n').enumerate() {
            if let Some(range) = ranges.peek() {
                if range.generated_line as usize == index {
                    out.push_str(&format!(
                        "#line {} \"{}\"\n",
                        range.source_line + 1,
                        range.source_file.display()
                    ));
                    ranges.next();
                }
            }
            out.push_str(line);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_mapping::{FiascoSourceMapping, MapDirection};

    fn origin(line: u32) -> Option<LineOrigin<'static>> {
        Some(LineOrigin { section: PreprocessSection::Interface, file: Path::new("source.cpp"), line })
    }

    #[test]
    fn coalesces_and_maps_lines() {
        let mut document = GeneratedDocument::new("generated.h");
        document.append_generated("// generated\n");
        document.append_line("int a;", origin(4));
        document.append_line("int b;", origin(5));
        document.append_line("int c;", origin(9));
        assert_eq!(document.mappings.len(), 2);
        assert_eq!(document.line_count(), 4);
        let rendered = document.render_with_line_directives();
        assert_eq!(
            rendered,
            "// generated\n#line 5 \"source.cpp\"\nint a;\nint b;\n#line 10 \"source.cpp\"\nint c;\n"
        );
        let mapping = FiascoSourceMapping::from_generated_ranges(document.mappings).unwrap();
        let target = mapping.map(MapDirection::ToPreprocess, "source.cpp", 5, 3).unwrap();
        assert_eq!((target.path, target.line, target.character), ("generated.h".into(), 2, 3));
        assert!(mapping.map(MapDirection::FromPreprocess, "generated.h", 0, 0).is_none());
    }

    #[test]
    fn heads_are_only_printed_before_content() {
        let mut document = GeneratedDocument::new("x.h");
        document.head("// banner\n");
        document.clear_head();
        document.head("// other\n");
        document.append_line("x", None);
        assert_eq!(document.text, "// other\nx\n");
    }
}
