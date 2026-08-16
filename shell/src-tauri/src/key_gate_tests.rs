// Fixture tests for build.rs's release key-gate extractor
// (`check_no_dev_placeholder_keys_in_release` in build.rs, RISK finding,
// licensing agent, 2026-08-16).
//
// The extractor + known placeholder byte arrays live in
// `build_support/key_gate.rs` and are spliced into TWO places: `build.rs`
// (real use) and here, via `include!`, purely so `cargo test` can
// exercise them - `build.rs` is its own build-script binary that `cargo
// test` never runs, so without this file the extractor had zero test
// coverage no matter how many tests exist downstream of it. Same
// precedent as `generate_handler_parser_tests.rs` (see that file's own
// header comment) for the ACL command parser.
include!("../build_support/key_gate.rs");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_hex_array_like_lib_pubkey_bytes() {
        let src = "const LIB_PUBKEY_BYTES: [u8; 4] = [\n    0x58, 0x93, 0x66, 0x04,\n];\n";
        assert_eq!(
            extract_const_bytes(src, "LIB_PUBKEY_BYTES").unwrap(),
            vec![0x58, 0x93, 0x66, 0x04]
        );
    }

    #[test]
    fn extracts_decimal_array_like_activation_pubkey_bytes() {
        let src = "pub const ACTIVATION_PUBKEY_BYTES: [u8; 4] = [\n    200, 83, 173, 15,\n];\n";
        assert_eq!(
            extract_const_bytes(src, "ACTIVATION_PUBKEY_BYTES").unwrap(),
            vec![200, 83, 173, 15]
        );
    }

    #[test]
    fn ignores_the_type_annotations_own_bracket() {
        // `[u8; 32]` (the type) has its own `[`/`]` BEFORE the `=` - the
        // extractor must skip past those and only read the initializer
        // list's brackets, or it would parse "u8; 32" as the byte list.
        let src = "const X: [u8; 2] = [\n    1, 2,\n];\n";
        assert_eq!(extract_const_bytes(src, "X").unwrap(), vec![1, 2]);
    }

    #[test]
    fn errors_on_missing_constant() {
        assert!(extract_const_bytes("const OTHER: [u8; 1] = [1];", "MISSING").is_err());
    }

    #[test]
    fn dev_lib_pubkey_bytes_matches_the_literal_committed_in_premium_rs() {
        let src = std::fs::read_to_string("src/premium.rs").expect("read src/premium.rs");
        let bytes = extract_const_bytes(&src, "LIB_PUBKEY_BYTES").expect("parse LIB_PUBKEY_BYTES");
        // Mutation-test tripwire for the gate itself: if someone edits
        // premium.rs's placeholder bytes (or reformats them in a way
        // extract_const_bytes can't follow) without updating
        // DEV_LIB_PUBKEY_BYTES here, this fails loudly in `cargo test`
        // instead of the release key-gate silently going blind on the
        // next --release build.
        assert_eq!(bytes, DEV_LIB_PUBKEY_BYTES);
    }

    #[test]
    fn dev_activation_pubkey_bytes_matches_the_literal_committed_in_activation_rs() {
        let src = std::fs::read_to_string("src/activation.rs").expect("read src/activation.rs");
        let bytes = extract_const_bytes(&src, "ACTIVATION_PUBKEY_BYTES")
            .expect("parse ACTIVATION_PUBKEY_BYTES");
        assert_eq!(bytes, DEV_ACTIVATION_PUBKEY_BYTES);
    }
}
