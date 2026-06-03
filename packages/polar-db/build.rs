// Compile and link the tree-sitter Polar grammar (C sources), exactly as
// `polar-compiler` does. Kept standalone so this crate does not depend on the
// old compiler crate. Two crates compiling the same grammar is fine: each
// produces its own static lib, and no single binary links both.
fn main() {
    let grammar_dir = "../tree-sitter-polar/src";

    println!("cargo:rerun-if-changed={grammar_dir}/parser.c");
    println!("cargo:rerun-if-changed={grammar_dir}/tree_sitter/parser.h");

    cc::Build::new()
        .include(grammar_dir)
        .file(format!("{grammar_dir}/parser.c"))
        .warnings(false)
        .compile("tree-sitter-polar");
}
