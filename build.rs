fn main() {
    // Compile the pure-C tetris engine used by the tetris-c scenario.
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    cc::Build::new()
        .file(format!("{manifest}/tetris-c/vendor/tetris_ffi.c"))
        .std("c11")
        .opt_level(3)
        .compile("tetris");
    println!("cargo:rerun-if-changed=tetris-c/vendor/tetris_ffi.c");
    println!("cargo:rerun-if-changed=tetris-c/vendor/board.c");

    // Expose workspace members as a compile-time env var for `eevee-eval -l`.
    let toml = std::fs::read_to_string(format!("{manifest}/Cargo.toml")).expect("Cargo.toml");
    let members = workspace_members(&toml);
    println!("cargo:rustc-env=WORKSPACE_MEMBERS={}", members.join(","));
    println!("cargo:rerun-if-changed=Cargo.toml");
}

fn workspace_members(toml: &str) -> Vec<String> {
    let mut in_workspace = false;
    for line in toml.lines() {
        let t = line.trim();
        if t == "[workspace]" {
            in_workspace = true;
            continue;
        }
        if t.starts_with('[') {
            in_workspace = false;
        }
        if in_workspace && t.starts_with("members") {
            if let Some(inner) = t.split('[').nth(1).and_then(|s| s.split(']').next()) {
                return inner
                    .split(',')
                    .map(|s| s.trim().trim_matches('"').to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
        }
    }
    vec![]
}
