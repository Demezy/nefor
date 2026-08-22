use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn active_target_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR").map_or_else(
        || repo_root().join("target"),
        |target| {
            let target = PathBuf::from(target);
            if target.is_absolute() {
                target
            } else {
                // Cargo runs an integration test with the package directory as
                // CWD, after resolving the caller's relative target from the
                // workspace root used for this repository's command surface.
                repo_root().join(target)
            }
        },
    )
}

fn binary_path(target_dir: &Path, binary: &str) -> PathBuf {
    target_dir
        .join("debug")
        .join(format!("{binary}{}", std::env::consts::EXE_SUFFIX))
}

pub fn require_prepared_binaries(
    test_target: &str,
    names: &[&'static str],
) -> HashMap<&'static str, PathBuf> {
    let target_dir = active_target_dir();
    let binaries: HashMap<_, _> = names
        .iter()
        .map(|name| (*name, binary_path(&target_dir, name)))
        .collect();
    let mut missing: Vec<_> = binaries
        .iter()
        .filter(|(_, path)| !path.is_file())
        .map(|(name, path)| format!("{name} ({})", path.display()))
        .collect();
    missing.sort();

    assert!(
        missing.is_empty(),
        "{test_target} requires prepared runtime binaries in the active Cargo target. Missing: {}. Run `just prepare-mag-e2e` with the same CARGO_TARGET_DIR, then retry the test.",
        missing.join(", ")
    );

    let paths: Vec<_> = binaries.values().cloned().collect();
    nefor_cargo_test_harness::verify_paths(&paths).unwrap_or_else(|error| {
        panic!(
            "{test_target} requires strictly signed runtime binaries in {}: {error}. Run `just prepare-mag-e2e` with the same CARGO_TARGET_DIR, then retry the test.",
            target_dir.display()
        )
    });
    binaries
}
