use std::fmt;
use pest::iterators::{Pair, Pairs};
use pest::Parser;

use super::ast::*;

#[derive(pest_derive::Parser)]
#[grammar = "preprocess/fpp_LL.pest"]
pub struct FPPParser;

pub fn parse_translation_unit(source: &str) -> Result<TranslationUnit, ParseError> {
    let mut pairs = FPPParser::parse(Rule::translation_unit, source)
        .map_err(ParseError::Pest)?;

    let root = pairs
        .next()
        .ok_or(ParseError::Missing("translation_unit"))?;

    parse_translation_unit_pair(root)
}

#[derive(Debug)]
pub enum ParseError {
    Pest(pest::error::Error<Rule>),
    Missing(&'static str),
    Unexpected {
        expected: &'static str,
        found: Rule,
        source: String,
    },
    InvalidOperator(String),
    InvalidIdentifier(String),
    InvalidStructure(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pest(error) => write!(f, "{error}"),

            Self::Missing(name) => {
                write!(f, "missing required grammar rule `{name}`")
            }

            Self::Unexpected {
                expected,
                found,
                source,
            } => {
                write!(
                    f,
                    "expected {expected}, found {found:?}: {source:?}"
                )
            }

            Self::InvalidOperator(op) => {
                write!(f, "unknown overloadable operator: {op:?}")
            }

            Self::InvalidIdentifier(identifier) => {
                write!(f, "invalid identifier: {identifier:?}")
            }

            Self::InvalidStructure(message) => {
                write!(f, "invalid AST structure: {message}")
            }
        }
    }
}

impl std::error::Error for ParseError {}


// Translation unit
fn parse_translation_unit_pair(
    pair: Pair<'_, Rule>,
) -> Result<TranslationUnit, ParseError> {
    debug_assert_eq!(pair.as_rule(), Rule::translation_unit);

    let mut declarations = Vec::new();
    let mut sections = Vec::new();

    for child in pair.into_inner() {
        match child.as_rule() {
            Rule::declaration => {
                declarations.push(parse_declaration_pair(child)?);
            }

            Rule::fpp_interface_section => {
                sections.push(FppSection::Interface(
                    parse_section_pair(child)?,
                ));
            }

            Rule::fpp_implementation_section => {
                sections.push(FppSection::Implementation(
                    parse_section_pair(child)?,
                ));
            }
            
            Rule::EOI => {
                break;
            }
            
            rule => {
                return Err(ParseError::Unexpected {
                    expected: "declaration or FPP section",
                    found: rule,
                    source: child.as_str().to_owned(),
                });
            }
        }
    }

    Ok(TranslationUnit {
        declarations,
        sections,
    })
}


// Declarations
fn parse_declaration_pair(
    pair: Pair<'_, Rule>,
) -> Result<Declaration, ParseError> {
    debug_assert_eq!(pair.as_rule(), Rule::declaration);

    let child = pair
        .into_inner()
        .next()
        .ok_or(ParseError::Missing("declaration child"))?;

    match child.as_rule() {
        Rule::preprocess_line => {
            Ok(Declaration::PreprocessLine(
                child.source_span(),
            ))
        }

        Rule::fpp_class_or_struct => {
            Ok(Declaration::ClassOrStruct(
                parse_class_or_struct(child)?,
            ))
        }

        Rule::fpp_function => {
            Ok(Declaration::Function(parse_function(child)?))
        }

        Rule::fpp_method => {
            Ok(Declaration::Method(parse_method(child)?))
        }

        Rule::extern_c => {
            Ok(Declaration::ExternC(ExternC {
                body: parse_extern_c(child)?,
            }))
        }

        Rule::verbatim_copy => {
            Ok(Declaration::Verbatim(
                child.as_str().to_owned(),
            ))
        }

        rule => Err(ParseError::Unexpected {
            expected: "declaration",
            found: rule,
            source: child.as_str().to_owned(),
        }),
    }
}


// extern "C"
fn parse_extern_c(pair: Pair<'_, Rule>) -> Result<String, ParseError> {
    debug_assert_eq!(pair.as_rule(), Rule::extern_c);

    // `balanced_block` is silent, so the enclosing span is the useful
    // representation. Strip the `extern "C"` prefix and retain the block.
    let source = pair.as_str();

    let block_start = source
        .find('{')
        .ok_or_else(|| {
            ParseError::InvalidStructure(
                "extern_c has no opening `{`".into(),
            )
        })?;

    Ok(source[block_start..].to_owned())
}


// class / struct
fn parse_class_or_struct(
    pair: Pair<'_, Rule>,
) -> Result<ClassOrStruct, ParseError> {
    debug_assert_eq!(pair.as_rule(), Rule::fpp_class_or_struct);

    let span = pair.source_span();
    let source = pair.as_str();

    let mut extension = false;
    let mut kind = None;
    let mut name = None;
    let mut inheritance = None;
    let mut body = String::new();
    let mut additional_methods = Vec::new();

    if source.contains("class") {
        kind = Some(ClassOrStructKind::Class);
    } else if source.contains("struct") {
        kind = Some(ClassOrStructKind::Struct);
    }
    
    for child in pair.into_inner() {
        match child.as_rule() {
            Rule::fpp_extension_specifier => {
                extension = true;
            }

            Rule::valid_class_or_struct_prefix_char_seq => {
                // This is syntactic C++ that FPP does not model.
                // The AST currently does not have a field for it.
            }

            Rule::identifier => {
                name = Some(Identifier(child.as_str().to_owned()));
            }

            Rule::valid_class_or_struct_suffix_char_seq => {
                inheritance = Some(child.as_str().trim().to_owned());
            }

            Rule::balanced_block => {
                body = child.as_str().to_owned();
            }

            rule => {
                return Err(ParseError::Unexpected {
                    expected: "class/struct component",
                    found: rule,
                    source: child.as_str().to_owned(),
                });
            }
        }
    }

    let kind = kind.ok_or(ParseError::InvalidStructure(
        "class/struct declaration has no class or struct keyword".into(),
    ))?;

    let name = name.ok_or(ParseError::Missing("class/struct identifier"))?;

    Ok(ClassOrStruct {
        span,
        extension,
        kind,
        name,
        inheritance,
        body,
        additional_methods,
    })
}


// Functions
fn parse_function(
    pair: Pair<'_, Rule>,
) -> Result<Function, ParseError> {
    debug_assert_eq!(pair.as_rule(), Rule::fpp_function);

    let span = pair.source_span();
    let source = pair.as_str();

    let mut extern_c = false;
    let mut templates = Vec::new();
    let mut attributes = Vec::new();
    let mut is_static = false;
    let mut is_inline = false;
    let mut noexport = false;
    let mut is_constexpr = false;
    let mut dependency = None;
    let mut return_type = None;
    let mut name = None;
    let mut parameters = None;
    let mut suffix = String::new();
    let mut body = None;

    let mut children = pair.into_inner().peekable();

    while let Some(child) = children.next() {
        match child.as_rule() {
            Rule::template => {
                templates.push(parse_template(child)?);
            }

            Rule::fpp_noexport => {
                noexport = true;
            }

            Rule::fpp_dependency => {
                dependency = Some(parse_dependency(child)?);
            }

            Rule::return_type => {
                return_type = Some(parse_return_type(child)?);
            }

            Rule::identifier => {
                name = Some(child.source_span().to_owned());
            }

            Rule::operator => {
                name = Some(child.source_span().to_owned());
            }

            Rule::balanced_parentheses => {
                parameters = Some(child.as_str().to_owned());
            }

            Rule::balanced_block => {
                body = Some(child.as_str().to_owned());
            }

            rule => {
                return Err(ParseError::Unexpected {
                    expected: "function component",
                    found: rule,
                    source: child.as_str().to_owned(),
                });
            }
        }
    }

    let return_type = return_type.ok_or(
        ParseError::Missing("function return_type"),
    )?;

    let name = name.ok_or(
        ParseError::Missing("function name"),
    )?;
    
    // Attributes are silent in the supplied grammar. Recover them from
    // the source instead of pretending they are Pair children.
    attributes = extract_attributes(source);

    // The grammar allows the initial `extern "C"` and the static/inline/
    // constexpr keywords as literals, so those too must be recovered from
    // the source.
    extern_c = contains_token(source, "extern \"C\"");
    is_static = contains_token(source, "static");
    is_inline = contains_token(source, "inline");
    is_constexpr = contains_token(source, "constexpr");

    if body.is_none() {
        body = Some(String::new());
    }

    suffix = extract_function_suffix(source);

    Ok(Function {
        span,
        extern_c,
        templates,
        attributes,
        is_static,
        is_inline,
        noexport,
        is_constexpr,
        dependency,
        return_type,
        name,
        parameters,
        suffix,
        body: body.unwrap(),
    })
}


// Method
fn parse_method(
    pair: Pair<'_, Rule>,
) -> Result<Method, ParseError> {
    debug_assert_eq!(pair.as_rule(), Rule::fpp_method);

    let span = pair.source_span();
    let source = pair.as_str();

    let mut access = None;
    let mut implementation = None;
    let mut templates = Vec::new();
    let mut attributes = Vec::new();
    let mut is_static = false;
    let mut is_inline = false;
    let mut noexport = false;
    let mut is_constexpr = false;
    let mut dependency = None;
    let mut return_type = None;
    let mut parent = None;
    let mut name = None;
    let mut parameters = None;
    let mut body = None;

    for child in pair.into_inner() {
        match child.as_rule() {
            Rule::fpp_access_specifier => {
                access = Some(parse_access_specifier(child)?);
            }

            Rule::fpp_implement_specifier => {
                implementation = Some(parse_implement_specifier(child)?);
            }

            Rule::template => {
                templates.push(parse_template(child)?);
            }

            Rule::fpp_noexport => {
                noexport = true;
            }

            Rule::fpp_dependency => {
                dependency = Some(parse_dependency(child)?);
            }

            Rule::return_type => {
                return_type = Some(parse_return_type(child)?);
            }

            Rule::fpp_method_core => {
                let (parsed_parent, parsed_name) =
                    parse_method_core(child)?;

                parent = Some(parsed_parent);
                name = Some(parsed_name);
            }

            Rule::balanced_parentheses => {
                parameters = Some(child.as_str().to_owned());
            }

            Rule::balanced_block => {
                body = Some(MethodBody::Definition(
                    child.as_str().to_owned(),
                ));
            }

            rule => {
                return Err(ParseError::Unexpected {
                    expected: "method component",
                    found: rule,
                    source: child.as_str().to_owned(),
                });
            }
        }
    }

    let parent = parent.ok_or(
        ParseError::Missing("method parent"),
    )?;

    let name = name.ok_or(
        ParseError::Missing("method name"),
    )?;

    attributes = extract_attributes(source);

    is_static = contains_token(source, "static");
    is_inline = contains_token(source, "inline");
    is_constexpr = contains_token(source, "constexpr");

    if body.is_none() {
        let tail = source.trim_end();

        if tail.ends_with("default;") {
            body = Some(MethodBody::Default);
        } else if tail.ends_with("delete;") {
            body = Some(MethodBody::Delete);
        } else {
            return Err(ParseError::InvalidStructure(
                "method has neither a body nor default/delete".into(),
            ));
        }
    }

    let suffix = extract_method_suffix(source);

    Ok(Method {
        span,
        access,
        implementation,
        inline: is_inline,
        templates,
        attributes,
        is_static,
        noexport,
        is_constexpr,
        dependency,
        return_type,
        parent,
        name,
        parameters,
        suffix,
        body: body.unwrap(),
    })
}


// Method core
fn parse_method_core(
    pair: Pair<'_, Rule>,
) -> Result<(SourceSpan, SourceSpan), ParseError> {
    debug_assert_eq!(pair.as_rule(), Rule::fpp_method_core);

    let mut parent = None;
    let mut name = None;

    for child in pair.into_inner() {
        match child.as_rule() {
            Rule::fpp_parent_name => {
                parent = Some(parse_parent_name(child)?);
            }

            Rule::fpp_method_name => {
                name = Some(parse_method_name(child)?);
            }

            rule => {
                return Err(ParseError::Unexpected {
                    expected: "method core component",
                    found: rule,
                    source: child.as_str().to_owned(),
                });
            }
        }
    }

    let parent = parent.ok_or(
        ParseError::Missing("method parent name"),
    )?;

    let name = name.ok_or(
        ParseError::Missing("method name"),
    )?;

    Ok((parent, name))
}


// Names
fn parse_parent_name(
    pair: Pair<'_, Rule>,
) -> Result<SourceSpan, ParseError> {
    debug_assert_eq!(pair.as_rule(), Rule::fpp_parent_name);

    let child = pair
        .into_inner()
        .next()
        .ok_or(ParseError::Missing("method parent child"))?;

    match child.as_rule() {
        Rule::identifier_tmplt => {
            Ok(child.source_span().to_owned())
        }

        rule => Err(ParseError::Unexpected {
            expected: "identifier template",
            found: rule,
            source: child.as_str().to_owned(),
        }),
    }
}

fn parse_method_name(
    pair: Pair<'_, Rule>,
) -> Result<SourceSpan, ParseError> {
    debug_assert_eq!(pair.as_rule(), Rule::fpp_method_name);

    let child = pair
        .into_inner()
        .next()
        .ok_or(ParseError::Missing("method name child"))?;

    match child.as_rule() {
        Rule::operator => {
            Ok(child.source_span().to_owned())
        }

        Rule::identifier_tmplt => {
            Ok(child.source_span().to_owned())
        }

        rule => Err(ParseError::Unexpected {
            expected: "operator or identifier template",
            found: rule,
            source: child.as_str().to_owned(),
        }),
    }
}

fn parse_identifier_template(
    pair: Pair<'_, Rule>,
) -> Result<IdentifierTemplate, ParseError> {
    debug_assert_eq!(pair.as_rule(), Rule::identifier_tmplt);

    let mut inner = pair.into_inner();

    let identifier = inner
        .next()
        .ok_or(ParseError::Missing("identifier"))?;

    let template_arguments = inner
        .next()
        .map(|p| p.as_str().to_owned());

    Ok(IdentifierTemplate {
        name: Identifier(identifier.as_str().to_owned()),
        template_arguments,
    })
}

fn parse_qualified_identifier(
    pair: Pair<'_, Rule>,
) -> Result<QualifiedIdentifier, ParseError> {
    debug_assert_eq!(
        pair.as_rule(),
        Rule::quali_identifier_tmplt
    );

    let components = pair
        .into_inner()
        .filter(|p| p.as_rule() == Rule::identifier_tmplt)
        .map(parse_identifier_template)
        .collect::<Result<Vec<_>, _>>()?;

    Ok(QualifiedIdentifier(components))
}


// Return type
fn parse_return_type(
    pair: Pair<'_, Rule>,
) -> Result<ReturnType, ParseError> {
    debug_assert_eq!(pair.as_rule(), Rule::return_type);

    let source = pair.as_str();

    let qualified = pair
        .clone()
        .into_inner()
        .find(|p| p.as_rule() == Rule::quali_identifier_tmplt)
        .ok_or(ParseError::Missing(
            "qualified return type",
        ))?;

    let name = parse_qualified_identifier(qualified)?;

    let trimmed = source.trim_end();

    let modifier = if trimmed.ends_with('*') {
        Some(TypeModifier::Pointer)
    } else if trimmed.ends_with('&') {
        Some(TypeModifier::Reference)
    } else {
        None
    };

    Ok(ReturnType { name, modifier })
}


// FPP metadata
fn parse_access_specifier(
    pair: Pair<'_, Rule>,
) -> Result<AccessSpecifier, ParseError> {
    match pair.as_str() {
        "PRIVATE" => Ok(AccessSpecifier::Private),
        "PROTECTED" => Ok(AccessSpecifier::Protected),
        "PUBLIC" => Ok(AccessSpecifier::Public),

        _ => Err(ParseError::InvalidStructure(format!(
            "unknown access specifier {:?}",
            pair.as_str()
        ))),
    }
}

fn parse_implement_specifier(
    pair: Pair<'_, Rule>,
) -> Result<ImplementSpecifier, ParseError> {
    match pair.as_str() {
        "IMPLEMENT" => Ok(ImplementSpecifier::Implement),
        "IMPLEMENT_DEFAULT" => {
            Ok(ImplementSpecifier::ImplementDefault)
        }
        "IMPLEMENT_OVERRIDE" => {
            Ok(ImplementSpecifier::ImplementOverride)
        }

        _ => Err(ParseError::InvalidStructure(format!(
            "unknown implementation specifier {:?}",
            pair.as_str()
        ))),
    }
}

fn parse_dependency(
    pair: Pair<'_, Rule>,
) -> Result<Dependency, ParseError> {
    debug_assert_eq!(pair.as_rule(), Rule::fpp_dependency);

    let source = pair.as_str();

    let open = source.find('[').ok_or(
        ParseError::InvalidStructure(
            "NEEDS has no opening bracket".into(),
        ),
    )?;

    let close = source.rfind(']').ok_or(
        ParseError::InvalidStructure(
            "NEEDS has no closing bracket".into(),
        ),
    )?;

    if close <= open {
        return Err(ParseError::InvalidStructure(
            "invalid NEEDS bracket range".into(),
        ));
    }

    Ok(Dependency {
        expression: source[open + 1..close].trim().to_owned(),
    })
}

fn parse_template(
    pair: Pair<'_, Rule>,
) -> Result<Template, ParseError> {
    debug_assert_eq!(pair.as_rule(), Rule::template);

    let arguments = pair
        .into_inner()
        .find(|p| p.as_rule() == Rule::balanced_angles)
        .map(|p| p.as_str().to_owned())
        .ok_or(ParseError::Missing(
            "template balanced_angles",
        ))?;

    Ok(Template { arguments })
}


// FPP sections
fn parse_section_pair(
    pair: Pair<'_, Rule>,
) -> Result<FppSectionData, ParseError> {
    let inner = pair.into_inner();

    let guard = None;
    
    let mut declarations = Vec::new();

    for child in inner {
        let rule = child.as_rule();
        match rule {
            Rule::declaration => {
                declarations.push(parse_declaration_pair(child)?);
            }

            rule => {
                return Err(ParseError::Unexpected {
                    expected: "declaration",
                    found: rule,
                    source: child.as_str().to_owned(),
                });
            }
        }
    }

    Ok(FppSectionData {
        guard,
        declarations,
    })
}

// Source-based recovery
fn extract_attributes(source: &str) -> Vec<Attribute> {
    let mut result = Vec::new();
    let mut offset = 0;

    while offset < source.len() {
        let rest = &source[offset..];

        if rest.starts_with("[[") {
            if let Some(end) = find_double_close(rest, 2) {
                result.push(Attribute {
                    source: rest[..end].to_owned(),
                });
                offset += end;
                continue;
            }
        }

        if let Some(end) =
            find_attribute_macro(rest, "attribute")
        {
            result.push(Attribute {
                source: rest[..end].to_owned(),
            });
            offset += end;
            continue;
        }

        if let Some(end) =
            find_attribute_macro(rest, "__attribute__")
        {
            result.push(Attribute {
                source: rest[..end].to_owned(),
            });
            offset += end;
            continue;
        }

        let ch_len = rest
            .chars()
            .next()
            .map(char::len_utf8)
            .unwrap_or(1);

        offset += ch_len;
    }

    result
}

fn find_double_close(source: &str, start: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut i = start;

    while i + 1 < bytes.len() {
        if bytes[i] == b']' && bytes[i + 1] == b']' {
            return Some(i + 2);
        }

        i += 1;
    }

    None
}

fn find_attribute_macro(
    source: &str,
    keyword: &str,
) -> Option<usize> {
    if !source.starts_with(keyword) {
        return None;
    }

    let after_keyword = source[keyword.len()..].trim_start();

    if !after_keyword.starts_with('(') {
        return None;
    }

    let whitespace_len =
        source[keyword.len()..].len() - after_keyword.len();

    let open =
        keyword.len() + whitespace_len;

    let close = find_matching_paren(source, open)?;

    Some(close + 1)
}

fn find_matching_paren(
    source: &str,
    open: usize,
) -> Option<usize> {
    let bytes = source.as_bytes();

    if bytes.get(open) != Some(&b'(') {
        return None;
    }

    let mut depth = 0usize;
    let mut i = open;

    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                i = skip_quoted(source, i, b'"')?;
                continue;
            }

            b'\'' => {
                i = skip_quoted(source, i, b'\'')?;
                continue;
            }

            b'(' => {
                depth += 1;
            }

            b')' => {
                depth = depth.checked_sub(1)?;

                if depth == 0 {
                    return Some(i);
                }
            }

            _ => {}
        }

        i += 1;
    }

    None
}

fn skip_quoted(
    source: &str,
    start: usize,
    quote: u8,
) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut i = start + 1;

    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }

        if bytes[i] == quote {
            return Some(i + 1);
        }

        i += 1;
    }

    None
}


// Function/method suffixes
fn extract_function_suffix(source: &str) -> String {
    let Some(close) = find_last_parameter_close(source) else {
        return String::new();
    };

    let after = &source[close + 1..];

    let body_start = find_top_level_char(after, '{');
    let declaration_end = after.find(';');

    let end = match (body_start, declaration_end) {
        (Some(body), Some(semi)) => body.min(semi),
        (Some(body), None) => body,
        (None, Some(semi)) => semi,
        (None, None) => after.len(),
    };

    after[..end].trim().to_owned()
}

fn extract_method_suffix(source: &str) -> String {
    let Some(close) = find_last_parameter_close(source) else {
        return String::new();
    };

    let after = &source[close + 1..];

    let body_start = find_top_level_char(after, '{');

    let default_pos = find_keyword(after, "default");
    let delete_pos = find_keyword(after, "delete");

    let end = body_start
        .into_iter()
        .chain(default_pos)
        .chain(delete_pos)
        .min()
        .unwrap_or(after.len());

    after[..end].trim().to_owned()
}

fn find_last_parameter_close(source: &str) -> Option<usize> {
    let bytes = source.as_bytes();

    let mut depth = 0usize;
    let mut last_close = None;
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                i = skip_quoted(source, i, b'"')?;
                continue;
            }

            b'\'' => {
                i = skip_quoted(source, i, b'\'')?;
                continue;
            }

            b'(' => {
                depth += 1;
            }

            b')' => {
                if depth == 0 {
                    return None;
                }

                depth -= 1;

                if depth == 0 {
                    last_close = Some(i);
                }
            }

            _ => {}
        }

        i += 1;
    }

    last_close
}

fn find_top_level_char(
    source: &str,
    wanted: char,
) -> Option<usize> {
    let bytes = source.as_bytes();

    let mut parens = 0usize;
    let mut brackets = 0usize;
    let mut angles = 0usize;

    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                i = skip_quoted(source, i, b'"')?;
                continue;
            }

            b'\'' => {
                i = skip_quoted(source, i, b'\'')?;
                continue;
            }

            b'(' => parens += 1,
            b')' => parens = parens.saturating_sub(1),

            b'[' => brackets += 1,
            b']' => brackets = brackets.saturating_sub(1),

            b'<' => angles += 1,
            b'>' => angles = angles.saturating_sub(1),

            byte if byte as char == wanted
                && parens == 0
                && brackets == 0
                && angles == 0 =>
                {
                    return Some(i);
                }

            _ => {}
        }

        i += 1;
    }

    None
}

fn find_keyword(
    source: &str,
    keyword: &str,
) -> Option<usize> {
    let mut offset = 0;

    while offset < source.len() {
        let rest = &source[offset..];

        if rest.starts_with(keyword) {
            let before_ok = offset == 0
                || !is_identifier_char(
                source.as_bytes()[offset - 1],
            );

            let end = offset + keyword.len();

            let after_ok = end == source.len()
                || !is_identifier_char(source.as_bytes()[end]);

            if before_ok && after_ok {
                return Some(offset);
            }
        }

        let len = rest
            .chars()
            .next()
            .map(char::len_utf8)
            .unwrap_or(1);

        offset += len;
    }

    None
}

fn is_identifier_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn contains_token(source: &str, token: &str) -> bool {
    find_keyword(source, token).is_some()
}

