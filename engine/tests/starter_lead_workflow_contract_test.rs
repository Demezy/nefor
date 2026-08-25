use std::path::PathBuf;
use std::sync::Mutex;

mod support;

use support::ScopedEnvVar;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repo root is one level above engine")
        .to_path_buf()
}

#[test]
fn scoped_env_restores_after_normal_completion() {
    static TEST_LOCK: Mutex<()> = Mutex::new(());
    const KEY: &str = "NEFOR_TEST_SCOPED_ENV_NORMAL";
    std::env::remove_var(KEY);
    {
        let _env = ScopedEnvVar::set(&TEST_LOCK, KEY, "temporary");
        assert_eq!(
            std::env::var_os(KEY).as_deref(),
            Some(std::ffi::OsStr::new("temporary"))
        );
    }
    assert_eq!(std::env::var_os(KEY), None);
}

#[test]
fn scoped_env_restores_during_unwind() {
    static TEST_LOCK: Mutex<()> = Mutex::new(());
    const KEY: &str = "NEFOR_TEST_SCOPED_ENV_UNWIND";
    std::env::set_var(KEY, "original");
    let result = std::panic::catch_unwind(|| {
        let _env = ScopedEnvVar::set(&TEST_LOCK, KEY, "temporary");
        panic!("deliberate unwind");
    });
    assert!(result.is_err());
    assert_eq!(
        std::env::var_os(KEY).as_deref(),
        Some(std::ffi::OsStr::new("original"))
    );
    std::env::remove_var(KEY);
}

#[test]
fn lead_side_never_reads_the_retired_graph_ir_shape() {
    for rel in [
        "lua/libs/lead-workflow/init.lua",
        "lua/libs/mag-workspace/init.lua",
    ] {
        let path = repo_root().join(rel);
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        for needle in ["ir.nodes", "ir.edges", "ir.terminal"] {
            assert!(
                !src.contains(needle),
                "{rel} still reads the retired graph IR shape (`{needle}`)"
            );
        }
    }
    let mag_lua = repo_root().join("lua/libs/mag-workspace/init.lua");
    let src = std::fs::read_to_string(mag_lua).expect("read lua/libs/mag-workspace/init.lua");
    assert!(
        !src.contains("io.popen"),
        "lua/libs/mag-workspace/init.lua still shells out to the mag CLI"
    );
}
