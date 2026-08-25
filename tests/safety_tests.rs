//! Safety-jail conformance: protected roots, personal-data directories,
//! allowlisted cache subpaths and traversal-escape resistance.
//!
//! These call the library API directly so the exact `SafetyError` variant
//! can be asserted.

#![cfg(unix)]

mod common;

use std::path::Path;

use common::TestWorkspace;
use diskpulse::cleaner::validate_path_safety;
use diskpulse::errors::SafetyError;

fn expect_variant(path: &Path, what: &str) -> SafetyError {
    let err = validate_path_safety(path)
        .err()
        .unwrap_or_else(|| panic!("{what} was unexpectedly allowed: {}", path.display()));
    err
}

// ---------------------------------------------------------------------------
// A.1 — System root blocking
// ---------------------------------------------------------------------------

#[test]
fn filesystem_roots_are_rejected() {
    for root in [
        "/", "/bin", "/sbin", "/usr", "/usr/bin", "/etc", "/var", "/lib", "/boot", "/dev", "/sys",
        "/proc",
    ] {
        let path = Path::new(root);
        // macOS aliases (/etc -> /private/etc etc.) resolve to real locations;
        // both spellings must land in the system-path bucket.
        if !path.exists() {
            continue;
        }
        let err = expect_variant(path, root);
        assert!(
            matches!(err, SafetyError::ProtectedSystemPath(_)),
            "{root} classified as {err:?}"
        );
    }
}

#[test]
fn contents_of_protected_system_roots_are_rejected() {
    // /usr/bin exists on macOS/Linux; a child of a blocked root is blocked.
    if Path::new("/usr/bin").is_dir() {
        let deep = Path::new("/usr/bin/env");
        if deep.exists() {
            let err = expect_variant(deep, "/usr/bin/env");
            assert!(matches!(err, SafetyError::ProtectedSystemPath(_)));
        }
    }
}

// ---------------------------------------------------------------------------
// A.2 — User profile protection
// ---------------------------------------------------------------------------

#[test]
fn home_root_is_rejected() {
    let Some(home) = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf()) else {
        return;
    };
    let err = expect_variant(&home, "home root");
    assert!(
        matches!(err, SafetyError::ProtectedHomeRoot(_)),
        "home classified as {err:?}"
    );
}

#[test]
fn personal_data_dirs_and_their_contents_are_rejected() {
    let Some(dirs) = directories::UserDirs::new() else {
        return;
    };
    let candidates: Vec<std::path::PathBuf> =
        [dirs.document_dir(), dirs.desktop_dir(), dirs.download_dir()]
            .into_iter()
            .flatten()
            .map(Path::to_path_buf)
            .collect();

    assert!(
        !candidates.is_empty(),
        "platform exposes no personal dirs to test"
    );

    for dir in &candidates {
        let err = expect_variant(dir, dir.to_str().unwrap_or("personal dir"));
        assert!(
            matches!(err, SafetyError::ProtectedUserData(_)),
            "{} classified as {err:?}",
            dir.display()
        );

        // Contents inherit the protection.
        if dir.is_dir() {
            let nested = dir.join("diskpulse-must-not-touch.txt");
            let err = validate_path_safety(&nested).expect_err("nested allowed!");
            assert!(matches!(err, SafetyError::ProtectedUserData(_)));
        }
    }
}

// ---------------------------------------------------------------------------
// A.3 — Safe target subpaths are allowlisted
// ---------------------------------------------------------------------------

#[test]
fn cache_subpaths_under_home_are_allowed() {
    let Some(home) = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf()) else {
        return;
    };

    for safe in [
        home.join(".cache/diskpulse_test"),
        home.join(".cargo/registry/cache"),
        home.join(".npm/_cacache"),
    ] {
        validate_path_safety(&safe)
            .unwrap_or_else(|err| panic!("{} wrongly rejected: {err}", safe.display()));
    }
}

// ---------------------------------------------------------------------------
// A.4 — Traversal & escapement resistance
// ---------------------------------------------------------------------------

#[test]
fn traversal_into_system_root_is_canonicalized_then_rejected() {
    let ws = TestWorkspace::new();
    let base = ws.create_dir("cache");

    // Pop far enough past the sandbox to reach (or pass) the filesystem top:
    // canonicalization must collapse this to a protected location.
    let mut escape = base.clone();
    for _ in 0..40 {
        escape.push("..");
    }
    escape.push("etc");

    let err = expect_variant(&escape, "root escape via ..");
    assert!(
        matches!(err, SafetyError::ProtectedSystemPath(_)),
        "traversal classified as {err:?}"
    );
}

#[test]
fn traversal_between_personal_dirs_is_rejected() {
    let Some(home) = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf()) else {
        return;
    };
    let Some(documents) =
        directories::UserDirs::new().and_then(|d| d.document_dir().map(Path::to_path_buf))
    else {
        return;
    };

    // Start from an allowed spot and dot-dot into Documents.
    let sneak = home.join(".cache/diskpulse_test/../../Documents");
    let _ = documents; // existence of the destination is what matters

    let err = expect_variant(&sneak, ".cache -> Documents traversal");
    assert!(matches!(err, SafetyError::ProtectedUserData(_)));
}

#[test]
fn symlink_escape_is_judged_by_target_location() {
    let ws = TestWorkspace::new();
    ws.create_file("cache/harmless.bin", 16);

    // Link pointing at a protected location: validating the LINK path must
    // follow it and refuse.
    let Some(documents) =
        directories::UserDirs::new().and_then(|d| d.document_dir().map(Path::to_path_buf))
    else {
        return;
    };
    let evil_link = ws.join("cache/evil-docs-link");
    std::os::unix::fs::symlink(&documents, &evil_link).expect("create link");

    let err = expect_variant(&evil_link, "link into Documents");
    assert!(matches!(err, SafetyError::ProtectedUserData(_)));
}
