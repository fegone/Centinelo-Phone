use tauri_build::{AppManifest, Attributes};

// The actual parsing logic lives in build_support/generate_handler_parser.rs
// so it can ALSO be included (separately) from
// src/generate_handler_parser_tests.rs and get real `cargo test`
// coverage - build.rs is its own build-script binary, never compiled
// under `cargo test`, so keeping the parser only here would leave it
// with zero test coverage no matter how many tests exist downstream of
// it. See that file's own doc comment for the RELIABILITY 4R finding
// this split fixes (a `]` inside a `// comment` used to be able to
// truncate the command list silently).
include!("build_support/generate_handler_parser.rs");

fn main() {
    // Re-run this build script - and therefore re-derive the ACL command
    // list - whenever `generate_handler!`'s own file (or the parser
    // itself) changes, so adding or removing a command can never leave
    // stale permissions behind.
    println!("cargo:rerun-if-changed=src/lib.rs");
    println!("cargo:rerun-if-changed=build_support/generate_handler_parser.rs");

    let lib_rs = std::fs::read_to_string("src/lib.rs")
        .unwrap_or_else(|e| panic!("failed to read src/lib.rs for ACL command extraction: {e}"));
    let commands = commands_from_generate_handler(&lib_rs)
        .unwrap_or_else(|e| panic!("src/lib.rs: {e}"));

    // `AppManifest::commands` wants `&'static [&'static str]`; leaking is
    // fine here since build.rs is a short-lived process that exits right
    // after `try_build` returns.
    let commands: &'static [&'static str] = Vec::leak(
        commands
            .into_iter()
            .map(|s| -> &'static str { s.leak() })
            .collect(),
    );

    let attributes = Attributes::new().app_manifest(AppManifest::new().commands(commands));

    if let Err(error) = tauri_build::try_build(attributes) {
        // Mirrors tauri_build::build()'s own error handling (it isn't
        // reusable directly since it always builds with default
        // Attributes) so a broken ACL still fails the build with the same
        // "unknown field -> stale tauri-build" hint instead of a bare
        // panic.
        let error = format!("{error:#}");
        println!("{error}");
        if error.starts_with("unknown field") {
            print!("found an unknown configuration field. This usually means that you are using a CLI version that is newer than `tauri-build` and is incompatible. ");
            println!(
                "Please try updating the Rust crates by running `cargo update` in the Tauri app folder."
            );
        }
        std::process::exit(1);
    }
}
