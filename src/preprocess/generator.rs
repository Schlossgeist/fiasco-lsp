//! Combine parsed chunks into the generated header, internal header and
//! implementation files, following the reference `preprocess` tool.
//!
//! Unlike the reference tool, source text is never reformatted: FPP keywords
//! and other text that must not appear in an output are replaced by blanks,
//! and synthesized text is placed on separate lines. Every emitted source line
//! therefore keeps its column layout and can be mapped 1:1.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::PathBuf;

use super::chunks::{
    self, ClassDef, ClassSyntax, Extension, FunctionHead, ItemKind, SectionKind,
};
use super::document::{GeneratedDocument, LineOrigin};
use super::source::SourceFile;
use super::tags;
use crate::source_mapping::PreprocessSection;

#[derive(Debug, Clone)]
pub struct Options {
    /// `-c`: base name for generated include directives and files.
    pub base: String,
    /// `-o`: base name of output files (overrides `-p`).
    pub out_base: Option<String>,
    /// `-p`: prefix of output file names.
    pub prefix: String,
    /// `-h`: name of the public header.
    pub public_header: Option<String>,
    /// `-i`: make inline functions real inline functions.
    pub inlines: bool,
    /// `-e`: enabled tags; sections are selected by evaluating their tags.
    pub tags: Option<HashSet<String>>,
    /// `-s`: one implementation file per input file instead of per section part.
    pub single_source: bool,
    /// `-t`: truncate implementation files without content.
    pub truncate_empty: bool,
    /// `-w`: warn if no inline code for NEEDS is found.
    pub warn_needs: bool,
    pub header_ext: String,
    pub source_ext: String,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            base: String::new(),
            out_base: None,
            prefix: String::new(),
            public_header: None,
            inlines: false,
            tags: None,
            single_source: false,
            truncate_empty: false,
            warn_needs: false,
            header_ext: "h".into(),
            source_ext: "cc".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub file: PathBuf,
    /// Zero-based line of the item the diagnostic refers to.
    pub line: u32,
    /// Zero-based line where the item's chunk starts (including leading
    /// blank lines and comments), as reported by the reference tool.
    pub block_line: u32,
    pub severity: Severity,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputKind {
    Header,
    Internal,
    Implementation,
}

#[derive(Debug, Clone)]
pub struct Output {
    pub kind: OutputKind,
    pub document: GeneratedDocument,
    /// `-t`: the file should be written with zero length.
    pub truncated: bool,
}

pub struct Generated {
    pub outputs: Vec<Output>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Generated {
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(|d| d.severity == Severity::Error)
    }
}

/// Replace a byte range by `text` (placed at its start), blanking the rest.
#[derive(Debug, Clone)]
struct Edit {
    range: Range<usize>,
    text: &'static str,
}

impl Edit {
    fn blank(range: Range<usize>) -> Self {
        Self { range, text: "" }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Visibility {
    Public,
    Protected,
    Private,
    ImplementationOnly,
    Free,
    Static,
}

struct Function {
    head: FunctionHead,
    visibility: Visibility,
    default_impl: bool,
    override_impl: bool,
    overrides_default: Option<usize>,
    inline: bool,
    always_inline: bool,
    is_static: bool,
    hide: bool,
    /// Index into `head.templates` of the class template header, if any.
    class_template: Option<usize>,
    template: bool,
    fully_specialized: bool,
    needs: Vec<String>,
}

struct Class {
    def: ClassDef,
    /// Owning chunk; `None` for implicitly declared nested classes.
    chunk: Option<usize>,
    section: SectionKind,
    nested: bool,
    funcs: Vec<usize>,
    extensions: Vec<(usize, Extension)>,
}

enum ChunkKind {
    Include,
    Code { blank: Vec<Range<usize>> },
    Class(usize),
    Function(Box<Function>),
}

struct Chunk {
    file: usize,
    section: SectionKind,
    part_ext: String,
    range: Range<usize>,
    start: usize,
    kind: ChunkKind,
}

impl Chunk {
    fn function(&self) -> Option<&Function> {
        match &self.kind {
            ChunkKind::Function(f) => Some(f),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Obj {
    Class(usize),
    Chunk(usize),
}

struct Generator<'a> {
    opts: &'a Options,
    files: &'a [SourceFile],
    tag_edits: Vec<Vec<Edit>>,
    chunks: Vec<Chunk>,
    interface: Vec<usize>,
    implementation: Vec<usize>,
    classes: Vec<Class>,
    class_names: HashMap<String, usize>,
    free_funcs: Vec<usize>,
    functions: HashMap<String, usize>,
    includes: HashMap<String, usize>,
    include_inline: HashSet<usize>,
    impl_parts: Vec<String>,
    public_base: String,
    printed: HashSet<Obj>,
    public_inline: HashSet<Obj>,
    private_inline: HashSet<Obj>,
    unit_inline: HashSet<Obj>,
    order_public: Vec<Obj>,
    order_private: Vec<Obj>,
    order_unit: Vec<Obj>,
    claimed: HashSet<(usize, usize, PreprocessSection)>,
    diagnostics: Vec<Diagnostic>,
    fatal: bool,
    /// Item start -> chunk start, for reference-compatible diagnostics.
    block_starts: HashMap<(usize, usize), usize>,
}

/// Preprocess `files` (in order) into the output documents. Output paths are
/// relative names as the reference tool would create them.
pub fn generate(files: &[SourceFile], opts: &Options) -> Generated {
    let mut generator = Generator {
        opts,
        files,
        tag_edits: Vec::new(),
        chunks: Vec::new(),
        interface: Vec::new(),
        implementation: Vec::new(),
        classes: Vec::new(),
        class_names: HashMap::new(),
        free_funcs: Vec::new(),
        functions: HashMap::new(),
        includes: HashMap::new(),
        include_inline: HashSet::new(),
        impl_parts: Vec::new(),
        public_base: opts.public_header.clone().unwrap_or_else(|| opts.base.clone()),
        printed: HashSet::new(),
        public_inline: HashSet::new(),
        private_inline: HashSet::new(),
        unit_inline: HashSet::new(),
        order_public: Vec::new(),
        order_private: Vec::new(),
        order_unit: Vec::new(),
        claimed: HashSet::new(),
        diagnostics: Vec::new(),
        fatal: false,
        block_starts: HashMap::new(),
    };
    generator.parse();
    let outputs = generator.print();
    Generated { outputs, diagnostics: generator.diagnostics }
}

fn utf16_width(c: char) -> usize {
    c.len_utf16()
}

impl<'a> Generator<'a> {
    fn diag(&mut self, file: usize, byte: usize, severity: Severity, message: String) {
        let source = &self.files[file];
        let line_of = |byte: usize| source.line_of(byte.min(source.text.len().saturating_sub(1))) as u32;
        let block = self.block_starts.get(&(file, byte)).copied().unwrap_or(byte);
        self.diagnostics.push(Diagnostic {
            file: source.path.clone(),
            line: line_of(byte),
            block_line: line_of(block),
            severity,
            message,
        });
    }

    fn error(&mut self, file: usize, byte: usize, message: String) {
        self.diag(file, byte, Severity::Error, message);
    }

    fn add_part(&mut self, part: &str) {
        if !self.impl_parts.iter().any(|p| p == part) {
            self.impl_parts.push(part.to_owned());
        }
    }

    // -----------------------------------------------------------------------
    // Parsing

    fn parse(&mut self) {
        let mut section = SectionKind::Implementation;
        let mut part = String::new();
        let mut pending_comment: Option<Chunk> = None;
        let mut previous_part_ext = String::new();
        // Like the section, skipping a disabled section continues into the next file.
        let mut skip = false;
        for (index, file) in self.files.iter().enumerate() {
            let edits = self.tag_enabled_edits(index, file);
            self.tag_edits.push(edits);
            if index > 0 {
                previous_part_ext = self.files[index - 1].part_ext.clone();
            }
            if self.opts.single_source {
                self.add_part(&file.part_ext);
            }
            let mut first_item = true;
            for item in chunks::items(file) {
                // The reference tool attributes a file's leading comments to
                // the block that started at the end of the previous file.
                let leading = std::mem::take(&mut first_item) && index > 0;
                self.block_starts.insert((index, item.start), item.range.start);
                let kind = match item.kind {
                    ItemKind::Section { kind, condition } => {
                        // The comment belongs to the previous section, but is
                        // dropped if the new section is disabled.
                        let comment = pending_comment.take();
                        let enabled = match (&self.opts.tags, &condition) {
                            (Some(tags), Some(c)) if self.opts.tags.is_some() || kind == SectionKind::Implementation => {
                                tags::evaluate(c, tags).unwrap_or(false)
                            }
                            _ => true,
                        };
                        if let Some(comment) = comment.filter(|_| enabled) {
                            self.add_chunk(comment);
                        }
                        skip = false;
                        section = kind;
                        if self.opts.tags.is_some() || kind == SectionKind::Implementation {
                            part = condition.clone().unwrap_or_default();
                            if !self.opts.single_source {
                                self.add_part(&part.clone());
                            }
                            if let (Some(tags), Some(condition)) = (&self.opts.tags, &condition) {
                                skip = match tags::evaluate(condition, tags) {
                                    Ok(value) => !value,
                                    Err(_) => {
                                        let message = format!("syntax error in tag '{condition}'");
                                        self.error(index, item.start, message);
                                        true
                                    }
                                };
                            }
                        } else if let Some(name) = condition {
                            if self.opts.public_header.as_ref().is_some_and(|h| *h != name) {
                                let message = "all INTERFACE arguments and option -h must be consistent";
                                self.error(index, item.start, message.into());
                            }
                            self.public_base = name;
                        }
                        continue;
                    }
                    _ if skip => continue,
                    ItemKind::SectionComment => {
                        let part_ext = if !self.opts.single_source {
                            part.clone()
                        } else if leading {
                            previous_part_ext.clone()
                        } else {
                            file.part_ext.clone()
                        };
                        pending_comment = Some(Chunk {
                            file: index,
                            section,
                            part_ext,
                            range: item.range,
                            start: item.start,
                            kind: ChunkKind::Code { blank: Vec::new() },
                        });
                        continue;
                    }
                    ItemKind::Include(name) => {
                        let chunk = self.chunks.len();
                        self.includes.insert(name, chunk);
                        ChunkKind::Include
                    }
                    ItemKind::Code => ChunkKind::Code { blank: Vec::new() },
                    ItemKind::Class(def) => {
                        let class = self.classes.len();
                        if def.syntax != ClassSyntax::ForwardDecl {
                            self.class_names.insert(def.name.clone(), class);
                        }
                        self.classes.push(Class {
                            def,
                            chunk: Some(self.chunks.len()),
                            section,
                            nested: false,
                            funcs: Vec::new(),
                            extensions: Vec::new(),
                        });
                        ChunkKind::Class(class)
                    }
                    ItemKind::Extension(extension) => {
                        match self.class_names.get(&extension.name) {
                            Some(&class) => self.classes[class].extensions.push((index, extension)),
                            None => {
                                let message =
                                    format!("Class extension for undefined class {}", extension.name);
                                self.error(index, item.start, message);
                            }
                        }
                        continue;
                    }
                    ItemKind::Function(head) => self.function(index, section, *head),
                };
                let part_ext =
                    if self.opts.single_source { file.part_ext.clone() } else { part.clone() };
                let chunk = Chunk { file: index, section, part_ext, range: item.range, start: item.start, kind };
                self.add_chunk(chunk);
            }
        }
    }

    fn add_chunk(&mut self, mut chunk: Chunk) {
        let index = self.chunks.len();
        if let ChunkKind::Function(f) = &chunk.kind {
            if f.head.class.is_empty() {
                self.free_funcs.push(index);
            } else if let Some(&class) = self.class_names.get(&f.head.class) {
                self.classes[class].funcs.push(index);
            }
            self.register_function(index, &mut chunk);
        }
        match chunk.section {
            SectionKind::Interface => self.interface.push(index),
            SectionKind::Implementation => self.implementation.push(index),
        }
        self.chunks.push(chunk);
    }

    /// IMPLEMENT_DEFAULT / IMPLEMENT_OVERRIDE bookkeeping.
    fn register_function(&mut self, index: usize, chunk: &mut Chunk) {
        let (file, f) = match &mut chunk.kind {
            ChunkKind::Function(f) => (chunk.file, f),
            _ => return,
        };
        let name = f.head.full_name();
        let Some(&existing) = self.functions.get(&name) else {
            self.functions.insert(name, index);
            return;
        };
        let (existing_default, existing_override) = {
            let e = self.chunks[existing].function().unwrap();
            (e.default_impl, e.override_impl)
        };
        if existing_default {
            self.printed.insert(Obj::Chunk(existing));
            f.overrides_default = Some(existing);
            if !f.override_impl {
                let message = format!(
                    "Function {name} overrides an IMPLEMENT_DEFAULT function but does not use IMPLEMENT_OVERRIDE"
                );
                let start = f.head.start;
                self.diag(file, start, Severity::Warning, message);
            }
        } else if f.default_impl {
            self.printed.insert(Obj::Chunk(index));
            if let ChunkKind::Function(e) = &mut self.chunks[existing].kind {
                e.overrides_default = Some(index);
            }
            self.functions.insert(name.clone(), index);
            if !existing_override {
                let file = self.chunks[existing].file;
                let start = self.chunks[existing].start;
                let message = format!(
                    "Function {name} overrides an IMPLEMENT_DEFAULT function but does not use IMPLEMENT_OVERRIDE"
                );
                self.diag(file, start, Severity::Warning, message);
            }
        }
    }

    fn function(&mut self, file: usize, section: SectionKind, head: FunctionHead) -> ChunkKind {
        let start = head.start;
        let blank_fpp = |head: &FunctionHead| {
            let mut blank: Vec<_> = head.visibility.iter().map(|v| v.1.clone()).collect();
            blank.extend(head.needs.iter().map(|n| n.0.clone()));
            blank.extend(head.noexport.iter().cloned());
            blank.extend(head.always_inline.iter().cloned());
            blank
        };
        // The reference tool has a check for functions in INTERFACE sections,
        // but it never triggers (it tests the section of the read position,
        // which is never set). Such functions are printed in the header.
        let mut visibility = Visibility::Free;
        let mut default_impl = false;
        let mut override_impl = false;
        if let Some((spec, _)) = head.visibility.first() {
            match spec.as_str() {
                "PUBLIC" => visibility = Visibility::Public,
                "PROTECTED" => visibility = Visibility::Protected,
                "PRIVATE" => visibility = Visibility::Private,
                "IMPLEMENT" => visibility = Visibility::ImplementationOnly,
                "IMPLEMENT_DEFAULT" => {
                    visibility = Visibility::ImplementationOnly;
                    default_impl = true;
                }
                _ => {
                    visibility = Visibility::ImplementationOnly;
                    override_impl = true;
                }
            }
        }
        if head.visibility.len() > 1 {
            let message = "only one visibility attribute allowed at start of declaration";
            self.error(file, start, message.into());
        }
        let mut class_template = None;
        if !head.class.is_empty() {
            if visibility == Visibility::Free {
                visibility = Visibility::Private;
            }
            if head.class.contains("::") && !self.class_names.contains_key(&head.class) {
                // Define the nested class along the way, like the reference tool.
                let top = head.class.split("::").next().unwrap();
                let section = self
                    .class_names
                    .get(top)
                    .map_or(section, |&c| self.classes[c].section);
                let class = self.classes.len();
                self.class_names.insert(head.class.clone(), class);
                let chunk = self.chunks.len();
                self.add_chunk(Chunk {
                    file,
                    section,
                    part_ext: self.files[file].part_ext.clone(),
                    range: start..start,
                    start,
                    kind: ChunkKind::Class(class),
                });
                self.classes.push(Class {
                    def: ClassDef {
                        name: head.class.clone(),
                        syntax: ClassSyntax::Class,
                        is_template: !head.class_templateargs.is_empty(),
                        open: 0,
                        close: 0,
                        has_bases: false,
                    },
                    chunk: Some(chunk),
                    section,
                    nested: true,
                    funcs: Vec::new(),
                    extensions: Vec::new(),
                });
            }
            if head.class.contains("::") && visibility != Visibility::ImplementationOnly {
                let message =
                    "Limitation: Only predeclared members supported for nested classes.  Use IMPLEMENT";
                self.error(file, start, message.into());
            }
            match self.class_names.get(&head.class) {
                None => {
                    let message = format!("Class {} has not been declared", head.class);
                    self.error(file, start, message);
                    return ChunkKind::Code { blank: blank_fpp(&head) };
                }
                Some(&class) => {
                    if self.classes[class].def.is_template && !head.templates.is_empty() {
                        class_template = Some(0);
                    }
                }
            }
        } else if matches!(visibility, Visibility::Public | Visibility::Protected | Visibility::Private) {
            let message = format!("Visibility specifier for free function {}", head.name);
            self.error(file, start, message);
            visibility = Visibility::Free;
        }

        let template = !head.templates.is_empty();
        let fully_specialized = template && head.templates.iter().all(|t| t.empty);
        let always_inline = !head.always_inline.is_empty();
        let marked_inline = !head.inline.is_empty() || head.constexpr;
        let inline = always_inline
            || (marked_inline && (self.opts.inlines || head.constexpr || fully_specialized));
        let is_static = !head.statics.is_empty();
        let mut hide = !head.noexport.is_empty() && inline;
        if is_static && head.class.is_empty() {
            visibility = Visibility::Static;
            hide = true;
        }
        let needs = if inline { head.needs.iter().flat_map(|n| n.1.clone()).collect() } else { Vec::new() };
        ChunkKind::Function(Box::new(Function {
            head,
            visibility,
            default_impl,
            override_impl,
            overrides_default: None,
            inline,
            always_inline,
            is_static,
            hide,
            class_template,
            template,
            fully_specialized,
            needs,
        }))
    }

    fn tag_enabled_edits(&mut self, index: usize, file: &SourceFile) -> Vec<Edit> {
        let mut edits = Vec::new();
        for (call, expression) in chunks::tag_enabled_calls(file) {
            let enabled = self.opts.tags.clone().unwrap_or_default();
            match tags::evaluate(&file.text[expression], &enabled) {
                Ok(value) => edits.push(Edit { range: call, text: if value { "true" } else { "false" } }),
                Err(e) => {
                    let message = format!("invalid TAG_ENABLED expression: {e}");
                    self.error(index, call.start, message);
                }
            }
        }
        edits
    }

    // -----------------------------------------------------------------------
    // Emission

    /// Emit the lines covering `range` of `file`; text outside `range` and in
    /// blank edits becomes whitespace. `suffix` is appended to the last line.
    fn emit(
        &mut self,
        doc: &mut GeneratedDocument,
        file: usize,
        range: Range<usize>,
        edits: &[Edit],
        section: PreprocessSection,
        suffix: &str,
    ) {
        if range.is_empty() {
            return;
        }
        let source = &self.files[file];
        let mut edits: Vec<&Edit> = edits
            .iter()
            .chain(self.tag_edits[file].iter())
            .filter(|e| e.range.start < range.end && e.range.end > range.start)
            .collect();
        edits.sort_by_key(|e| (e.range.start, std::cmp::Reverse(e.range.end)));

        let first_line = source.line_of(range.start);
        let last_line = source.line_of(range.end - 1);
        let mut edit_index = 0;
        let mut skip_width = 0usize;
        let mut active_end = 0;
        for line in first_line..=last_line {
            let line_range = source.line_range(line);
            let mut out = String::with_capacity(line_range.len());
            for (offset, c) in source.text[line_range.clone()].char_indices() {
                let byte = line_range.start + offset;
                if c == '\n' || c == '\r' {
                    continue;
                }
                while edit_index < edits.len() && edits[edit_index].range.end <= byte {
                    edit_index += 1;
                }
                let blank = |out: &mut String| {
                    if c == '\t' {
                        out.push('\t');
                    } else {
                        out.push_str(&" ".repeat(utf16_width(c)));
                    }
                };
                if byte < range.start || byte >= range.end {
                    blank(&mut out);
                    continue;
                }
                if byte >= active_end {
                    if let Some(edit) = edits.get(edit_index).filter(|e| e.range.start <= byte) {
                        // Start of a new edit (overlapping later edits are ignored).
                        out.push_str(edit.text);
                        skip_width = edit.text.len();
                        active_end = edit.range.end;
                        edit_index += 1;
                    }
                }
                if byte < active_end {
                    let width = utf16_width(c);
                    if skip_width >= width {
                        skip_width -= width;
                    } else {
                        skip_width = 0;
                        blank(&mut out);
                    }
                    continue;
                }
                out.push(c);
            }
            let trimmed_len = out.trim_end().len();
            out.truncate(trimmed_len);
            let blank_line = out.is_empty();
            if line == last_line {
                out.push_str(suffix);
            }
            let boundary = line == first_line || line == last_line;
            let key = (file, line, section);
            let origin = if (blank_line && boundary) || self.claimed.contains(&key) {
                None
            } else {
                self.claimed.insert(key);
                Some(LineOrigin { section, file: &source.path, line: line as u32 })
            };
            doc.append_line(&out, origin);
        }
    }

    /// Emit `range`, inserting synthesized `prefix` lines directly before
    /// the line where the item itself starts (after its leading comments).
    #[allow(clippy::too_many_arguments)]
    fn emit_with_prefix(
        &mut self,
        doc: &mut GeneratedDocument,
        file: usize,
        range: Range<usize>,
        item_start: usize,
        prefix: &[String],
        edits: &[Edit],
        section: PreprocessSection,
        suffix: &str,
    ) {
        if prefix.is_empty() {
            return self.emit(doc, file, range, edits, section, suffix);
        }
        let source = &self.files[file];
        let split = source.line_range(source.line_of(item_start)).start.max(range.start);
        self.emit(doc, file, range.start..split, edits, section, "");
        for line in prefix {
            doc.append_generated(line);
        }
        self.emit(doc, file, split..range.end, edits, section, suffix);
    }

    fn section_kind(&self, chunk: usize) -> PreprocessSection {
        match self.chunks[chunk].section {
            SectionKind::Interface => PreprocessSection::Interface,
            SectionKind::Implementation => PreprocessSection::Implementation,
        }
    }

    fn print_code(&mut self, doc: &mut GeneratedDocument, chunk: usize) {
        if !self.printed.insert(Obj::Chunk(chunk)) {
            return;
        }
        let c = &self.chunks[chunk];
        let (file, range) = (c.file, c.range.clone());
        let edits: Vec<Edit> = match &c.kind {
            ChunkKind::Code { blank } => blank.iter().cloned().map(Edit::blank).collect(),
            _ => Vec::new(),
        };
        let section = self.section_kind(chunk);
        self.emit(doc, file, range, &edits, section, "");
    }

    /// Blank edits for the inline keywords of a function: keep exactly one
    /// `inline` if the function is inline (and not constexpr or ALWAYS_INLINE),
    /// returns whether `inline` must be added in front.
    fn inline_edits(f: &Function, keep_always_inline: bool, edits: &mut Vec<Edit>) -> bool {
        let keep_inline = f.inline && !f.head.constexpr;
        let mut inline = f.head.inline.iter();
        if keep_inline {
            inline.next();
        }
        edits.extend(inline.cloned().map(Edit::blank));
        if !keep_always_inline {
            edits.extend(f.head.always_inline.iter().cloned().map(Edit::blank));
        }
        keep_inline && f.head.inline.is_empty()
    }

    /// Template headers must precede all specifiers, but FPP allows
    /// `PUBLIC inline template<...>`. If a kept token precedes the (kept)
    /// template headers, they are moved to a synthesized line in front.
    fn hoist_templates(&self, chunk: usize, edits: &mut Vec<Edit>, strip_defaults: bool) -> Option<String> {
        let c = &self.chunks[chunk];
        let f = c.function()?;
        let blanked = |r: &Range<usize>, edits: &[Edit]| {
            edits.iter().any(|e| e.text.is_empty() && e.range.start <= r.start && r.end <= e.range.end)
        };
        let headers: Vec<_> = f.head.templates.iter().filter(|t| !blanked(&t.range, edits)).collect();
        let hoistable = |r: &Range<usize>| {
            headers.iter().any(|h| h.range.start <= r.start && r.end <= h.range.end)
                || f.head.attributes.contains(r)
        };
        // Template headers, then C++11 attributes, must come first.
        let last = headers.iter().map(|h| h.range.start).chain(f.head.attributes.iter().map(|a| a.start)).max()?;
        let text = &self.files[c.file].text;
        let linkage = |r: &Range<usize>| text[r.clone()].starts_with('"') || &text[r.clone()] == "extern";
        let preceded = f.head.pretext.iter().any(|r| {
            r.end <= last && !blanked(r, edits) && !hoistable(r) && !linkage(r)
        });
        if !preceded {
            return None;
        }
        let mut hoisted = Vec::new();
        for header in &headers {
            let mut header_text = String::new();
            let mut pos = header.range.start;
            if strip_defaults {
                for default in &header.defaults {
                    header_text.push_str(&text[pos..default.start]);
                    pos = default.end;
                }
            }
            header_text.push_str(&text[pos..header.range.end]);
            hoisted.push(header_text.split_whitespace().collect::<Vec<_>>().join(" "));
        }
        edits.extend(headers.iter().map(|h| Edit::blank(h.range.clone())));
        for attribute in &f.head.attributes {
            hoisted.push(text[attribute.clone()].split_whitespace().collect::<Vec<_>>().join(" "));
            edits.push(Edit::blank(attribute.clone()));
        }
        Some(hoisted.join(" "))
    }

    fn print_funcdecl(&mut self, doc: &mut GeneratedDocument, chunk: usize) {
        let f = self.chunks[chunk].function().unwrap();
        if f.visibility != Visibility::ImplementationOnly {
            let mut edits: Vec<Edit> = f.head.visibility.iter().map(|v| Edit::blank(v.1.clone())).collect();
            edits.extend(f.head.needs.iter().map(|n| Edit::blank(n.0.clone())));
            edits.extend(f.head.noexport.iter().cloned().map(Edit::blank));
            edits.extend(f.head.qualifier.iter().cloned().map(Edit::blank));
            edits.extend(f.head.func_templateargs.iter().cloned().map(Edit::blank));
            if let Some(index) = f.class_template {
                edits.push(Edit::blank(f.head.templates[index].range.clone()));
            }
            let add_inline = Self::inline_edits(f, false, &mut edits);
            let c = &self.chunks[chunk];
            let (file, range) = (c.file, c.range.start..f.head.decl_end);
            let mut prefix: Vec<String> = self.hoist_templates(chunk, &mut edits, false).into_iter().collect();
            if add_inline {
                prefix.push("inline".into());
            }
            let start = self.chunks[chunk].start;
            self.emit_with_prefix(doc, file, range, start, &prefix, &edits, PreprocessSection::Declaration, ";");
        }
        if self.chunks[chunk].function().unwrap().inline {
            self.handle_inline(chunk);
        }
    }

    fn print_funcdef(&mut self, doc: &mut GeneratedDocument, chunk: usize) {
        let c = &self.chunks[chunk];
        let f = c.function().unwrap();
        if f.override_impl && f.overrides_default.is_none() {
            let message = format!("No default implementation for {} found", f.head.full_name());
            let (file, start) = (c.file, f.head.start);
            self.error(file, start, message);
        }
        let f = self.chunks[chunk].function().unwrap();
        if f.head.assign_spec.is_some() || !self.printed.insert(Obj::Chunk(chunk)) {
            return;
        }
        let c = &self.chunks[chunk];
        let mut edits: Vec<Edit> = f.head.visibility.iter().map(|v| Edit::blank(v.1.clone())).collect();
        edits.extend(f.head.needs.iter().map(|n| Edit::blank(n.0.clone())));
        edits.extend(f.head.noexport.iter().cloned().map(Edit::blank));
        edits.extend(f.head.virtuals.iter().cloned().map(Edit::blank));
        edits.extend(f.head.explicits.iter().cloned().map(Edit::blank));
        edits.extend(f.head.virt_specs.iter().cloned().map(Edit::blank));
        edits.extend(f.head.default_args.iter().cloned().map(Edit::blank));
        for template in &f.head.templates {
            edits.extend(template.defaults.iter().cloned().map(Edit::blank));
        }
        if !f.head.class.is_empty() {
            edits.extend(f.head.statics.iter().cloned().map(Edit::blank));
        }
        let add_inline = Self::inline_edits(f, f.inline, &mut edits);
        let (file, range, start) = (c.file, c.range.clone(), c.start);
        let mut prefix: Vec<String> = self.hoist_templates(chunk, &mut edits, true).into_iter().collect();
        if add_inline {
            prefix.push("inline".into());
        }
        let section = self.section_kind(chunk);
        self.emit_with_prefix(doc, file, range, start, &prefix, &edits, section, "");
    }

    fn print_classdecl(&mut self, doc: &mut GeneratedDocument, class: usize) {
        let chunk = self.classes[class].chunk.expect("classes have chunks");
        if !self.printed.insert(Obj::Chunk(chunk)) {
            return;
        }
        if self.classes[class].nested {
            // Nested classes only get their (implementation-only) functions processed.
            for f in self.classes[class].funcs.clone() {
                self.print_funcdecl(doc, f);
            }
            return;
        }
        let c = &self.chunks[chunk];
        let (file, range) = (c.file, c.range.clone());
        let section = self.section_kind(chunk);
        let def = self.classes[class].def.clone();
        if def.syntax != ClassSyntax::Class {
            self.emit(doc, file, range, &[], section, "");
            return;
        }

        // Without -i, `inline` is dropped from class bodies like in the reference tool.
        let body_edits: Vec<Edit> = if self.opts.inlines {
            Vec::new()
        } else {
            let code = &self.files[file].code;
            code[def.open..def.close]
                .match_indices("inline")
                .map(|(i, _)| def.open + i)
                .filter(|&i| {
                    let word = |b: Option<&u8>| b.is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_');
                    !word(code.as_bytes().get(i.wrapping_sub(1))) && !word(code.as_bytes().get(i + 6))
                })
                .map(|i| Edit::blank(i..i + 6))
                .collect()
        };
        let extensions = self.classes[class].extensions.clone();
        let extension_bases = extensions.iter().any(|(_, e)| e.colon.is_some());
        if extension_bases {
            self.emit(doc, file, range.start..def.open, &[], section, "");
            let mut has_bases = def.has_bases;
            for (ext_file, extension) in &extensions {
                if let Some(colon) = extension.colon {
                    let edits =
                        if has_bases { vec![Edit { range: colon..colon + 1, text: "," }] } else { vec![] };
                    self.emit(doc, *ext_file, colon..extension.open, &edits, PreprocessSection::Implementation, "");
                    has_bases = true;
                }
            }
            self.emit(doc, file, def.open..def.close, &body_edits, section, "");
        } else {
            self.emit(doc, file, range.start..def.close, &body_edits, section, "");
        }
        for (ext_file, extension) in &extensions {
            doc.append_generated("private: // EXTENSION");
            self.emit(
                doc,
                *ext_file,
                extension.open + 1..extension.close,
                &[],
                PreprocessSection::Implementation,
                "",
            );
        }

        let funcs = self.classes[class].funcs.clone();
        let visibility = |s: &Self, f: usize| s.chunks[f].function().unwrap().visibility;
        for (label, vis) in [
            ("public:", Visibility::Public),
            ("protected:", Visibility::Protected),
            ("private:", Visibility::Private),
        ] {
            doc.head(label);
            doc.head("\n");
            let selected: Vec<usize> = funcs.iter().copied().filter(|&f| visibility(self, f) == vis).collect();
            for f in selected {
                self.print_funcdecl(doc, f);
            }
            doc.clear_head();
        }
        let selected: Vec<usize> =
            funcs.iter().copied().filter(|&f| visibility(self, f) == Visibility::ImplementationOnly).collect();
        for f in selected {
            self.print_funcdecl(doc, f);
        }
        self.emit(doc, file, def.close..range.end, &[], section, "");
    }

    // -----------------------------------------------------------------------
    // Inline functions

    fn obj_name(&self, obj: Obj) -> String {
        match obj {
            Obj::Class(c) => self.classes[c].def.name.clone(),
            Obj::Chunk(c) => match &self.chunks[c].kind {
                ChunkKind::Function(f) => f.head.full_name(),
                _ => "<chunk>".into(),
            },
        }
    }

    fn class_obj(&self, class: usize) -> Obj {
        self.classes[class].chunk.map_or(Obj::Class(class), Obj::Chunk)
    }

    fn lookup_by_name(&mut self, item: &str, context: usize) -> Vec<Obj> {
        if let Some(&class) = self.class_names.get(item) {
            return vec![self.class_obj(class)];
        }
        if let Some(&include) = self.includes.get(item) {
            self.include_inline.insert(include);
            return vec![Obj::Chunk(include)];
        }
        let (class, name) = item.split_once("::").unwrap_or(("", item));
        let name = name.split("::").next().unwrap_or(name);
        let funcs = if class.is_empty() {
            self.free_funcs.clone()
        } else {
            self.class_names.get(class).map(|&c| self.classes[c].funcs.clone()).unwrap_or_default()
        };
        let named: Vec<usize> =
            funcs.into_iter().filter(|&f| self.chunks[f].function().unwrap().head.name == name).collect();
        let inline: Vec<Obj> = named
            .iter()
            .filter(|&&f| self.chunks[f].function().unwrap().inline)
            .map(|&f| Obj::Chunk(f))
            .collect();
        if inline.is_empty() {
            let (file, start) = (self.chunks[context].file, self.chunks[context].start);
            if named.is_empty() {
                self.error(file, start, format!("Cannot find {item}"));
            } else if self.opts.warn_needs {
                self.diag(file, start, Severity::Warning, format!("Cannot find inline code for {item}"));
            }
        }
        inline
    }

    fn function_class(&self, f: &Function) -> Option<&Class> {
        self.class_names.get(&f.head.class).map(|&c| &self.classes[c])
    }

    fn inline_known(&mut self, item: Obj, function: usize) -> bool {
        let f = self.chunks[function].function().unwrap();
        if let Obj::Chunk(c) = item {
            if let Some(i) = self.chunks[c].function() {
                if i.hide && !f.hide {
                    let message = format!(
                        "Nonhidden function {} depends on hidden function {} ({})",
                        f.head.full_name(),
                        i.head.full_name(),
                        if i.visibility == Visibility::Static { "static" } else { "NOEXPORT" }
                    );
                    let (file, start) = (self.chunks[function].file, f.head.start);
                    self.error(file, start, message);
                }
            }
        }
        let f = self.chunks[function].function().unwrap();
        let private_context = f.visibility == Visibility::Private
            || self.function_class(f).is_some_and(|c| c.section == SectionKind::Implementation);
        self.public_inline.contains(&item)
            || (private_context && self.private_inline.contains(&item))
            || (f.hide && self.unit_inline.contains(&item))
    }

    fn needs_of(&self, obj: Obj) -> Vec<String> {
        match obj {
            Obj::Chunk(c) => self.chunks[c].function().map(|f| f.needs.clone()).unwrap_or_default(),
            Obj::Class(_) => Vec::new(),
        }
    }

    /// Put an inline function and everything it NEEDS into the public,
    /// private or unit inline order, dependencies first.
    fn handle_inline(&mut self, function: usize) {
        let f = self.chunks[function].function().unwrap();
        let class = f.head.class.clone();
        let mut needed = Vec::new();
        for item in f.needs.clone() {
            needed.extend(self.lookup_by_name(&item, function));
        }
        needed.push(Obj::Chunk(function));
        if !class.is_empty() {
            let mut lookup = self.lookup_by_name(&class, function);
            lookup.extend(needed);
            needed = lookup;
        }
        let mut guard = 0;
        'needed: while let Some(&object) = needed.first() {
            guard += 1;
            if guard > 100_000 || self.fatal {
                break;
            }
            if self.inline_known(object, function) {
                needed.remove(0);
                continue;
            }
            let mut more = Vec::new();
            if let Obj::Chunk(c) = object {
                if let Some(of) = self.chunks[c].function() {
                    if !of.head.class.is_empty() {
                        let class = of.head.class.clone();
                        more.extend(self.lookup_by_name(&class, c));
                    }
                }
                // The reference tool looks these up in scalar context, so
                // ambiguous names (overloads) are skipped.
                for item in self.needs_of(object) {
                    let found = self.lookup_by_name(&item, c);
                    if found.len() == 1 {
                        more.extend(found);
                    }
                }
            }
            for dependency in more {
                if self.inline_known(dependency, function) {
                    continue;
                }
                if dependency == Obj::Chunk(function) {
                    let message = format!(
                        "Function {} NEEDS {}, which circularly depends on this function",
                        self.obj_name(object),
                        self.obj_name(dependency)
                    );
                    let (file, start) = match object {
                        Obj::Chunk(c) => (self.chunks[c].file, self.chunks[c].start),
                        Obj::Class(_) => (self.chunks[function].file, self.chunks[function].start),
                    };
                    self.error(file, start, message);
                    self.fatal = true;
                    break 'needed;
                }
                needed.insert(0, dependency);
                continue 'needed;
            }
            let f = self.chunks[function].function().unwrap();
            let private_context = f.visibility == Visibility::Private
                || (!class.is_empty()
                    && self.function_class(f).is_some_and(|c| c.section == SectionKind::Implementation));
            if f.hide {
                self.unit_inline.insert(object);
                self.order_unit.push(object);
            } else if private_context {
                self.private_inline.insert(object);
                self.order_private.push(object);
            } else {
                self.public_inline.insert(object);
                self.order_public.push(object);
            }
            needed.remove(0);
        }
    }

    fn print_inlines(&mut self, doc: &mut GeneratedDocument, objects: &[Obj]) {
        for &object in objects {
            let class = match object {
                Obj::Class(c) => Some(c),
                Obj::Chunk(c) => match self.chunks[c].kind {
                    ChunkKind::Class(class) => Some(class),
                    _ => None,
                },
            };
            if let Some(class) = class {
                if self.classes[class].section != SectionKind::Interface {
                    self.print_classdecl(doc, class);
                }
            }
        }
        for &object in objects {
            if let Obj::Chunk(c) = object {
                if self.chunks[c].function().is_some() {
                    self.print_funcdef(doc, c);
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Output files

    fn is_forward(&self, chunk: usize) -> bool {
        matches!(self.chunks[chunk].kind, ChunkKind::Class(c) if self.classes[c].def.syntax == ClassSyntax::ForwardDecl)
    }

    fn class_in_interface_or_public(&self, f: &Function) -> bool {
        self.class_names.get(&f.head.class).is_some_and(|&c| {
            self.classes[c].section == SectionKind::Interface
                || self.public_inline.contains(&self.class_obj(c))
        })
    }

    fn print(&mut self) -> Vec<Output> {
        let opts = self.opts;
        let (out_base, header_base) = match &opts.out_base {
            Some(o) => (o.clone(), o.clone()),
            None => (opts.prefix.clone() + &opts.base, opts.prefix.clone() + &self.public_base),
        };
        let h = &opts.header_ext;
        let banner = "// AUTOMATICALLY GENERATED -- DO NOT EDIT!         -*- c++ -*-\n\n";

        // Public header.
        let mut header = GeneratedDocument::new(format!("{header_base}.{h}"));
        header.append_generated(banner);
        header.append_generated("#pragma once");
        for chunk in self.interface.clone() {
            if matches!(self.chunks[chunk].kind, ChunkKind::Include) {
                self.print_code(&mut header, chunk);
            }
        }
        header.head("\n//\n// INTERFACE definition follows\n//\n\n");
        let interface: Vec<usize> = self
            .interface
            .iter()
            .copied()
            .filter(|&c| self.is_forward(c))
            .chain(self.interface.iter().copied().filter(|&c| !self.is_forward(c)))
            .collect();
        for chunk in interface {
            match self.chunks[chunk].kind {
                ChunkKind::Code { .. } => self.print_code(&mut header, chunk),
                ChunkKind::Class(class) => self.print_classdecl(&mut header, class),
                ChunkKind::Function(_) => self.print_funcdef(&mut header, chunk),
                ChunkKind::Include => {}
            }
        }
        for chunk in self.implementation.clone() {
            if let Some(f) = self.chunks[chunk].function() {
                if f.head.class.is_empty() && !f.is_static {
                    self.print_funcdecl(&mut header, chunk);
                }
            }
        }
        let public_templates: Vec<usize> = self
            .implementation
            .iter()
            .copied()
            .filter(|&c| {
                self.chunks[c].function().is_some_and(|f| {
                    f.template
                        && !f.fully_specialized
                        && !self.public_inline.contains(&Obj::Chunk(c))
                        && (f.visibility == Visibility::Free
                            || (!f.head.class.is_empty() && self.class_in_interface_or_public(f)))
                })
            })
            .collect();
        if !self.public_inline.is_empty() || !public_templates.is_empty() {
            let all_includes = !public_templates.is_empty();
            header.clear_head();
            header.head("\n//\n// IMPLEMENTATION includes follow (for use by inline functions/templates)\n//\n\n");
            for chunk in self.implementation.clone() {
                if matches!(self.chunks[chunk].kind, ChunkKind::Include)
                    && (all_includes || self.include_inline.contains(&chunk))
                {
                    self.print_code(&mut header, chunk);
                }
            }
            header.clear_head();
            header.head("\n//\n// IMPLEMENTATION of inline functions (and needed classes)\n//\n\n");
            let order = self.order_public.clone();
            self.print_inlines(&mut header, &order);
        }
        header.clear_head();
        header.head("\n//\n// IMPLEMENTATION of function templates\n//\n\n");
        for chunk in public_templates {
            self.print_funcdef(&mut header, chunk);
        }
        header.clear_head();

        // Internal header.
        let mut internal = GeneratedDocument::new(format!("{out_base}_i.{h}"));
        internal.append_generated(banner);
        internal.append_generated("#pragma once");
        internal.append_generated(&format!("#include \"{}.{h}\"", self.public_base));
        for chunk in self.implementation.clone() {
            if matches!(self.chunks[chunk].kind, ChunkKind::Include) {
                self.print_code(&mut internal, chunk);
            }
        }
        internal.head("\n//\n// INTERFACE internal declaration follows\n//\n\n");
        let classes: Vec<usize> = self
            .implementation
            .iter()
            .copied()
            .filter(|&c| self.is_forward(c))
            .chain(self.implementation.iter().copied().filter(|&c| {
                matches!(self.chunks[c].kind, ChunkKind::Class(_)) && !self.is_forward(c)
            }))
            .collect();
        for chunk in classes {
            if let ChunkKind::Class(class) = self.chunks[chunk].kind {
                self.print_classdecl(&mut internal, class);
            }
        }
        internal.clear_head();
        internal.head("\n//\n// IMPLEMENTATION of inline functions follows\n//\n\n");
        let order = self.order_private.clone();
        self.print_inlines(&mut internal, &order);
        internal.clear_head();
        internal.head("\n//\n// IMPLEMENTATION of function templates\n//\n\n");
        let private_templates: Vec<usize> = self
            .implementation
            .iter()
            .copied()
            .filter(|&c| {
                self.chunks[c].function().is_some_and(|f| {
                    f.template
                        && !f.fully_specialized
                        && !self.public_inline.contains(&Obj::Chunk(c))
                        && !self.private_inline.contains(&Obj::Chunk(c))
                        && (f.visibility == Visibility::Static
                            || (!f.head.class.is_empty() && !self.class_in_interface_or_public(f)))
                })
            })
            .collect();
        for chunk in private_templates {
            self.print_funcdef(&mut internal, chunk);
        }
        internal.clear_head();

        let mut outputs = vec![
            Output { kind: OutputKind::Header, document: header, truncated: false },
            Output { kind: OutputKind::Internal, document: internal, truncated: false },
        ];

        // Implementation files.
        for part in self.impl_parts.clone() {
            let suffix = if part.is_empty() { String::new() } else { format!("-{part}") };
            let mut doc = GeneratedDocument::new(format!("{out_base}{suffix}.{}", opts.source_ext));
            let mut empty = true;
            doc.append_generated(banner);
            doc.append_generated(&format!(
                "#include \"{}.{h}\"\n#include \"{}_i.{h}\"\n",
                self.public_base, opts.base
            ));
            doc.head("\n//\n// INTERFACE implementation function declaration follows\n//\n\n");
            let in_part: Vec<usize> =
                self.implementation.iter().copied().filter(|&c| self.chunks[c].part_ext == part).collect();
            for &chunk in &in_part {
                if let Some(f) = self.chunks[chunk].function() {
                    if f.head.class.is_empty() && f.is_static {
                        self.print_funcdecl(&mut doc, chunk);
                    }
                }
            }
            doc.clear_head();
            doc.head("\n//\n// IMPLEMENTATION follows\n//\n\n");
            for &chunk in &in_part {
                if matches!(self.chunks[chunk].kind, ChunkKind::Code { .. }) {
                    self.print_code(&mut doc, chunk);
                    empty = false;
                }
            }
            let unit: Vec<Obj> = self
                .order_unit
                .clone()
                .into_iter()
                .filter(|o| match o {
                    Obj::Chunk(c) => self.chunks[*c].part_ext == part,
                    Obj::Class(_) => false,
                })
                .collect();
            self.print_inlines(&mut doc, &unit);
            for &chunk in &in_part {
                if let Some(f) = self.chunks[chunk].function() {
                    if f.template && !f.fully_specialized {
                        continue;
                    }
                    self.print_funcdef(&mut doc, chunk);
                    empty = false;
                }
            }
            doc.clear_head();
            let truncated = empty && opts.truncate_empty;
            outputs.push(Output { kind: OutputKind::Implementation, document: doc, truncated });
        }
        outputs
    }
}

/// Convenience wrapper: read and preprocess `paths`.
pub fn generate_from_paths(paths: &[PathBuf], opts: &Options) -> color_eyre::Result<Generated> {
    let files = paths
        .iter()
        .map(|p| {
            let text = std::fs::read_to_string(p)
                .map_err(|e| color_eyre::eyre::eyre!("Cannot open {} for reading: {e}", p.display()))?;
            SourceFile::new(p.clone(), &text)
        })
        .collect::<color_eyre::Result<Vec<_>>>()?;
    Ok(generate(&files, opts))
}
