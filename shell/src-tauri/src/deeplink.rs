//! `centinelo://` and `tel:` protocol handling (Tauri deep-link plugin,
//! `tauri_plugin_deep_link` 2.4.9 - API verified against its vendored
//! source, see module notes below for exactly what's platform-specific and
//! why).
//!
//! `centinelo` is always claimed - it's this app's own scheme, no conflict
//! risk, matches v1's unconditional `app.setAsDefaultProtocolClient
//! ('centinelo')` (src/main/main.js). `tel` is opt-in
//! (`settings.bridge.register_tel_handler`, off by default) since claiming
//! the OS-wide `tel:` handler competes with other apps (FaceTime, Skype,
//! ...) - matches v1's `registerTelHandler` setting exactly.
//!
//! Both schemes feed the *same* confirmation flow as the click-to-call
//! bridge (bridge.rs): a `"click-to-call"` Tauri event with
//! `{number, source, auto_dial}`, so a deep link never dials any more
//! silently than an HTTP bridge request does - one `auto_dial` setting, one
//! UI flow, two arrival paths. See ui/js/app.js.
//!
//! ## Platform split (why registration isn't symmetric)
//!
//! `tauri-plugin-deep-link`'s `register()`/`unregister()` only have a real
//! implementation on Windows (writes `HKCU\Software\Classes\<scheme>`) and
//! Linux (writes a `.desktop` file + `xdg-mime default`); on macOS they
//! unconditionally return `Error::UnsupportedPlatform` - Apple has no
//! runtime API for this, a URL scheme can only be claimed via the *bundled
//! app's* `Info.plist` (`CFBundleURLTypes`), generated at build time from
//! `tauri.conf.json`'s `plugins.deep-link.desktop.schemes`, which is why
//! that list includes `tel` unconditionally (both schemes) rather than only
//! `centinelo`.
//!
//! Net effect: on a *built and installed* macOS app, Centinelo is always
//! Info.plist-capable of handling `tel:` links (subject to the user picking
//! it in System Settings' default-apps picker, or macOS's app-chooser
//! prompt when more than one app claims the scheme) - the in-app toggle
//! there can't add/remove that OS-level capability, so instead it gates
//! whether an incoming `tel:` link is *acted on at all* (silently ignored
//! if off). On Windows/Linux the toggle is the real thing: it calls
//! `register("tel")`/`unregister("tel")`, which actually adds/removes the
//! OS association. Both readings satisfy "opt-in" honestly; which one
//! applies is a platform fact, not a shortcut - see shell/README.md's
//! "Design fidelity notes" for the project's general precedent of calling
//! out exactly this kind of scope/behavior split rather than papering over
//! it.
//!
//! `centinelo` itself is registered the same asymmetric way (always, no
//! setting) - `register("centinelo")` on Windows/Linux, a no-op on macOS
//! where the Info.plist entry alone is already sufficient (no OS "default
//! handler" contention to resolve for a scheme only this app claims).

use crate::settings::SettingsStore;
use crate::tray;
use std::sync::Arc;
use tauri::{App, AppHandle, Emitter};
use tauri_plugin_deep_link::DeepLinkExt;

const TEL_SCHEME: &str = "tel";
const CENTINELO_SCHEME: &str = "centinelo";

pub fn setup(app: &App, settings: Arc<SettingsStore>) {
    let handle_for_listener = app.handle().clone();
    let settings_for_listener = settings.clone();
    app.deep_link().on_open_url(move |event| {
        for url in event.urls() {
            handle_url(&handle_for_listener, &settings_for_listener, &url);
        }
    });

    // Covers "the app was just launched by clicking a link" - by the time
    // `.setup()` runs (and the `on_open_url` listener above exists), the
    // plugin's own init has already parsed this process's argv/launch URL
    // once; `get_current()` is how a consumer picks that up retroactively
    // (mirrors v1's `handleProtocolArgv(process.argv)` at the end of its own
    // `whenReady()`).
    if let Ok(Some(urls)) = app.deep_link().get_current() {
        let handle = app.handle().clone();
        for url in urls {
            handle_url(&handle, &settings, &url);
        }
    }

    apply_tel_registration(app.handle(), settings.snapshot().bridge.register_tel_handler);

    #[cfg(not(target_os = "macos"))]
    if let Err(e) = app.deep_link().register(CENTINELO_SCHEME) {
        log::warn!("deep-link: couldn't register centinelo:// scheme: {e}");
    }
}

/// Applies (Windows/Linux) or is a documented no-op (macOS) for the `tel:`
/// OS association per the setting - see module doc. Called at startup and
/// again whenever the setting changes (commands::set_register_tel_handler).
pub fn apply_tel_registration(app: &AppHandle, enabled: bool) {
    #[cfg(target_os = "macos")]
    {
        let _ = (app, enabled); // no-op on macOS, see module doc
    }
    #[cfg(not(target_os = "macos"))]
    {
        let result = if enabled {
            app.deep_link().register(TEL_SCHEME)
        } else {
            app.deep_link().unregister(TEL_SCHEME)
        };
        if let Err(e) = result {
            log::warn!("deep-link: couldn't update tel: registration (enabled={enabled}): {e}");
        }
    }
}

fn handle_url(app: &AppHandle, settings: &Arc<SettingsStore>, url: &url::Url) {
    let scheme = url.scheme();
    if scheme != TEL_SCHEME && scheme != CENTINELO_SCHEME {
        return; // not ours (shouldn't happen - the plugin only forwards configured schemes)
    }
    // `centinelo://provision?...` is a distinct verb from the dial links
    // this function otherwise handles (`centinelo://<number>`,
    // `centinelo://dial?number=...`) - routed to provisioning.rs before
    // any of the dial-target extraction below ever runs, same way this
    // function already special-cases nothing else about `centinelo:`
    // hosts today. See provisioning.rs's module doc for the full flow.
    if crate::provisioning::is_provision_link(url) {
        crate::provisioning::handle_deep_link(app.clone(), url.clone());
        return;
    }
    let snapshot = settings.snapshot();
    if scheme == TEL_SCHEME && !snapshot.bridge.register_tel_handler {
        log::info!("deep-link: ignoring a tel: link - \"Answer tel: links\" is off in Settings");
        return;
    }
    // Both log lines below are redacted (HIPAA remote-install hardening,
    // 2026-08-13) - a raw `tel:`/`centinelo:` URL and its extracted dial
    // target both carry a caller's phone number (PHI), and this handler
    // runs in every release build (no `debug_assertions` gate), so both
    // would otherwise land in the persistent on-disk log. See
    // `bridge::redacted_log_number`'s doc for why fingerprinting instead of
    // just dropping the module - "a malformed link arrived" and "the same
    // number was retried" both stay diagnosable without the digits.
    let Some(number) = extract_dial_target(url) else {
        log::warn!("{}", no_dial_target_log_line(url));
        return;
    };
    log::info!("{}", dial_target_log_line(scheme, &number, snapshot.bridge.auto_dial));
    let _ = app.emit(
        crate::bridge::EVENT_CLICK_TO_CALL,
        serde_json::json!({
            "number": number,
            "source": scheme,
            "auto_dial": snapshot.bridge.auto_dial,
        }),
    );
    tray::show_and_focus(app);
}

/// The line logged when a `tel:`/`centinelo:` link's target extraction
/// *fails* - HIPAA remote-install hardening (2026-08-13): the raw `url`
/// itself carries the caller's phone number (`tel:3525550199`), so this
/// must never interpolate `url` directly, only its scheme and length -
/// see `handle_url`'s doc for the full "why redact instead of drop the
/// module" reasoning. Pulled out as its own pure function so a test can
/// assert the guarantee directly instead of just exercising the predicate.
fn no_dial_target_log_line(url: &url::Url) -> String {
    format!("deep-link: couldn't find a number to dial in a {} link ({} chars)", url.scheme(), url.as_str().len())
}

/// The line logged when a link's dial target *is* found - same redaction
/// rule and reasoning as [`no_dial_target_log_line`], via
/// `bridge::redacted_log_number`.
fn dial_target_log_line(scheme: &str, number: &str, auto_dial: bool) -> String {
    format!(
        "deep-link: {scheme}: link for {} (auto_dial={auto_dial}) - {}",
        crate::bridge::redacted_log_number(number),
        if auto_dial { "dialing immediately" } else { "asking for confirmation" }
    )
}

/// Mirrors v1's `extractDialTarget()` (src/main/main.js): for `tel:`, keep
/// everything after the scheme; for `centinelo:`, check the `number` query
/// param first, then the host, then the path - covers
/// `centinelo://dial?number=501`, `centinelo://501` and `centinelo:501`
/// alike, in that precedence order. Percent-decodes (matches v1's
/// `decodeURIComponent`), then keeps only dial-able characters - the same
/// final filter clears both "not actually a number" (e.g. bare
/// `centinelo://dial` with no payload) and any leftover punctuation.
fn extract_dial_target(url: &url::Url) -> Option<String> {
    let decode = |s: &str| percent_encoding::percent_decode_str(s).decode_utf8_lossy().into_owned();
    let raw = match url.scheme() {
        TEL_SCHEME => decode(url.path()),
        CENTINELO_SCHEME => {
            let from_query = url
                .query_pairs()
                .find(|(k, _)| k.as_ref() == "number")
                .map(|(_, v)| v.into_owned());
            let from_host = url.host_str().filter(|h| !h.is_empty()).map(str::to_string);
            let from_path = {
                let decoded = decode(url.path());
                let trimmed = decoded.trim_start_matches('/');
                if trimmed.is_empty() { None } else { Some(trimmed.to_string()) }
            };
            from_query.or(from_host).or(from_path)?
        }
        _ => return None,
    };
    let cleaned: String = raw.chars().filter(|c| c.is_ascii_digit() || matches!(c, '+' | '*' | '#')).collect();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> url::Url {
        url::Url::parse(s).unwrap()
    }

    #[test]
    fn tel_plain() {
        assert_eq!(extract_dial_target(&u("tel:5551234")), Some("5551234".into()));
    }

    #[test]
    fn tel_percent_encoded_plus() {
        assert_eq!(extract_dial_target(&u("tel:%2B13525550199")), Some("+13525550199".into()));
    }

    #[test]
    fn tel_with_formatting() {
        assert_eq!(extract_dial_target(&u("tel:(352)%20555-0199")), Some("3525550199".into()));
    }

    #[test]
    fn centinelo_authority_with_query() {
        assert_eq!(extract_dial_target(&u("centinelo://dial?number=501")), Some("501".into()));
    }

    #[test]
    fn centinelo_authority_host_only() {
        assert_eq!(extract_dial_target(&u("centinelo://501")), Some("501".into()));
    }

    #[test]
    fn centinelo_opaque_path() {
        assert_eq!(extract_dial_target(&u("centinelo:501")), Some("501".into()));
    }

    #[test]
    fn centinelo_query_wins_over_host() {
        // query and host disagree - v1's precedence (searchParams first) wins.
        assert_eq!(extract_dial_target(&u("centinelo://999?number=501")), Some("501".into()));
    }

    #[test]
    fn centinelo_no_number_anywhere() {
        assert_eq!(extract_dial_target(&u("centinelo://dial")), None);
    }

    #[test]
    fn unrelated_scheme_ignored() {
        assert_eq!(extract_dial_target(&u("https://example.com/501")), None);
    }

    // HIPAA remote-install hardening (2026-08-13) - these pin the actual
    // guarantee (no PHI in the constructed log line), matching bridge.rs's
    // own tests for `redacted_log_number`.

    #[test]
    fn dial_target_log_line_never_contains_the_number() {
        let line = dial_target_log_line("tel", "3525550199", false);
        assert!(!line.contains("3525550199"), "log line leaked the raw number: {line}");
        assert!(line.contains("tel:"));
    }

    #[test]
    fn no_dial_target_log_line_never_contains_the_url() {
        let url = u("tel:3525550199");
        let line = no_dial_target_log_line(&url);
        assert!(!line.contains("3525550199"), "log line leaked the raw url: {line}");
        assert!(line.contains("tel"));
    }
}
