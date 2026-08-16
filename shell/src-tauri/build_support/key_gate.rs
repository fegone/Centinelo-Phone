// Byte-array extraction + known dev/test placeholder values for
// build.rs's release key-gate check
// (`check_no_dev_placeholder_keys_in_release` in build.rs). Split out
// into its own file - and `include!`d from BOTH build.rs (real use) and
// `src/key_gate_tests.rs` (via `include!`, purely so `cargo test` can
// exercise it) - for the exact same reason
// `build_support/generate_handler_parser.rs` is: build.rs is its own
// build-script binary that `cargo test` never runs, so without this split
// the extractor (the part most likely to silently drift and blind the
// gate) would have zero test coverage. See
// `src/generate_handler_parser_tests.rs`'s header comment for the
// precedent this follows.

/// Public half of the well-known, publicly-documented dev/test Ed25519
/// seed `[0x24; 32]` - see `src/premium.rs`'s `LIB_PUBKEY_BYTES` doc
/// comment and `premium/docs/PACKAGING-SIGNATURES.md`. NOT a secret:
/// anyone can regenerate it from that seed, and it already ships in the
/// clear as `LIB_PUBKEY_BYTES` itself. Duplicated here only so the
/// release key-gate check can recognize "the constant still holds the
/// placeholder" without adding `ed25519-dalek`/`centinelo-premium-abi` as
/// a build-dependency just to derive it at build time.
const DEV_LIB_PUBKEY_BYTES: [u8; 32] = [
    0x58, 0x93, 0x66, 0x04, 0xab, 0xda, 0x11, 0x2b, 0xc9, 0x49, 0x33, 0x56, 0x9c, 0x82, 0xf8, 0xd0,
    0xcc, 0x0d, 0xdf, 0x92, 0xa3, 0xf8, 0x32, 0x9f, 0x2f, 0x44, 0x8f, 0x7f, 0x48, 0x4a, 0x59, 0x4c,
];

/// Same rationale as [`DEV_LIB_PUBKEY_BYTES`] above, for
/// `src/activation.rs`'s `ACTIVATION_PUBKEY_BYTES` dev/test placeholder.
const DEV_ACTIVATION_PUBKEY_BYTES: [u8; 32] = [
    200, 83, 173, 15, 12, 210, 182, 25, 174, 169, 44, 238, 196, 253, 86, 162, 77, 100, 153, 213,
    132, 206, 121, 37, 126, 69, 207, 216, 19, 155, 96, 167,
];

/// Extracts a `const NAME: [u8; N] = [ ... ];` byte array's values from
/// Rust source text, tolerant of hex (`0x58`) or decimal (`200`)
/// literals and any whitespace/newline formatting rustfmt might apply.
/// Locates the FIRST `= [` after `const_name` (the initializer, not the
/// `[u8; N]` type annotation's own bracket, which comes before the `=`)
/// and reads up to the next `]`. Used only by the release key-gate check
/// - not a general-purpose Rust parser.
fn extract_const_bytes(source: &str, const_name: &str) -> Result<Vec<u8>, String> {
    let idx = source
        .find(const_name)
        .ok_or_else(|| format!("could not find `{const_name}` in source"))?;
    let after = &source[idx..];
    let list_start = after
        .find("= [")
        .ok_or_else(|| format!("could not find `= [` after `{const_name}`"))?
        + "= [".len();
    let rel_end = after[list_start..]
        .find(']')
        .ok_or_else(|| format!("could not find closing `]` for `{const_name}`'s initializer"))?;
    let list_str = &after[list_start..list_start + rel_end];

    list_str
        .split(',')
        .map(str::trim)
        .filter(|tok| !tok.is_empty())
        .map(|tok| {
            if let Some(hex) = tok.strip_prefix("0x") {
                u8::from_str_radix(hex, 16)
            } else {
                tok.parse::<u8>()
            }
            .map_err(|e| format!("bad byte literal `{tok}` in `{const_name}`: {e}"))
        })
        .collect()
}
