//! The premium capability catalog and the status a query about one can
//! return.
//!
//! Both enums cross the FFI boundary as plain `u32`, never as the Rust enum
//! type itself — see each type's "Why `u32` at the boundary" note. Turning a
//! foreign `u32` into one of these enums always goes through a checked
//! `from_u32`, never a `transmute`: an out-of-range discriminant on a
//! `#[repr(u32)]` fieldless enum is instant undefined behavior if you
//! transmute it, so this crate never does that, in either direction.

/// A premium capability the shell might ask about. Mirrors
/// `centinelo_license::FEATURE_*` string constants one-to-one (see
/// [`Capability::feature_name`]) — this crate cannot literally share that
/// constant with `centinelo-license` (this crate is public/vendored,
/// `centinelo-license` stays private), so the string is duplicated here by
/// hand. `loader-poc`'s test suite pins the two lists together (see its
/// `abi_feature_names_match_license_crate` test) so drift between them is a
/// CI failure, not a silent bug.
///
/// # Why `u32` at the FFI boundary, not `Capability` itself
///
/// [`crate::PremiumAbiV1::capability_status`] takes a plain `u32`, not this
/// enum. A shell built against a newer copy of this crate (more variants)
/// calling into an older dylib is fine either way. The dangerous direction
/// is an *older* shell — built against a `Capability` with fewer variants —
/// calling into thin air is not a concern (it can only ever construct
/// discriminants it knows about). The real risk this guards against is the
/// dylib side: if the FFI parameter type were `Capability` directly, a
/// mismatched build could hand a bit pattern with no matching variant to a
/// `#[repr(u32)]` enum parameter, which is undefined behavior for a
/// fieldless enum the instant it's *read*, before any of our code even
/// runs. Taking `u32` and converting with [`Capability::from_u32`] (a
/// checked `match`, not a transmute) turns that failure mode into an
/// ordinary [`crate::FfiResult::UnknownCapability`] return instead.
///
/// # Extending this enum
///
/// Adding a new capability (F4/F5: recording, metrics, ...) is additive and
/// safe: append a new variant with a fresh discriminant (never reuse or
/// renumber an existing one, for the same reason version numbers in
/// `centinelo-license::SCHEMA_VERSION` are never reused). Do **not** remove
/// a variant even after a feature ships for real — an old dylib built
/// against a slightly older enum copy should never be asked about a
/// capability number it can't `match` on, but if it somehow is,
/// `from_u32` returning `None` (-> `UnknownCapability`) keeps that safe
/// rather than a hard failure.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    /// Full BLF console: receptionist grid, drag-drop transfer. Free tier
    /// gets 4 basic BLF favorites (see the product spec §4); this is the
    /// premium console on top of that.
    BlfConsole = 1,
    /// Local (whisper.cpp) live call transcription + notes.
    Transcription = 2,
    /// Local call recording + MOS/jitter quality metrics.
    Recording = 3,
}

impl Capability {
    /// Every known capability, in ascending discriminant order. Used by
    /// `loader-poc`'s demo binary to enumerate "what could this dylib do"
    /// and by tests that need to walk the full set.
    pub const ALL: &'static [Capability] = &[
        Capability::BlfConsole,
        Capability::Transcription,
        Capability::Recording,
    ];

    /// The canonical `centinelo_license::FEATURE_*` string this capability
    /// is gated behind. `centinelo-premium` uses this to call
    /// `license.has(capability.feature_name())` — see that crate's
    /// `capability_status_for` — which is also why this crate cannot import
    /// the constant directly (that would make the public ABI crate depend
    /// on the private license crate).
    pub const fn feature_name(self) -> &'static str {
        match self {
            Capability::BlfConsole => "blf_console",
            Capability::Transcription => "transcription",
            Capability::Recording => "recording",
        }
    }

    /// Checked conversion from the raw `u32` that actually crosses the FFI
    /// boundary. Returns `None` for any discriminant this build doesn't
    /// recognize (an ABI/version-skew case, not a bug) — never transmutes.
    pub const fn from_u32(raw: u32) -> Option<Capability> {
        match raw {
            1 => Some(Capability::BlfConsole),
            2 => Some(Capability::Transcription),
            3 => Some(Capability::Recording),
            _ => None,
        }
    }

    /// The `u32` to send across the FFI boundary. A plain enum-to-integer
    /// cast is always safe in this direction (every `Capability` value is,
    /// by construction, one the `match` in `from_u32` above recognizes).
    pub const fn as_u32(self) -> u32 {
        self as u32
    }
}

/// The result of asking a loaded premium dylib about one [`Capability`].
///
/// # Why this exists as a status query rather than "just call it"
///
/// v0 of `centinelo-premium` has no capability with real behavior yet (BLF
/// console, transcription, and recording are all F3/F4 work) — the honest
/// v0 shape is a pure status probe: "if I called this, what would happen".
/// That keeps `capability_status` side-effect-free (safe to call
/// speculatively for UI purposes, e.g. graying out a menu item, without
/// worrying about triggering anything) and gives F4 a clean place to add
/// the actual per-capability invoke functions later without reshaping this
/// query.
///
/// # Ordering: license is checked before "is it built yet"
///
/// `centinelo-premium` always resolves [`CapabilityStatus::NotLicensed`]
/// before it would ever resolve [`CapabilityStatus::NotImplemented`] — see
/// that crate's `capability_status_for`. That ordering is deliberate and is
/// what `loader-poc`'s
/// `unlicensed_feature_blocked_while_licensed_feature_reaches_stub` test
/// actually proves: a license that omits a feature gets `NotLicensed`
/// *before* the v0-stub question is ever considered, while a license that
/// includes it clears the gate and reaches `NotImplemented`. Reversing that
/// order would make it impossible to tell, from the outside, whether the
/// license check ran at all.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityStatus {
    /// Licensed and implemented — safe to actually invoke.
    Available = 0,
    /// This build of the dylib implements the capability, but the active
    /// license does not include it. The shell's answer: show the
    /// free-tier UI, maybe an upsell — never crash, never nag-loop.
    NotLicensed = 1,
    /// The active license *does* include this feature, but this build of
    /// `centinelo-premium` hasn't implemented it yet (all of v0's
    /// capabilities resolve here once they clear the license gate).
    NotImplemented = 2,
    /// The capability is licensed and (nominally) implemented, but
    /// answering the status query itself failed — reserved for future use
    /// once a real implementation can fail (e.g. probing hardware). No v0
    /// code path produces this; it exists so the enum doesn't need another
    /// breaking wire change when one does.
    Error = 3,
}

impl CapabilityStatus {
    /// Checked conversion from the raw `u32` an out-parameter was filled
    /// with. See [`Capability::from_u32`] for why this is a `match`, not a
    /// transmute, and why that matters for a `#[repr(u32)]` fieldless enum.
    pub const fn from_u32(raw: u32) -> Option<CapabilityStatus> {
        match raw {
            0 => Some(CapabilityStatus::Available),
            1 => Some(CapabilityStatus::NotLicensed),
            2 => Some(CapabilityStatus::NotImplemented),
            3 => Some(CapabilityStatus::Error),
            _ => None,
        }
    }

    /// The `u32` a callee writes into the out-parameter. Always safe (see
    /// [`Capability::as_u32`]'s twin note).
    pub const fn as_u32(self) -> u32 {
        self as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_round_trips_through_u32() {
        for cap in Capability::ALL {
            assert_eq!(Capability::from_u32(cap.as_u32()), Some(*cap));
        }
    }

    #[test]
    fn capability_from_u32_rejects_unknown_discriminants() {
        assert_eq!(Capability::from_u32(0), None);
        assert_eq!(Capability::from_u32(4), None);
        assert_eq!(Capability::from_u32(u32::MAX), None);
    }

    #[test]
    fn capability_feature_names_are_distinct_and_snake_case() {
        let mut names: Vec<&str> = Capability::ALL.iter().map(|c| c.feature_name()).collect();
        let before = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), before, "feature_name values must be unique");
        for n in names {
            assert!(
                n.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{n:?} should be snake_case to match centinelo_license::FEATURE_* style"
            );
        }
    }

    #[test]
    fn capability_status_round_trips_through_u32() {
        for status in [
            CapabilityStatus::Available,
            CapabilityStatus::NotLicensed,
            CapabilityStatus::NotImplemented,
            CapabilityStatus::Error,
        ] {
            assert_eq!(CapabilityStatus::from_u32(status.as_u32()), Some(status));
        }
    }

    #[test]
    fn capability_status_from_u32_rejects_unknown_discriminants() {
        assert_eq!(CapabilityStatus::from_u32(4), None);
        assert_eq!(CapabilityStatus::from_u32(u32::MAX), None);
    }
}
