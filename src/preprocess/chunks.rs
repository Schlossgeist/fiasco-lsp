//! Split a parsed source file into the chunks the reference tool works with.
//!
//! tree-sitter provides the item boundaries; function heads are analyzed on
//! their token sequence (similar to the regular expressions of the reference
//! tool), which keeps the analysis robust against macros like `FIASCO_INIT`
//! that tree-sitter cannot classify correctly.

use std::ops::Range;

use tree_sitter::Node;

use super::source::SourceFile;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionKind {
    Interface,
    Implementation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassSyntax {
    Class,
    Enum,
    Typedef,
    ForwardDecl,
}

#[derive(Debug, Clone)]
pub struct ClassDef {
    pub name: String,
    pub syntax: ClassSyntax,
    pub is_template: bool,
    /// Byte offsets of the opening and the final closing brace (class syntax only).
    pub open: usize,
    pub close: usize,
    pub has_bases: bool,
}

#[derive(Debug, Clone)]
pub struct Extension {
    pub name: String,
    /// Byte offset of the `:` of the base clause, if any.
    pub colon: Option<usize>,
    pub open: usize,
    pub close: usize,
}

#[derive(Debug, Clone, Default)]
pub struct TemplateHeader {
    /// From `template` to the end of the parameter list.
    pub range: Range<usize>,
    pub empty: bool,
    pub defaults: Vec<Range<usize>>,
}

/// Everything the generator needs to know about a function head.
#[derive(Debug, Clone, Default)]
pub struct FunctionHead {
    pub name: String,
    pub class: String,
    pub class_templateargs: String,
    /// `Class::` (including `::`), blanked in in-class declarations.
    pub qualifier: Option<Range<usize>>,
    pub visibility: Vec<(String, Range<usize>)>,
    pub inline: Vec<Range<usize>>,
    pub constexpr: bool,
    pub always_inline: Vec<Range<usize>>,
    pub noexport: Vec<Range<usize>>,
    pub needs: Vec<(Range<usize>, Vec<String>)>,
    pub statics: Vec<Range<usize>>,
    pub virtuals: Vec<Range<usize>>,
    pub explicits: Vec<Range<usize>>,
    pub virt_specs: Vec<Range<usize>>,
    pub templates: Vec<TemplateHeader>,
    /// Tokens before the name/qualifier (return type, specifiers, ...).
    pub pretext: Vec<Range<usize>>,
    /// C++11 attributes (`[[...]]`) in the pretext.
    pub attributes: Vec<Range<usize>>,
    pub default_args: Vec<Range<usize>>,
    /// Explicit template arguments after the name (`f<int>`), not part of declarations.
    pub func_templateargs: Option<Range<usize>>,
    /// `0`, `default` or `delete` for declarations without a body.
    pub assign_spec: Option<String>,
    /// End of the text that forms the declaration (without `;`).
    pub decl_end: usize,
    /// Start of the actual item (after leading comments).
    pub start: usize,
}

impl FunctionHead {
    pub fn full_name(&self) -> String {
        if self.class.is_empty() {
            self.name.clone()
        } else {
            format!("{}::{}", self.class, self.name)
        }
    }
}

#[derive(Debug, Clone)]
pub enum ItemKind {
    Section { kind: SectionKind, condition: Option<String> },
    Include(String),
    Code,
    /// Comments directly before a section header.
    SectionComment,
    Class(ClassDef),
    Extension(Extension),
    Function(Box<FunctionHead>),
}

#[derive(Debug, Clone)]
pub struct Item {
    /// The text belonging to this item, including leading comments and the
    /// rest of the last line.
    pub range: Range<usize>,
    /// Start of the item itself.
    pub start: usize,
    pub kind: ItemKind,
}

struct Flat<'t> {
    range: Range<usize>,
    kind: FlatKind<'t>,
    /// Nodes of an incomplete head that tree-sitter split off (ERROR).
    prefix: Option<Node<'t>>,
}

enum FlatKind<'t> {
    Node(Node<'t>),
    Section,
    SectionComment,
    Directive,
    Verbatim,
}

/// The items of `file` in source order.
pub fn items(file: &SourceFile) -> Vec<Item> {
    let mut flat = Vec::new();
    let root = file.tree.root_node();
    flatten_children(file, root, &mut flat, true);
    merge_preprocess_off(file, root, &mut flat);
    balance_braces(file, &mut flat);

    let mut result = Vec::with_capacity(flat.len());
    let mut previous_end = 0;
    for (index, item) in flat.iter().enumerate() {
        // An item owns the rest of its last line, unless the next item starts there.
        let mut end = file.end_of_line(item.range.end.saturating_sub(1).max(item.range.start));
        if let Some(next) = flat.get(index + 1) {
            end = end.min(next.range.start).max(item.range.end);
        }
        let range = previous_end.min(item.range.start)..end;
        previous_end = end;
        let kind = match item.kind {
            FlatKind::Section => section(file, item),
            FlatKind::Directive | FlatKind::Verbatim => ItemKind::Code,
            FlatKind::SectionComment => ItemKind::SectionComment,
            FlatKind::Node(node) => classify(file, node, item),
        };
        result.push(Item { range, start: item.range.start, kind });
    }
    result
}

fn section(file: &SourceFile, item: &Flat<'_>) -> ItemKind {
    let text = &file.text[item.range.clone()];
    let kind = if text.trim_start().starts_with("INTERFACE") {
        SectionKind::Interface
    } else {
        SectionKind::Implementation
    };
    let condition = text
        .split_once('[')
        .and_then(|(_, rest)| rest.rsplit_once(']'))
        .map(|(condition, _)| condition.trim().to_owned());
    ItemKind::Section { kind, condition }
}

fn flatten_children<'t>(file: &'t SourceFile, node: Node<'t>, out: &mut Vec<Flat<'t>>, resplit: bool) {
    let mut cursor = node.walk();
    let children: Vec<_> = node.children(&mut cursor).collect();
    let mut directive_end = 0;
    let mut pending_comments: Option<Range<usize>> = None;
    for child in children {
        if child.kind() == "comment" {
            let range = pending_comments.take().map_or(child.byte_range(), |r| r.start..child.end_byte());
            // `//-` ends a chunk: it and the comments before it are code.
            if file.node_text(child).strip_prefix("//-").is_some_and(|rest| rest.trim().is_empty()) {
                out.push(Flat { range, kind: FlatKind::Verbatim, prefix: None });
            } else {
                pending_comments = Some(range);
            }
            continue;
        }
        // Comments directly before a section header form a code chunk of the
        // previous section in the reference tool.
        let comments = pending_comments.take();
        if child.kind() == "fiasco_section" {
            if let Some(range) = comments {
                out.push(Flat { range, kind: FlatKind::SectionComment, prefix: None });
            }
        }
        if child.start_byte() < directive_end {
            // Part of a preprocessor directive line (condition, newline token).
            continue;
        }
        let kind = child.kind();
        if !child.is_named() && kind.starts_with('#') {
            let end = file.end_of_line(child.start_byte());
            directive_end = end;
            let end = if kind == "#endif" { child.end_byte() } else { end };
            out.push(Flat { range: child.start_byte()..end, kind: FlatKind::Directive, prefix: None });
        } else if (kind == ";" && !child.is_named())
            || (file.code[child.byte_range()].trim() == ";" && matches!(out.last().map(|l| &l.kind), Some(FlatKind::Node(_))))
        {
            if let Some(last) = out.last_mut() {
                if matches!(last.kind, FlatKind::Node(_)) {
                    last.range.end = child.end_byte();
                    continue;
                }
            }
            out.push(Flat { range: child.byte_range(), kind: FlatKind::Verbatim, prefix: None });
        } else if child.byte_range().is_empty() {
            continue;
        } else if kind == "fiasco_section" {
            out.push(Flat { range: child.byte_range(), kind: FlatKind::Section, prefix: None });
        } else if matches!(
            kind,
            "preproc_if" | "preproc_ifdef" | "preproc_else" | "preproc_elif" | "preproc_elifdef"
        ) || (kind == "ERROR" && has_section_child(child))
        {
            flatten_children(file, child, out, resplit);
        } else if resplit && file.pieces.iter().any(|(origin, _)| *origin == child.byte_range()) {
            for (origin, piece) in &file.pieces {
                if *origin == child.byte_range() {
                    flatten_children(file, piece.root_node(), out, false);
                }
            }
        } else if !child.is_named() || kind == "ERROR" {
            out.push(Flat { range: child.byte_range(), kind: FlatKind::Verbatim, prefix: None });
        } else {
            // tree-sitter splits heads containing unknown macros, e.g.
            // `T * FIASCO_FLATTEN X::f() {}`, into an ERROR (or an incomplete
            // declaration) and the remaining function.
            if let Some(last) = out.last() {
                let error = match &last.kind {
                    FlatKind::Verbatim => node_at(node, last.range.clone())
                        .or_else(|| piece_node_at(file, last.range.clone()))
                        .filter(|e| e.kind() == "ERROR"),
                    FlatKind::Node(n)
                        if last.prefix.is_none() && matches!(n.kind(), "template_declaration" | "declaration") =>
                    {
                        Some(*n)
                    }
                    _ => None,
                };
                let head_like = !file.code[last.range.clone()].contains([';', '{', '}']);
                let gap_blank = file.code[last.range.end..child.start_byte()].trim().is_empty();
                if let (Some(error), true, true) = (error, head_like, gap_blank) {
                    if matches!(kind, "function_definition" | "declaration" | "template_declaration" | "compound_statement") {
                        let start = last.range.start;
                        out.pop();
                        out.push(Flat { range: start..child.end_byte(), kind: FlatKind::Node(child), prefix: Some(error) });
                        continue;
                    }
                }
            }
            out.push(Flat { range: child.byte_range(), kind: FlatKind::Node(child), prefix: None });
        }
    }
}

/// tree-sitter ends items early in some cases, e.g. at a multi-line `#define`
/// inside a function body. Like the reference tool, extend items until their
/// braces balance, but never across a section header.
fn balance_braces(file: &SourceFile, flat: &mut Vec<Flat<'_>>) {
    let code = file.code.as_bytes();
    let mut index = 0;
    while index < flat.len() {
        if !matches!(flat[index].kind, FlatKind::Node(_)) {
            index += 1;
            continue;
        }
        let limit = flat[index + 1..]
            .iter()
            .find(|f| matches!(f.kind, FlatKind::Section))
            .map_or(code.len(), |f| f.range.start);
        let mut depth = 0i32;
        let mut end = flat[index].range.end;
        for &b in &code[flat[index].range.clone()] {
            match b {
                b'{' => depth += 1,
                b'}' => depth -= 1,
                _ => {}
            }
        }
        if depth > 0 {
            for (offset, &b) in code[end..limit].iter().enumerate() {
                match b {
                    b'{' => depth += 1,
                    b'}' => depth -= 1,
                    _ => {}
                }
                if depth == 0 {
                    end += offset + 1;
                    // Include a directly following `;`.
                    let rest = &file.code[end..limit];
                    if rest.trim_start().starts_with(';') {
                        end += rest.find(';').unwrap() + 1;
                    }
                    break;
                }
            }
            if depth == 0 && end > flat[index].range.end {
                flat[index].range.end = end;
                let absorbed = flat[index + 1..].iter().take_while(|f| f.range.start < end).count();
                flat.drain(index + 1..index + 1 + absorbed);
            }
        }
        index += 1;
    }
}

fn piece_node_at<'t>(file: &'t SourceFile, range: Range<usize>) -> Option<Node<'t>> {
    file.pieces.iter().find_map(|(_, t)| node_at(t.root_node(), range.clone()))
}

fn node_at<'t>(parent: Node<'t>, range: Range<usize>) -> Option<Node<'t>> {
    let mut cursor = parent.walk();
    let found = parent.children(&mut cursor).find(|c| c.byte_range() == range);
    found
}

use super::source::has_section_child;

/// `// preprocess off` ... `// preprocess on` regions are copied verbatim.
fn merge_preprocess_off(file: &SourceFile, root: Node<'_>, flat: &mut Vec<Flat<'_>>) {
    let mut markers = Vec::new();
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if child.kind() == "comment" {
            let text = file.node_text(child);
            if let Some(rest) = text.strip_prefix("//") {
                match rest.trim() {
                    "preprocess off" => markers.push((child.start_byte(), true)),
                    "preprocess on" => markers.push((child.end_byte(), false)),
                    _ => {}
                }
            }
        }
    }
    let mut iter = markers.into_iter();
    while let Some((start, true)) = iter.by_ref().find(|m| m.1) {
        let Some((end, _)) = iter.by_ref().find(|m| !m.1) else { break };
        flat.retain(|f| f.range.end <= start || f.range.start >= end);
        let index = flat.partition_point(|f| f.range.start < start);
        flat.insert(index, Flat { range: start..end, kind: FlatKind::Verbatim, prefix: None });
    }
}

fn compact(text: &str) -> String {
    text.split_whitespace().collect()
}

fn classify(file: &SourceFile, node: Node<'_>, item: &Flat<'_>) -> ItemKind {
    if item.prefix.is_some() {
        return function(file, node, item).map_or(ItemKind::Code, |f| ItemKind::Function(Box::new(f)));
    }
    match node.kind() {
        "preproc_include" => {
            let path = node.child_by_field_name("path").map(|p| compact(file.node_text(p)));
            path.map_or(ItemKind::Code, ItemKind::Include)
        }
        "class_specifier" | "struct_specifier" | "enum_specifier" => {
            class_def(file, node, item, false).map_or(ItemKind::Code, ItemKind::Class)
        }
        "type_definition" => typedef_name(file, node)
            .map(|name| {
                ItemKind::Class(ClassDef {
                    name,
                    syntax: ClassSyntax::Typedef,
                    is_template: false,
                    open: 0,
                    close: 0,
                    has_bases: false,
                })
            })
            .unwrap_or(ItemKind::Code),
        "fiasco_extension" => extension(file, node).map_or(ItemKind::Code, ItemKind::Extension),
        "template_declaration" => {
            let inner = last_named_child(node);
            match inner.map(|n| n.kind()) {
                Some("class_specifier" | "struct_specifier")
                    if !has_child_kind(node, "fiasco_template_modifiers") =>
                {
                    class_def(file, inner.unwrap(), item, true)
                        .map_or(ItemKind::Code, ItemKind::Class)
                }
                Some("function_definition" | "declaration" | "template_declaration") => {
                    function(file, node, item).map_or(ItemKind::Code, |f| ItemKind::Function(Box::new(f)))
                }
                _ => ItemKind::Code,
            }
        }
        "declaration" => {
            if let Some(class) = node
                .child_by_field_name("type")
                .filter(|t| matches!(t.kind(), "class_specifier" | "struct_specifier"))
                .and_then(|t| class_def(file, t, item, false))
                .filter(|c| c.syntax == ClassSyntax::Class)
            {
                return ItemKind::Class(class);
            }
            function(file, node, item).map_or(ItemKind::Code, |f| ItemKind::Function(Box::new(f)))
        }
        "function_definition" => {
            function(file, node, item).map_or(ItemKind::Code, |f| ItemKind::Function(Box::new(f)))
        }
        // `extern "C" void f() {...}` is a free function for the reference tool.
        "linkage_specification"
            if node.child_by_field_name("body").is_some_and(|b| b.kind() == "function_definition") =>
        {
            function(file, node, item).map_or(ItemKind::Code, |f| ItemKind::Function(Box::new(f)))
        }
        _ => ItemKind::Code,
    }
}

fn last_named_child(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    let last = node.named_children(&mut cursor).filter(|c| c.kind() != "comment").last();
    last
}

fn has_child_kind(node: Node<'_>, kind: &str) -> bool {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).any(|c| c.kind() == kind);
    found
}

fn class_def(file: &SourceFile, node: Node<'_>, item: &Flat<'_>, is_template: bool) -> Option<ClassDef> {
    let name = node.child_by_field_name("name").map(|n| compact(file.node_text(n)))?;
    let body = node.child_by_field_name("body");
    let Some(body) = body else {
        // Only `class Name;` is a forward declaration; everything else is code.
        let text = compact(&file.code[item.range.clone()]);
        let forward = ["class", "struct"].iter().any(|k| {
            let decl = format!("{k}{name};");
            text == decl || (is_template && text.starts_with("template<") && text.ends_with(&format!(">{decl}")))
        });
        return forward.then_some(ClassDef {
            name,
            syntax: ClassSyntax::ForwardDecl,
            is_template,
            open: 0,
            close: 0,
            has_bases: false,
        });
    };
    let syntax = if node.kind() == "enum_specifier" { ClassSyntax::Enum } else { ClassSyntax::Class };
    let close = body.end_byte().checked_sub(1)?;
    if file.text.as_bytes()[close] != b'}' {
        return None;
    }
    Some(ClassDef {
        name,
        syntax,
        is_template,
        open: body.start_byte(),
        close,
        has_bases: has_child_kind(node, "base_class_clause"),
    })
}

fn typedef_name(file: &SourceFile, node: Node<'_>) -> Option<String> {
    let mut cursor = node.walk();
    let declarator = node.children_by_field_name("declarator", &mut cursor).last()?;
    let mut stack = vec![declarator];
    let mut name = None;
    while let Some(n) = stack.pop() {
        if n.kind() == "type_identifier" || n.kind() == "primitive_type" {
            name = Some(file.node_text(n).to_owned());
            break;
        }
        let mut c = n.walk();
        let children: Vec<_> = n.named_children(&mut c).collect();
        // Depth first, leftmost first: the declared name precedes array/function suffixes.
        stack.extend(children.into_iter().rev().filter(|c| c.kind() != "parameter_list"));
    }
    name
}

fn extension(file: &SourceFile, node: Node<'_>) -> Option<Extension> {
    let class = node.named_child(0)?;
    let name = class.child_by_field_name("name").map(|n| compact(file.node_text(n)))?;
    let body = class.child_by_field_name("body")?;
    let mut cursor = class.walk();
    let colon = class
        .children(&mut cursor)
        .find(|c| c.kind() == "base_class_clause")
        .map(|c| c.start_byte());
    Some(Extension { name, colon, open: body.start_byte(), close: body.end_byte() - 1 })
}

// ---------------------------------------------------------------------------
// Function heads

#[derive(Debug, Clone, PartialEq)]
enum TokKind {
    Word,
    Punct,
    Parens(Vec<Range<usize>>),
    TemplateParams(Vec<Range<usize>>),
    TemplateArgs,
    Needs(Vec<String>),
    Attribute,
    Atom,
}

#[derive(Debug, Clone)]
struct Tok {
    range: Range<usize>,
    kind: TokKind,
}

struct Collector<'a> {
    file: &'a SourceFile,
    toks: Vec<Tok>,
    stop: Option<Node<'a>>,
}

impl<'a> Collector<'a> {
    fn text(&self, tok: &Tok) -> &'a str {
        &self.file.text[tok.range.clone()]
    }

    /// Returns false once the function body was reached.
    fn collect(&mut self, node: Node<'a>) -> bool {
        let range = node.byte_range();
        let kind = match node.kind() {
            "comment" => return true,
            "compound_statement" | "try_statement" | "field_initializer_list"
            | "default_method_clause" | "delete_method_clause" | "initializer_list" => {
                self.stop = Some(node);
                return false;
            }
            "fiasco_needs" => TokKind::Needs(needs_list(self.file, node)),
            "template_parameter_list" => {
                TokKind::TemplateParams(template_defaults_textual(self.file, node.byte_range()))
            }
            "template_argument_list" => TokKind::TemplateArgs,
            "parameter_list" => TokKind::Parens(defaults(node)),
            "argument_list" => TokKind::Parens(Vec::new()),
            "attribute_declaration" => TokKind::Attribute,
            "attribute_specifier" | "alignas_qualifier"
            | "ms_declspec_modifier" | "string_literal" | "raw_string_literal"
            | "char_literal" | "concatenated_string" | "requires_clause" => TokKind::Atom,
            "explicit_function_specifier" => TokKind::Word,
            "{" => {
                // A body tree-sitter did not recognize as such.
                self.stop = Some(node);
                return false;
            }
            _ if node.child_count() == 0 => {
                let text = &self.file.text[range.clone()];
                if text.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
                    TokKind::Word
                } else {
                    TokKind::Punct
                }
            }
            _ => {
                let mut cursor = node.walk();
                let children: Vec<_> = node.children(&mut cursor).collect();
                for child in children {
                    if !self.collect(child) {
                        return false;
                    }
                }
                return true;
            }
        };
        if !range.is_empty() {
            self.toks.push(Tok { range, kind });
        }
        true
    }

    /// Group unmatched `(`...`)` token sequences into a single paren token.
    fn group_parens(&mut self) {
        let mut result: Vec<Tok> = Vec::with_capacity(self.toks.len());
        let mut open = Vec::new();
        for tok in std::mem::take(&mut self.toks) {
            let text = &self.file.text[tok.range.clone()];
            if tok.kind == TokKind::Punct && text == "(" {
                open.push(result.len());
                result.push(tok);
            } else if tok.kind == TokKind::Punct && text == ")" && !open.is_empty() {
                let start = open.pop().unwrap();
                let range = result[start].range.start..tok.range.end;
                result.truncate(start);
                result.push(Tok { range, kind: TokKind::Parens(Vec::new()) });
            } else {
                result.push(tok);
            }
        }
        self.toks = result;
    }
}

/// The NEEDS items with normalized whitespace (`Irq::operator new`).
fn needs_list(file: &SourceFile, node: Node<'_>) -> Vec<String> {
    let mut cursor = node.walk();
    let list = node
        .named_children(&mut cursor)
        .filter(|c| c.kind() != "comment")
        .map(|c| file.node_text(c).split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    list
}

/// Default values in `<...>` found on the masked text: from a top-level `=`
/// to the end of the parameter.
fn template_defaults_textual(file: &SourceFile, range: Range<usize>) -> Vec<Range<usize>> {
    let code = file.code.as_bytes();
    let mut result = Vec::new();
    let mut depth = 0i32;
    let mut default_start = None;
    for pos in range.start + 1..range.end.saturating_sub(1) {
        match code[pos] {
            b'<' | b'(' | b'[' | b'{' => depth += 1,
            b'>' | b')' | b']' | b'}' => depth -= 1,
            b'=' if depth == 0
                && default_start.is_none()
                && !matches!(code[pos - 1], b'=' | b'!' | b'<' | b'>')
                && code.get(pos + 1) != Some(&b'=') =>
            {
                default_start = Some(pos)
            }
            b',' if depth == 0 => {
                if let Some(start) = default_start.take() {
                    result.push(start..start + file.text[start..pos].trim_end().len());
                }
            }
            _ => {}
        }
    }
    if let Some(start) = default_start {
        let end = range.end - 1;
        result.push(start..start + file.text[start..end].trim_end().len());
    }
    result
}

/// Ranges of `= default` values in a parameter list (from `=` to the value end).
fn defaults(list: Node<'_>) -> Vec<Range<usize>> {
    let mut result = Vec::new();
    let mut cursor = list.walk();
    for param in list.named_children(&mut cursor) {
        if !param.kind().starts_with("optional_") {
            continue;
        }
        let mut c = param.walk();
        let eq = param.children(&mut c).find(|c| c.kind() == "=");
        if let Some(eq) = eq {
            result.push(eq.start_byte()..param.end_byte());
        }
    }
    result
}

const NOT_A_NAME: &[&str] = &[
    "__attribute__", "__attribute", "alignas", "decltype", "noexcept", "throw", "requires",
    "sizeof", "alignof", "typeof", "__typeof__", "__declspec", "static_assert", "explicit",
    "NEEDS", "operator", "template", "return",
];

fn function(file: &SourceFile, node: Node<'_>, item: &Flat<'_>) -> Option<FunctionHead> {
    let mut collector = Collector { file, toks: Vec::new(), stop: None };
    if let Some(prefix) = item.prefix {
        collector.collect(prefix);
    }
    let reached_body = !collector.collect(node);
    collector.group_parens();
    let toks = std::mem::take(&mut collector.toks);
    let text = |t: &Tok| &file.text[t.range.clone()];

    let mut head = FunctionHead { start: item.range.start, ..Default::default() };
    let mut end = toks.len();
    match collector.stop.map(|n| n.kind()) {
        Some("default_method_clause") => head.assign_spec = Some("default".into()),
        Some("delete_method_clause") => head.assign_spec = Some("delete".into()),
        _ if !reached_body => {
            // A declaration: only `= 0;`, `= default;` and `= delete;` are functions.
            if end > 0 && text(&toks[end - 1]) == ";" {
                end -= 1;
            }
            if end < 2 || text(&toks[end - 2]) != "=" {
                return None;
            }
            let spec = text(&toks[end - 1]);
            if !matches!(spec, "0" | "default" | "delete") {
                return None;
            }
            head.assign_spec = Some(spec.to_owned());
        }
        _ => {}
    }
    head.decl_end = match (&head.assign_spec, collector.stop) {
        (Some(_), Some(stop)) => {
            let clause = file.text[stop.byte_range()].trim_end();
            stop.start_byte() + clause.strip_suffix(';').unwrap_or(clause).trim_end().len()
        }
        (Some(_), None) => toks[end - 1].range.end,
        (None, _) => toks[..end].last()?.range.end,
    };
    let mut head_toks = if head.assign_spec.is_some() && collector.stop.is_none() {
        &toks[..end - 2]
    } else {
        &toks[..end]
    };
    // A ctor initializer tree-sitter did not recognize: `X::X() : a(0)`.
    if let Some(colon) = head_toks.iter().enumerate().position(|(i, t)| {
        text(t) == ":" && head_toks[..i].iter().any(|p| matches!(p.kind, TokKind::Parens(_)))
    }) {
        head_toks = &head_toks[..colon];
        if head.assign_spec.is_none() {
            head.decl_end = head_toks.last()?.range.end;
        }
    }

    // Find the function name and its argument list.
    let (name_start, args) = if let Some(op) = head_toks
        .iter()
        .position(|t| t.kind == TokKind::Word && text(t) == "operator")
    {
        let mut args = head_toks[op + 1..].iter().position(|t| matches!(t.kind, TokKind::Parens(_)))? + op + 1;
        if text(&head_toks[args]).chars().filter(|c| !c.is_whitespace()).eq("()".chars())
            && args == op + 1
            && matches!(head_toks.get(args + 1).map(|t| &t.kind), Some(TokKind::Parens(_)))
        {
            args += 1;
        }
        let name_end = if args > op + 1 && matches!(head_toks[args - 1].kind, TokKind::TemplateArgs) && args - 1 > op + 1 {
            head.func_templateargs = Some(head_toks[args - 1].range.clone());
            args - 1
        } else {
            args
        };
        head.name = head_toks[op..name_end].iter().map(text).collect::<Vec<_>>().join(" ");
        head.name = normalize_operator(&head.name);
        (op, args)
    } else {
        // Like the reference tool, the argument list is the last parenthesized
        // group (macros like `FIASCO_INIT_SFX(x)` may precede the name).
        let mut found = None;
        let mut found_templateargs = None;
        for (index, tok) in head_toks.iter().enumerate() {
            if !matches!(tok.kind, TokKind::Parens(_)) || index == 0 {
                continue;
            }
            let mut name = index - 1;
            let mut templateargs = None;
            if head_toks[name].kind == TokKind::TemplateArgs && name > 0 {
                templateargs = Some(head_toks[name].range.clone());
                name -= 1;
            }
            let candidate = &head_toks[name];
            if candidate.kind != TokKind::Word || NOT_A_NAME.contains(&text(candidate)) {
                continue;
            }
            let mut start = name;
            head.name = text(candidate).to_owned();
            if start > 0 && text(&head_toks[start - 1]) == "~" {
                start -= 1;
                head.name.insert(0, '~');
            }
            found = Some((start, index));
            found_templateargs = templateargs;
        }
        head.func_templateargs = found_templateargs;
        found?
    };
    if let TokKind::Parens(defaults) = &head_toks[args].kind {
        head.default_args = defaults.clone();
    }
    for tok in &head_toks[args + 1..] {
        if tok.kind == TokKind::Word && matches!(text(tok), "override" | "final") {
            head.virt_specs.push(tok.range.clone());
        }
    }

    // Class qualifier: (Name (<args>)? ::)+ directly before the name.
    let mut qualifier_start = name_start;
    let mut components = Vec::new();
    while qualifier_start >= 2 && text(&head_toks[qualifier_start - 1]) == "::" {
        let mut component = qualifier_start - 2;
        let mut args = String::new();
        if head_toks[component].kind == TokKind::TemplateArgs && component > 0 {
            args = compact(text(&head_toks[component]));
            component -= 1;

        }
        if head_toks[component].kind != TokKind::Word {
            break;
        }
        components.push((text(&head_toks[component]).to_owned(), args));
        qualifier_start = component;
    }
    if !components.is_empty() {
        components.reverse();
        let (last, last_args) = components.pop().unwrap();
        let mut class: Vec<String> = components.into_iter().map(|(n, a)| n + &a).collect();
        class.push(last);
        head.class = class.join("::");
        head.class_templateargs = last_args;
        head.qualifier = Some(head_toks[qualifier_start].range.start..head_toks[name_start].range.start);
    }

    let pretext = &head_toks[..qualifier_start];
    if head.class.is_empty() && pretext.iter().all(|t| matches!(t.kind, TokKind::Needs(_))) {
        // No return type: probably a macro invocation.
        return None;
    }
    let has_template = pretext.iter().any(|t| text(t) == "template");
    if !has_template && pretext.iter().any(|t| text(t) == "=") {
        return None;
    }

    head.pretext = pretext.iter().map(|t| t.range.clone()).collect();
    // Like the reference tool, specifiers are only recognized at the start
    // (`void inline X::f()` is not an inline function).
    let mut index = 0;
    while index < pretext.len() {
        let tok = &pretext[index];
        let range = tok.range.clone();
        match (&tok.kind, text(tok)) {
            (TokKind::Needs(list), _) => head.needs.push((range, list.clone())),
            (TokKind::Attribute, _) => head.attributes.push(range),
            (TokKind::Atom, t) if t.starts_with("__attribute") || t.starts_with("alignas") || t.starts_with("requires") => {}
            (TokKind::Word, w) if w.starts_with("__attribute") || w == "attribute" => {
                if matches!(pretext.get(index + 1).map(|t| &t.kind), Some(TokKind::Parens(_))) {
                    index += 1;
                }
            }
            (TokKind::Word, word) => match word {
                "PUBLIC" | "PRIVATE" | "PROTECTED" | "IMPLEMENT" | "IMPLEMENT_DEFAULT"
                | "IMPLEMENT_OVERRIDE" => head.visibility.push((word.to_owned(), range)),
                "inline" | "INLINE" => head.inline.push(range),
                "constexpr" => head.constexpr = true,
                "ALWAYS_INLINE" => head.always_inline.push(range),
                "NOEXPORT" => head.noexport.push(range),
                "static" => head.statics.push(range),
                "virtual" => head.virtuals.push(range),
                w if w.starts_with("explicit") => head.explicits.push(range),
                "template" => {
                    if let Some(Tok { kind: TokKind::TemplateParams(defaults), range: params }) =
                        pretext.get(index + 1)
                    {
                        let inner = &file.text[params.start + 1..params.end - 1];
                        head.templates.push(TemplateHeader {
                            range: range.start..params.end,
                            empty: inner.trim().is_empty(),
                            defaults: defaults.clone(),
                        });
                        index += 1;
                    }
                }
                _ => break,
            },
            _ => break,
        }
        index += 1;
    }
    Some(head)
}

fn normalize_operator(name: &str) -> String {
    // `operator ==` -> `operator==`, `operator new []` -> `operator new[]`.
    let mut result = String::new();
    let mut last_word = false;
    for part in name.split_whitespace() {
        let word = part.starts_with(|c: char| c.is_alphanumeric() || c == '_');
        if word && last_word {
            result.push(' ');
        }
        result.push_str(part);
        last_word = part.ends_with(|c: char| c.is_alphanumeric() || c == '_');
    }
    result
}

/// Calls of `TAG_ENABLED(...)`: the whole call and the expression inside.
pub fn tag_enabled_calls(file: &SourceFile) -> Vec<(Range<usize>, Range<usize>)> {
    let code = file.code.as_bytes();
    let mut result = Vec::new();
    for (start, _) in file.code.match_indices("TAG_ENABLED") {
        let before = start.checked_sub(1).map(|i| code[i]);
        if before.is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_') {
            continue;
        }
        let mut pos = start + "TAG_ENABLED".len();
        while pos < code.len() && code[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if code.get(pos) != Some(&b'(') {
            continue;
        }
        let open = pos;
        let mut depth = 0;
        let close = code[open..].iter().position(|&b| {
            depth += match b {
                b'(' => 1,
                b')' => -1,
                _ => 0,
            };
            depth == 0
        });
        if let Some(close) = close {
            let close = open + close;
            result.push((start..close + 1, open + 1..close));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heads(text: &str) -> Vec<FunctionHead> {
        let file = SourceFile::new("a.cpp", text).unwrap();
        items(&file)
            .into_iter()
            .filter_map(|i| match i.kind {
                ItemKind::Function(f) => Some(*f),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn classifies_items_and_owns_lines() {
        let text = "INTERFACE:\n#include \"foo.h\"\nclass Foo;\n// doc\nclass Bar : public Baz\n{\n  int x;\n}; // trailing\ntypedef int Myint;\nIMPLEMENTATION [arm]:\nEXTENSION class Bar : Other { int ext; };\nint x;\n";
        let file = SourceFile::new("a.cpp", text).unwrap();
        let items = items(&file);
        let kinds: Vec<_> = items
            .iter()
            .map(|i| match &i.kind {
                ItemKind::Section { kind, condition } => format!("S{kind:?}{condition:?}"),
                ItemKind::Include(n) => format!("I{n}"),
                ItemKind::Code | ItemKind::SectionComment => "C".into(),
                ItemKind::Class(c) => format!("K{}{:?}{}", c.name, c.syntax, c.has_bases),
                ItemKind::Extension(e) => format!("E{}{}", e.name, e.colon.is_some()),
                ItemKind::Function(f) => format!("F{}", f.full_name()),
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "SInterfaceNone",
                "I\"foo.h\"",
                "KFooForwardDeclfalse",
                "KBarClasstrue",
                "KMyintTypedeffalse",
                "SImplementationSome(\"arm\")",
                "EBartrue",
                "C"
            ]
        );
        assert_eq!(&text[items[3].range.clone()], "// doc\nclass Bar : public Baz\n{\n  int x;\n}; // trailing\n");
        // Ranges are contiguous.
        for pair in items.windows(2) {
            assert_eq!(pair[0].range.end, pair[1].range.start);
        }
    }

    #[test]
    fn analyzes_function_heads() {
        let text = "IMPLEMENTATION:\n\
            PUBLIC inline NEEDS[\"foo.h\", Bar::x] int Bar::get(int a = 3) const override\n{ return x; }\n\
            PUBLIC static FIASCO_INIT\nvoid\nBar::init()\n{}\n\
            PUBLIC explicit Bar::Bar(int a) : x(a) {}\n\
            PUBLIC template<typename T> inline T Tpl<T>::get() { return t; }\n\
            PUBLIC virtual void Bar::pure() = 0;\n\
            PUBLIC Bar::Bar() = default;\n\
            PUBLIC bool Bar::operator == (Bar const &o) const { return true; }\n\
            PUBLIC Bar::operator bool() const { return true; }\n\
            PUBLIC Bar::~Bar() {}\n\
            static void helper(int a) {}\n\
            IMPLEMENT template<typename T> template<typename R>\nR *Outer<T>::Inner::get() const\n{ return nullptr; }\n\
            FOO(bar) { }\n";
        let heads = heads(text);
        let names: Vec<_> = heads.iter().map(|h| h.full_name()).collect();
        assert_eq!(
            names,
            [
                "Bar::get", "Bar::init", "Bar::Bar", "Tpl::get", "Bar::pure", "Bar::Bar",
                "Bar::operator==", "Bar::operator bool", "Bar::~Bar", "helper",
                "Outer<T>::Inner::get"
            ]
        );
        let get = &heads[0];
        assert_eq!(get.visibility[0].0, "PUBLIC");
        assert_eq!(get.inline.len(), 1);
        assert_eq!(get.needs[0].1, ["\"foo.h\"", "Bar::x"]);
        assert_eq!(&text[get.default_args[0].clone()], "= 3");
        assert_eq!(&text[get.virt_specs[0].clone()], "override");
        assert_eq!(&text[get.qualifier.clone().unwrap()], "Bar::");
        assert!(text[..get.decl_end].ends_with("const override"));
        assert_eq!(heads[1].statics.len(), 1);
        assert_eq!(heads[2].explicits.len(), 1);
        assert!(text[..heads[2].decl_end].ends_with("Bar(int a)"));
        assert_eq!(heads[3].class_templateargs, "<T>");
        assert_eq!(heads[3].templates.len(), 1);
        assert_eq!(heads[4].assign_spec.as_deref(), Some("0"));
        assert!(text[..heads[4].decl_end].ends_with("= 0"));
        assert_eq!(heads[5].assign_spec.as_deref(), Some("default"));
        assert!(text[..heads[5].decl_end].ends_with("= default"));
        assert_eq!(heads[10].templates.len(), 2);
    }

    #[test]
    fn finds_tag_enabled_calls() {
        let file = SourceFile::new("a.cpp", "x = TAG_ENABLED(a && (b)); // TAG_ENABLED(c)\nMY_TAG_ENABLED(d);\n").unwrap();
        let calls = tag_enabled_calls(&file);
        assert_eq!(calls.len(), 1);
        assert_eq!(&file.text[calls[0].1.clone()], "a && (b)");
    }
}
