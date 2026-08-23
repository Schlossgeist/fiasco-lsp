use crate::preprocess::ast::{AccessSpecifier, ClassOrStruct, Declaration, FppSection, MethodBody, TranslationUnit};

pub struct CPPTriple {
    pub filename:       String,
    pub public_header:  String,
    pub private_header: String,
    pub implementation: String,
}

fn guard_matches(
    guard: &str,
    active_tags: &Vec<String>,
) -> bool {
    true
}

fn source_line(
    source: &Source,
    offset: usize,
    filename: &str,
) -> String {
    format!(
        "\n#line {} \"{}\"\n",
        source.line_at(offset),
        filename
    )
}

#[derive(Debug)]
pub struct Source {
    pub text: String,
    pub line_beginnings: Vec<usize>,
}

impl Source {
    pub fn new(text: String) -> Self {
        let mut line_beginnings = vec![0];

        for (offset, char) in text.chars().enumerate() {
            if char == '\n' {
                line_beginnings.push(offset + 1);
            }
        }

        Self {
            text,
            line_beginnings,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }
    
    pub fn line_at(&self, offset: usize) -> usize {
        self.line_beginnings
            .binary_search(&offset)
            .unwrap_or_else(|next_line| {
                next_line - 1
            })
    }
}

pub fn generate_cpp_triple(
    filename: &str,
    original_source: Source,
    active_tags: Vec<String>,
    translation_unit: TranslationUnit,
) -> CPPTriple {
    // first pass (collection)
    let mut implementation_includes: Vec<Declaration> = Vec::new();
    let mut classes: Vec<ClassOrStruct> = Vec::new();
    let mut structs: Vec<ClassOrStruct> = Vec::new();

    for declaration in translation_unit.declarations {
        match declaration {
            Declaration::PreprocessLine(span) => {
                let text = span.get(original_source.text());

                if text.starts_with("#include") {
                    implementation_includes.push(declaration);
                }
            }
            _ => {}
        }
    }

    for section in &translation_unit.sections {
        if let Some(guard) = match &section {
            FppSection::Interface(data) | FppSection::Implementation(data) => &data.guard,
        } {
            if !guard_matches(guard.as_str(), &active_tags) {
                continue;
            }
        }

        match section {
            FppSection::Interface(data) => {
                for declaration in &data.declarations {
                    match declaration {
                        Declaration::ClassOrStruct(content) => {
                            if content.extension {
                                if let Some(extended_class) = classes.iter_mut().find(|c| {
                                    content.name.0 == c.name.0
                                }) {
                                    if let Some(extension_class_name) = &content.inheritance {
                                        let line_number = &*source_line(&original_source, content.span.start, filename);
                                        
                                        if let Some(inheritance) = &extended_class.inheritance {
                                            let extension_class_name_with_line_number = &format!("\
                                                {line_number}\
                                                \t{extension_class_name}\n\
                                            ");
                                            inheritance.to_owned().push_str(&format!(", {extension_class_name_with_line_number}"));
                                        } else {
                                            let extension_class_name_with_line_number = &format!("\
                                                {line_number}\
                                                \t{extension_class_name}\n\
                                            ");
                                            extended_class.inheritance = Some(extension_class_name_with_line_number.to_string());
                                        }
                                    }
                                }
                            } else {
                                classes.push(content.clone());
                            }
                        }
                        _ => {}
                    }
                }
            }
            
            FppSection::Implementation(data) => {
                for declaration in &data.declarations {
                    match declaration {
                        Declaration::Method(content) => {
                            if let Some(class) = classes.iter_mut().find(|class| {
                                content.parent.get(original_source.text()) == class.name.0
                            }) {
                                class.additional_methods.push(content.clone());
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    
    // second pass (printing)
    const DO_NOT_EDIT: &str = "// AUTOMATICALLY GENERATED -- DO NOT EDIT!         -*- c++ -*-\n\n";
    
    let mut public_header = String::new();
    let mut private_header = String::new();
    let mut implementation = String::new();
    
    // public header
    public_header.push_str(DO_NOT_EDIT);
    public_header.push_str("#pragma once\n");
    public_header.push_str("\n//\n// INTERFACE definition follows\n//\n\n");
    
    // private header
    private_header.push_str(DO_NOT_EDIT);
    private_header.push_str("#pragma once\n");
    private_header.push_str(&format!("#include \"{filename}.h\"\n"));

    // implementation
    implementation.push_str(DO_NOT_EDIT);
    implementation.push_str(&format!("#include \"{filename}.h\"\n"));
    implementation.push_str(&format!("#include \"{filename}_i.h\"\n"));
    implementation.push_str("\n//\n// IMPLEMENTATION definition follows\n//\n\n");
    
    for section in translation_unit.sections {
        if let Some(guard) = match &section {
            FppSection::Interface(data) | FppSection::Implementation(data) => &data.guard,
        } {
            if !guard_matches(guard.as_str(), &active_tags) {
                continue;
            }
        }
        
        match section {
            FppSection::Interface(data) => {
                for class in &classes {
                    public_header.push_str(&*source_line(&original_source, class.span.start, filename));
                    let class_name = &class.name;
                    public_header.push_str(&format!("\
                        class {class_name} \
                    "));

                    if let Some(inheritance) = &class.inheritance {
                        public_header.push_str(":\n");
                        public_header.push_str(&format!("{inheritance}"));
                    }
                    public_header.push_str("\n{\n");
                    
                    public_header.push_str("public:\n");
                    for public_method in class.additional_methods.iter().filter(|method| {
                        method.access == Some(AccessSpecifier::Public)
                    }) {
                        public_header.push_str(&*source_line(&original_source, public_method.span.start, filename));
                        let prefix = public_method.get_sanitized_prefix(original_source.text());
                        let parent = public_method.parent.get(original_source.text());
                        let name = public_method.name.get(original_source.text());
                        let parameters = public_method.parameters.as_deref().unwrap_or("()");
                        let suffix = &public_method.suffix;
                        let body = public_method.body.to_string();
                        
                        public_header.push_str(&format!("\
                            \t{prefix}\n\
                            \t{name}{parameters}{suffix};\n\
                        "));
                        
                        implementation.push_str(&format!("\
                            {prefix}\n\
                            {parent}::{name}{parameters}{suffix}\n\
                            {body}\n\
                        "));
                    }

                    public_header.push_str("};\n");
                }
            }
            FppSection::Implementation(data) => {
                for declaration in data.declarations {
                    match declaration {
                        Declaration::PreprocessLine(span) => {
                            implementation.push_str(&*source_line(&original_source, span.start, filename));
                            let preprocess_line = span.get(original_source.text());
                            implementation.push_str(&format!("\
                                {preprocess_line}\n\
                            "));
                        }
                        Declaration::ClassOrStruct(content) => {}
                        Declaration::Function(function) => {
                            let line_number = &*source_line(&original_source, function.span.start, filename);
                            
                            let prefix = function.get_prefix_span().get(original_source.text());
                            let name = function.name.get(original_source.text());
                            let parameters = function.parameters.as_deref().unwrap_or("()");
                            let suffix = function.suffix;
                            let body = function.body;
                            
                            if !body.is_empty() {
                                public_header.push_str(line_number);
                                public_header.push_str(&format!("\
                                    {prefix}\n{name}{parameters}{suffix};\n\
                                "));
                            }
                            
                            implementation_includes.retain(|impl_include| {
                                match impl_include {
                                    Declaration::PreprocessLine(span) => {
                                        public_header.push_str("\n//\n// IMPLEMENTATION includes follow (for use by inline functions/templates)\n//\n\n");
                                        public_header.push_str(&*source_line(&original_source, span.start, filename));
                                        let preprocess_line = span.get(original_source.text());
                                        public_header.push_str(&format!("\
                                            {preprocess_line}\n\
                                        "));
                                        false
                                    }
                                    _ => true
                                }
                            });
                            
                            let full_function = function.span.get(original_source.text());
                            if !function.templates.is_empty() {
                                public_header.push_str("\n//\n// IMPLEMENTATION of function templates\n//\n\n");
                                public_header.push_str(line_number);
                                public_header.push_str(&format!("\
                                    {full_function}\n\
                                "));
                            } else {
                                implementation.push_str(line_number);
                                implementation.push_str(&format!("\
                                    {full_function}\n\
                                "));
                            }
                        }
                        Declaration::Method(content) => {}
                        Declaration::ExternC(content) => {}
                        Declaration::Verbatim(content) => {}
                    }
                }
            }
        }
    }
    
    CPPTriple{
        filename: filename.to_string(),
        public_header,
        private_header,
        implementation,
    }
}
