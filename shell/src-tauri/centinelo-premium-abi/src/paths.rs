//! Where the loader looks for the premium dylib and its signature — a
//! single source of truth shared by the loader (public) and the
//! build/sign tooling (private), so neither side can drift from the other
//! about what the file is called.

use std::path::{Path, PathBuf};

/// The default `cargo build`/`cargo build --release` output name for the
/// `centinelo-premium` cdylib on this target OS. Deliberately just
/// Cargo's own default naming (`lib<name>.dylib` / `<name>.dll` /
/// `lib<name>.so`) rather than a custom rename — a custom name is one more
/// place the build script and the loader could disagree about what string
/// to use. `centinelo-premium`'s `[lib] name = "centinelo_premium"` (see
/// that crate's `Cargo.toml`) is what these filenames are derived from.
///
/// Linux has no shipping product target (product spec: Windows + macOS
/// only) but this still needs a value there — CI runs `cargo test` on
/// `ubuntu-latest` (see `.github/workflows/license-ci.yml`), and
/// `loader-poc`'s integration tests build and load a real dylib, so this
/// needs to resolve to *something* real on Linux too, exactly like
/// `centinelo-license::machine_fingerprint`'s "unsupported platform" arm
/// having to still compile everywhere even though the product doesn't ship
/// there.
pub fn expected_library_filename() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "libcentinelo_premium.dylib"
    }
    #[cfg(target_os = "windows")]
    {
        "centinelo_premium.dll"
    }
    #[cfg(target_os = "linux")]
    {
        "libcentinelo_premium.so"
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        compile_error!(
            "centinelo-premium-abi: add an expected_library_filename() arm for this target OS"
        );
    }
}

/// Side-car signature file suffix — see `premium/docs/loader-integration.md`
/// ("Side-car signature, not appended") for why this is a separate file
/// next to the dylib rather than bytes appended to it.
pub const SIGNATURE_FILE_SUFFIX: &str = ".sig";

/// Where the loader expects to find the premium dylib: directly beside the
/// running executable. `exe_dir` is the caller's `std::env::current_exe()`
/// parent directory — this function takes it as a parameter (rather than
/// calling `current_exe()` itself) so it stays a pure, easily-unit-tested
/// path-joining function with no I/O of its own; see
/// `premium/docs/loader-integration.md` for the per-platform installer
/// layout this assumes (dylib sits next to the shell binary in both the
/// macOS `.app/Contents/MacOS/` bundle and the Windows install directory).
pub fn expected_library_path(exe_dir: impl AsRef<Path>) -> PathBuf {
    exe_dir.as_ref().join(expected_library_filename())
}

/// Where the loader expects the dylib's integrity signature:
/// `<library filename>.sig`, next to the dylib itself.
pub fn expected_signature_path(exe_dir: impl AsRef<Path>) -> PathBuf {
    let mut name = expected_library_filename().to_string();
    name.push_str(SIGNATURE_FILE_SUFFIX);
    exe_dir.as_ref().join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_path_is_exe_dir_joined_with_filename() {
        let dir = Path::new("/opt/Centinelo Phone");
        let path = expected_library_path(dir);
        assert_eq!(path.file_name().unwrap(), expected_library_filename());
        assert_eq!(path.parent().unwrap(), dir);
    }

    #[test]
    fn signature_path_is_library_path_plus_sig_suffix() {
        let dir = Path::new("/opt/Centinelo Phone");
        let lib = expected_library_path(dir);
        let sig = expected_signature_path(dir);
        assert_eq!(sig, PathBuf::from(format!("{}.sig", lib.display())));
    }

    #[test]
    fn filename_is_platform_appropriate() {
        let name = expected_library_filename();
        assert!(name.contains("centinelo_premium"));
        #[cfg(target_os = "macos")]
        assert!(name.starts_with("lib") && name.ends_with(".dylib"));
        #[cfg(target_os = "windows")]
        assert!(name.ends_with(".dll") && !name.starts_with("lib"));
        #[cfg(target_os = "linux")]
        assert!(name.starts_with("lib") && name.ends_with(".so"));
    }
}
