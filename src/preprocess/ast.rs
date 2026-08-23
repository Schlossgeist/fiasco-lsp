use std::fmt;
use std::fmt::{Display, Formatter};
use pest::iterators::Pair;
use crate::preprocess::parser::Rule;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceSpan {
    pub start: usize,
    pub end: usize,
}

impl SourceSpan {
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    pub const fn len(self) -> usize {
        self.end - self.start
    }

    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    pub fn get(self, source: &str) -> &str {
        &source[self.start..self.end]
    }
}

pub trait Spanned {
    fn span(&self) -> SourceSpan;
}

pub trait PestSpanExt {
    fn source_span(&self) -> SourceSpan;
}

impl<'i> PestSpanExt for Pair<'i, Rule> {
    fn source_span(&self) -> SourceSpan {
        let span = self.as_span();

        SourceSpan {
            start: span.start(),
            end: span.end(),
        }
    }
}

pub struct FppSectionData {
    pub guard: Option<String>,
    pub declarations: Vec<Declaration>,
}

pub enum FppSection {
    Interface(FppSectionData),
    Implementation(FppSectionData),
}

pub struct TranslationUnit {
    pub declarations: Vec<Declaration>,
    pub sections: Vec<FppSection>,
}

pub enum Declaration {
    PreprocessLine(SourceSpan),
    ClassOrStruct(ClassOrStruct),
    Function(Function),
    Method(Method),
    ExternC(ExternC),
    Verbatim(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassOrStructKind {
    Class,
    Struct,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identifier(pub String);

impl Display for Identifier {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)?;

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassOrStruct {
    pub span: SourceSpan,
    
    pub kind: ClassOrStructKind,
    pub extension: bool,
    pub name: Identifier,
    pub inheritance: Option<String>,
    pub body: String,
    pub additional_methods: Vec<Method>,
}

pub struct ExternC {
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
    pub expression: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeModifier {
    Pointer,
    Reference,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentifierTemplate {
    pub name: Identifier,
    pub template_arguments: Option<String>,
}

impl Display for IdentifierTemplate {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)?;

        if let Some(arguments) = &self.template_arguments {
            write!(f, "{}", arguments)?;
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualifiedIdentifier(pub Vec<IdentifierTemplate>);

impl Display for QualifiedIdentifier {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        for (i, component) in self.0.iter().enumerate() {
            if i != 0 {
                write!(f, "::")?;
            }

            write!(f, "{}", component)?;
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReturnType {
    pub name: QualifiedIdentifier,
    pub modifier: Option<TypeModifier>,
}

impl Display for ReturnType {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)?;

        match self.modifier {
            Some(TypeModifier::Pointer) => write!(f, "*"),
            Some(TypeModifier::Reference) => write!(f, "&"),
            None => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribute {
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    pub span: SourceSpan,
    
    pub extern_c: bool,
    pub templates: Vec<Template>,
    pub attributes: Vec<Attribute>,
    pub is_static: bool,
    pub is_inline: bool,
    pub noexport: bool,
    pub is_constexpr: bool,
    pub dependency: Option<Dependency>,
    pub return_type: ReturnType,
    pub name: SourceSpan,
    pub parameters: Option<String>,
    pub suffix: String,
    pub body: String,
}

impl Function {
    pub fn get_prefix_span(&self) -> SourceSpan {
        SourceSpan{
            start: self.span.start,
            end: self.name.start - 1,
        }
    } 
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessSpecifier {
    Private,
    Protected,
    Public,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImplementSpecifier {
    Implement,
    ImplementDefault,
    ImplementOverride,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MethodBody {
    Definition(String),
    Default,
    Delete,
}

impl Display for MethodBody {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self { 
            MethodBody::Definition(definition) => write!(f, "{}", definition),
            MethodBody::Default => write!(f, " = default;"),
            MethodBody::Delete => write!(f, " = delete;"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Method {
    pub span: SourceSpan,
    
    pub access: Option<AccessSpecifier>,
    pub implementation: Option<ImplementSpecifier>,
    pub inline: bool,
    pub templates: Vec<Template>,
    pub attributes: Vec<Attribute>,
    pub is_static: bool,
    pub noexport: bool,
    pub is_constexpr: bool,
    pub dependency: Option<Dependency>,
    pub return_type: Option<ReturnType>,
    pub parent: SourceSpan,
    pub name: SourceSpan,
    pub parameters: Option<String>,
    pub suffix: String,
    pub body: MethodBody,
}

impl Method {
    pub fn get_prefix_span(&self) -> SourceSpan {
        SourceSpan{
            start: self.span.start,
            end: self.parent.start - 1,
        }
    }
    
    pub fn get_sanitized_prefix(&self, source: &str) -> String {
        self.get_prefix_span().get(source).split_whitespace()
            .filter(|word| !matches!(*word, "PUBLIC" | "PROTECTED" | "PRIVATE"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}
