use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // Each grammar uses its own generated headers. The official Rust bindings
    // compile the runtime; do not also link the old vendored runtime.
    for (name, directory, scanner) in [
        ("zig", "vendor/tree-sitter-zig/src", false),
        ("javascript", "vendor/tree-sitter-javascript/src", true),
        (
            "typescript",
            "vendor/tree-sitter-typescript/typescript/src",
            true,
        ),
        ("tsx", "vendor/tree-sitter-typescript/tsx/src", true),
        ("rust", "vendor/tree-sitter-rust/src", true),
        ("c", "vendor/tree-sitter-c/src", false),
        ("cpp", "vendor/tree-sitter-cpp/src", true),
        ("python", "vendor/tree-sitter-python/src", true),
        ("gn", "vendor/tree-sitter-gn/src", true),
    ] {
        let directory = Path::new(directory);
        println!("cargo:rerun-if-changed={}", directory.display());
        let mut build = cc::Build::new();
        build
            .include(directory)
            .file(directory.join("parser.c"))
            .std("c11")
            .opt_level(2)
            .warnings(false);
        if scanner {
            build.file(directory.join("scanner.c"));
        }
        build.compile(&format!("diffo_tree_sitter_{name}"));
    }
    println!("cargo:rerun-if-changed=vendor/tree-sitter-typescript/common");
}
