fn main() {
    let grammar_dir = "../tree-sitter-mirin/src";

    println!("cargo:rerun-if-changed={grammar_dir}/parser.c");
    println!("cargo:rerun-if-changed={grammar_dir}/scanner.c");
    println!("cargo:rerun-if-changed={grammar_dir}/tree_sitter/parser.h");

    cc::Build::new()
        .include(grammar_dir)
        .file(format!("{grammar_dir}/parser.c"))
        .file(format!("{grammar_dir}/scanner.c"))
        .warnings(false)
        .compile("tree-sitter-mirin");
}
