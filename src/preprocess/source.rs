//! A parsed preprocess input file.

use std::ops::Range;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{eyre, Result};
use tree_sitter::{Node, Parser, Tree};

pub struct SourceFile {
    pub path: PathBuf,
    /// The file contents with `#if 0` regions blanked out (byte offsets are
    /// identical to the original contents). Everything is emitted from here.
    pub text: String,
    /// Like `text`, but with comments and string/character literals blanked,
    /// for textual scans that must not match inside them.
    pub code: String,
    /// Byte offset of the start of each line.
    pub line_starts: Vec<usize>,
    pub tree: Tree,
    /// Separately parsed chunks of top-level items tree-sitter failed on
    /// (see `split_chunks`), keyed by the byte range of the replaced node.
    pub pieces: Vec<(Range<usize>, Tree)>,
    /// Part name derived from the file name (`name-part.cpp`), used with `-s`.
    pub part_ext: String,
}

impl SourceFile {
    pub fn new(path: impl Into<PathBuf>, original: &str) -> Result<Self> {
        let path = path.into();
        let (masked, regions) = mask_if_zero(original);
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_fpp::LANGUAGE.into())?;
        let tree = parser.parse(&masked, None).ok_or_else(|| eyre!("FPP parsing failed"))?;
        let code = mask_comments_and_strings(&masked, tree.root_node());
        // The reference tool drops `#if 0` regions only between items; inside
        // a block they are kept verbatim (they are parsed masked, though).
        let mut text = masked.clone();
        let mut depth = 0i64;
        let mut scanned = 0;
        for region in regions {
            for &b in &code.as_bytes()[scanned..region.start] {
                depth += match b {
                    b'{' => 1,
                    b'}' => -1,
                    _ => 0,
                };
            }
            scanned = region.start;
            if depth > 0 {
                text.replace_range(region.clone(), &original[region]);
            }
        }
        let mut line_starts = vec![0];
        line_starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        let part_ext = part_ext(&path);

        // Errors can make tree-sitter swallow many items into one ERROR
        // node. Split such regions like the reference tool would and parse
        // the pieces separately, so an error only affects its own chunk.
        let mut pieces = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind().starts_with("preproc_if") || child.kind().starts_with("preproc_el") {
                    stack.push(child);
                } else if child.has_error() && !has_section_child(child) {
                    let chunks = split_chunks(&code, &line_starts, child.byte_range());
                    // A node with errors is only replaced if it swallowed several items.
                    if child.kind() != "ERROR" && chunks.len() < 2 {
                        continue;
                    }
                    for range in chunks {
                        let ts_range = tree_sitter::Range {
                            start_byte: range.start,
                            end_byte: range.end,
                            start_point: point(&line_starts, range.start),
                            end_point: point(&line_starts, range.end),
                        };
                        parser.set_included_ranges(&[ts_range])?;
                        if let Some(piece) = parser.parse(&masked, None) {
                            pieces.push((child.byte_range(), piece));
                        }
                    }
                }
            }
        }
        Ok(Self { path, text, code, line_starts, tree, pieces, part_ext })
    }

    /// Zero-based line containing `byte`.
    pub fn line_of(&self, byte: usize) -> usize {
        self.line_starts.partition_point(|&start| start <= byte) - 1
    }

    /// Byte range of line `line` including its line terminator.
    pub fn line_range(&self, line: usize) -> Range<usize> {
        let start = self.line_starts[line];
        let end = self.line_starts.get(line + 1).copied().unwrap_or(self.text.len());
        start..end
    }

    /// End of the line containing `byte` (after the newline, if any).
    pub fn end_of_line(&self, byte: usize) -> usize {
        self.line_range(self.line_of(byte)).end
    }

    pub fn node_text(&self, node: Node<'_>) -> &str {
        &self.text[node.byte_range()]
    }
}

pub fn has_section_child(node: Node<'_>) -> bool {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).any(|c| c.kind() == "fiasco_section");
    found
}

fn point(line_starts: &[usize], byte: usize) -> tree_sitter::Point {
    let row = line_starts.partition_point(|&start| start <= byte) - 1;
    tree_sitter::Point::new(row, byte - line_starts[row])
}

/// Split `range` into chunks like the reference tool reads them: line by
/// line, until braces balance and a `;` or a block was seen. Preprocessor
/// lines at the start of a chunk form their own chunk.
fn split_chunks(code: &str, line_starts: &[usize], range: Range<usize>) -> Vec<Range<usize>> {
    let mut result = Vec::new();
    let mut start = range.start;
    let mut depth = 0i64;
    let mut seen_end = false;
    let mut line = line_starts.partition_point(|&s| s <= range.start) - 1;
    let mut continuation = false;
    while start < range.end {
        let line_end = line_starts.get(line + 1).copied().unwrap_or(code.len()).min(range.end);
        let line_start = line_starts[line].max(start).max(range.start);
        let text = &code[line_start..line_end];
        line += 1;
        if continuation || (code[start..line_start].trim().is_empty() && text.trim_start().starts_with('#')) {
            continuation = text.trim_end().ends_with('\\');
            if !continuation {
                result.push(start..line_end);
                start = line_end;
            }
            continue;
        }
        for b in text.bytes() {
            match b {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    seen_end = true;
                }
                b';' => seen_end = true,
                _ => {}
            }
        }
        if code[start..line_end].trim().is_empty() {
            start = line_end;
        } else if depth <= 0 && seen_end {
            result.push(start..line_end);
            start = line_end;
            depth = 0;
            seen_end = false;
        }
        if line_end >= range.end {
            break;
        }
    }
    if start < range.end && !code[start..range.end].trim().is_empty() {
        result.push(start..range.end);
    }
    result
}

/// `-s` mode: `dir/name-part.cpp` belongs to implementation part `part`.
fn part_ext(path: &Path) -> String {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    match stem.split_once('-') {
        Some((_, part)) if name.contains('.') => part.to_owned(),
        _ => String::new(),
    }
}

fn blank_bytes(text: &mut [u8], range: Range<usize>) {
    for b in &mut text[range] {
        if *b != b'\n' {
            *b = b' ';
        }
    }
}

/// Drop `#if 0` regions with the semantics of the reference tool: only `#if 0`
/// starts an ignored region; nested conditionals are tracked so the matching
/// `#else`/`#elif`/`#endif` is found. The `#if 0`/`#else`/`#endif` lines
/// themselves are dropped, the `#else` branch is kept.
pub fn mask_if_zero(original: &str) -> (String, Vec<Range<usize>>) {
    let mut bytes = original.as_bytes().to_vec();
    let mut stack: Vec<i8> = Vec::new();
    let mut regions: Vec<Range<usize>> = Vec::new();
    let ignoring = |stack: &[i8]| stack.contains(&1);
    let mut offset = 0;
    for line in original.split_inclusive('\n') {
        let range = offset..offset + line.len();
        offset += line.len();
        let directive = line.trim_start().strip_prefix('#').map(|rest| {
            let rest = rest.trim_start();
            let word_end = rest.find(|c: char| !c.is_ascii_alphanumeric()).unwrap_or(rest.len());
            (&rest[..word_end], rest[word_end..].trim())
        });
        let mask = match directive {
            Some(("if", condition)) if condition == "0" || condition.starts_with("0 ") => {
                stack.push(1);
                true
            }
            Some(("if" | "ifdef" | "ifndef", _)) if !stack.is_empty() => {
                stack.push(0);
                ignoring(&stack)
            }
            Some(("else" | "elif", _)) if !stack.is_empty() => {
                let was_ignoring = ignoring(&stack);
                let top = stack.last_mut().unwrap();
                *top = -*top;
                was_ignoring
            }
            Some(("endif", _)) if !stack.is_empty() => {
                let value = stack.pop().unwrap();
                ignoring(&stack) || value != 0
            }
            _ => ignoring(&stack),
        };
        if mask {
            match regions.last_mut() {
                Some(last) if last.end == range.start => last.end = range.end,
                _ => regions.push(range.clone()),
            }
            blank_bytes(&mut bytes, range);
        }
    }
    // Only ASCII bytes of complete lines were replaced by spaces.
    (String::from_utf8(bytes).expect("masking keeps UTF-8 valid"), regions)
}

fn mask_comments_and_strings(text: &str, root: Node<'_>) -> String {
    let mut bytes = text.as_bytes().to_vec();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "comment" | "string_literal" | "raw_string_literal" | "char_literal"
            | "system_lib_string" => {
                let range = node.byte_range();
                // Keep the delimiters of literals so the code still looks like an expression.
                let range = if node.kind() == "comment" {
                    range
                } else {
                    range.start + 1..range.end.saturating_sub(1).max(range.start + 1)
                };
                for b in &mut bytes[range] {
                    if *b != b'\n' {
                        *b = b' ';
                    }
                }
            }
            _ => {
                let mut cursor = node.walk();
                stack.extend(node.children(&mut cursor));
            }
        }
    }
    // Masking replaces whole literals/comments, so multi-byte sequences are
    // replaced completely; fall back to the lossless text otherwise.
    String::from_utf8(bytes).unwrap_or_else(|_| text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_if_zero_like_reference() {
        let text = "a\n#if 0\nb\n#ifdef X\nc\n#endif\n#else\nd\n#endif\ne\n#ifdef Y\nf\n#endif\n";
        let (masked, _) = mask_if_zero(text);
        assert_eq!(masked.len(), text.len());
        let kept: Vec<_> = masked.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(kept, ["a", "d", "e", "#ifdef Y", "f", "#endif"]);
    }

    #[test]
    fn keeps_if_zero_inside_blocks() {
        let text = "#if 0\nx\n#endif\nvoid f() {\n#if 0\ny(\n#endif\n}\n";
        let file = SourceFile::new("a.cpp", text).unwrap();
        assert!(!file.text.contains('x'));
        assert!(file.text.contains("y("));
        assert!(!file.tree.root_node().has_error());
    }

    #[test]
    fn splits_error_regions_into_chunks() {
        let code = "int a;\nvoid f()\n{\n  x;\n}\n#define X \\\n  1\nint b = { 1 };\n";
        let mut starts = vec![0];
        starts.extend(code.match_indices('\n').map(|(i, _)| i + 1));
        let chunks: Vec<_> = split_chunks(code, &starts, 0..code.len()).into_iter().map(|r| &code[r]).collect();
        assert_eq!(chunks, ["int a;\n", "void f()\n{\n  x;\n}\n", "#define X \\\n  1\n", "int b = { 1 };\n"]);
    }

    #[test]
    fn part_names_and_lines() {
        assert_eq!(part_ext(Path::new("/x/foo-arm.cpp")), "arm");
        assert_eq!(part_ext(Path::new("foo.cpp")), "");
        let file = SourceFile::new("a.cpp", "ab\ncd\n// x \"y\"\nconst char *s = \"z\";\n").unwrap();
        assert_eq!(file.line_of(0), 0);
        assert_eq!(file.line_of(3), 1);
        assert_eq!(file.line_range(1), 3..6);
        assert!(!file.code.contains('x'));
        assert!(file.code.contains("\" \""));
    }
}
