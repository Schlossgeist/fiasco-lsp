use std::fs;
use std::path::Path;
use super::generator::*;
use super::parser::{parse_translation_unit};
use pest::Parser;

use rstest::rstest;

#[rstest]
// #[case::test_c_preproc              ("c-preproc.cpp")]
// #[case::test_comment_in_string      ("comment_in_string.cpp")]
// #[case::test_default_args           ("default_args.cpp")]
// #[case::test_dropsection            ("dropsection.cpp")]
// #[case::test_dropsection_ext        ("dropsection-ext.cpp")]
// #[case::test_explicit               ("explicit.cpp")]
#[case::test_extension_inherit      ("extension_inherit.cpp")]
// #[case::test_extern_c               ("extern_c.cpp")]
// #[case::test_func_defs              ("func_defs.cpp")]
// #[case::test_implement_default      ("implement_default.cpp")]
// #[case::test_implement_default_err  ("implement_default_err.cpp")]
// #[case::test_implement_template     ("implement_template.cpp")]
// #[case::test_inline                 ("inline.cpp")]
// #[case::test_interface              ("interface.cpp")]
// #[case::test_line                   ("line.cpp")]
// #[case::test_mapping                ("mapping.cpp")]
// #[case::test_multifile1             ("multifile1.cpp")]
// #[case::test_multifile2             ("multifile2.cpp")]
// #[case::test_nested_class           ("nested_class.cpp")]
// #[case::test_noinline               ("noinline.cpp")]
// #[case::test_operator               ("operator.cpp")]
// #[case::test_parser                 ("parser.cpp")]
// #[case::test_static                 ("static.cpp")]
// #[case::test_tag_enabled            ("tag_enabled.cpp")]
// #[case::test_template               ("template.cpp")]
// #[case::test_template_base_class    ("template_base_class.cpp")]
// #[case::test_variable               ("variable.cpp")]
fn test_parse(#[case] path: &str) {
    let data_dir = Path::new("preprocess/tool/preprocess/test");
    let source_file = data_dir.join(path);
    
    let input = fs::read_to_string(&source_file).unwrap();
    let result = parse_translation_unit(&input);
    
    assert!(result.is_ok());
    
    let source = Source::new(input);
    
    let cpp = generate_cpp_triple(source_file.file_stem().unwrap().to_str().unwrap(),
                                  source,
                                  Vec::new(),
                                  result.unwrap());
    
    println!("{}", cpp.public_header);
    println!("{}", cpp.private_header);
    println!("{}", cpp.implementation);
}
