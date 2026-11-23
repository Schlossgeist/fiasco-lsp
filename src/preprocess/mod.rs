//! Standalone reimplementation of Fiasco's `preprocess` tool.

pub mod chunks;
pub mod cli;
pub mod document;
pub mod generator;
pub mod source;
pub mod tags;

pub use generator::Severity;
pub use source::SourceFile;

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use std::collections::BTreeSet;
    use std::fs;
    use tree_sitter::{Node, Parser};
    use std::path::Path;

    /// Upstream preprocess test inputs, provided by the shallow
    /// `tests/fixtures/fiasco` submodule (see `tests/fixtures/README.md`).
    /// Collects the 1-based lines on which outermost ERROR or MISSING nodes start.
    fn collect_error_lines(node: Node, lines: &mut BTreeSet<usize>) {
        if node.is_error() || node.is_missing() {
            lines.insert(node.start_position().row + 1);
            return;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.has_error() || child.is_missing() {
                collect_error_lines(child, lines);
            }
        }
    }

    const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/fiasco/tool/preprocess/test");

    /// Each case lists the lines on which syntax errors are expected. The
    /// upstream tool works on raw text, so some inputs are not valid C++
    /// (unbalanced parentheses, undefined macros, text inside `#if 0`, ...).
    #[rstest]
    #[case::c_preproc("c-preproc.cpp", &[])]
    #[case::comment_in_string("comment_in_string.cpp", &[9])]
    #[case::default_args("default_args.cpp", &[32])]
    #[case::dropsection("dropsection.cpp", &[])]
    #[case::dropsection_ext("dropsection-ext.cpp", &[])]
    #[case::explicit("explicit.cpp", &[])]
    #[case::extension_inherit("extension_inherit.cpp", &[])]
    #[case::extern_c("extern_c.cpp", &[])]
    #[case::func_defs("func_defs.cpp", &[50])]
    #[case::implement_default("implement_default.cpp", &[])]
    #[case::implement_default_err("implement_default_err.cpp", &[])]
    #[case::implement_template("implement_template.cpp", &[])]
    #[case::inline("inline.cpp", &[])]
    #[case::interface("interface.cpp", &[])]
    #[case::line("line.cpp", &[])]
    #[case::mapping("mapping.cpp", &[])]
    #[case::multifile1("multifile1.cpp", &[])]
    #[case::multifile2("multifile2.cpp", &[])]
    #[case::nested_class("nested_class.cpp", &[])]
    #[case::noinline("noinline.cpp", &[])]
    #[case::operator("operator.cpp", &[])]
    #[case::parser("parser.cpp", &[79, 89, 96])]
    #[case::static_("static.cpp", &[])]
    #[case::tag_enabled("tag_enabled.cpp", &[])]
    #[case::template("template.cpp", &[250])]
    #[case::template_base_class("template_base_class.cpp", &[])]
    #[case::variable("variable.cpp", &[])]
    fn parses_upstream_fixture(#[case] name: &str, #[case] expected_error_lines: &[usize]) {
        let path = Path::new(FIXTURE_DIR).join(name);
        let input = fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!("cannot read {} (run `git submodule update --init --remote --depth 1`): {e}", path.display())
        });
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_fpp::LANGUAGE.into()).unwrap();
        let tree = parser.parse(&input, None).unwrap();
        let mut lines = BTreeSet::new();
        collect_error_lines(tree.root_node(), &mut lines);
        let lines: Vec<usize> = lines.into_iter().collect();
        assert_eq!(lines, expected_error_lines, "{name}: unexpected syntax error lines");
    }
}
