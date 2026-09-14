//! Unit tests for `lua/libs/read-only-tools` — the opt-in `include` seam.
//! The assertions live in `tests/lua/read-only-tools/build_test.lua`; this
//! harness stubs the `nefor` global, wires `package.path` to the shared Lua
//! tree, and runs it.

use std::path::PathBuf;
use std::sync::Mutex;

use mlua::{Function, Lua, Value};

mod support;

use support::ScopedEnvVar;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .expect("repo root is one level above engine")
        .to_path_buf()
}

#[test]
fn read_only_tools_include_seam() {
    let _repo_root = ScopedEnvVar::set(&ENV_LOCK, "NEFOR_REPO_ROOT", repo_root());
    let lua = Lua::new();
    install_stub_nefor(&lua).expect("install nefor stub");
    set_package_path(&lua).expect("set package.path");
    let fixture = tempfile::tempdir().expect("tool fixture");
    let skill_dir = fixture.path().join("skills/example");
    let workspace = fixture.path().join("workspace");
    std::fs::create_dir_all(&skill_dir).expect("skill directory");
    std::fs::create_dir_all(workspace.join("nested")).expect("workspace directory");
    std::fs::write(skill_dir.join("skill.md"), "Ordinary workflow skill.\n")
        .expect("skill fixture");
    std::fs::write(workspace.join("AGENTS.md"), "Root repository guidance.\n")
        .expect("root instruction fixture");
    std::fs::write(
        workspace.join("nested/CLAUDE.md"),
        "Nested repository guidance.\n",
    )
    .expect("nested instruction fixture");
    lua.globals()
        .set(
            "NEFOR_CONFIG_DIR",
            fixture.path().to_string_lossy().as_ref(),
        )
        .expect("config root");
    lua.globals()
        .set(
            "READ_ONLY_TEST_WORKSPACE",
            workspace.to_string_lossy().as_ref(),
        )
        .expect("workspace root");

    let test_path = repo_root().join("tests/lua/read-only-tools/build_test.lua");
    let src = std::fs::read_to_string(&test_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", test_path.display()));

    if let Err(e) = lua
        .load(&src)
        .set_name(test_path.display().to_string())
        .exec()
    {
        panic!("read-only-tools build_test.lua failed:\n{e}");
    }
}

fn install_stub_nefor(lua: &Lua) -> mlua::Result<()> {
    let nefor = lua.create_table()?;
    nefor::lua::bindings::install_json(lua, &nefor)?;

    let log_tbl = lua.create_table()?;
    let no_op: Function = lua.create_function(|_, _: mlua::Variadic<Value>| Ok(()))?;
    log_tbl.set("info", no_op.clone())?;
    log_tbl.set("warn", no_op.clone())?;
    log_tbl.set("error", no_op.clone())?;
    log_tbl.set("debug", no_op)?;
    nefor.set("log", log_tbl)?;

    lua.globals().set("nefor", nefor)?;
    Ok(())
}

fn set_package_path(lua: &Lua) -> mlua::Result<()> {
    let root = repo_root();
    let starter = root.join("examples/nefor-agent").display().to_string();
    let lua_root = root.join("lua").display().to_string();
    let tool_gate = root.join("plugins/tool-gate/lua").display().to_string();
    let script = format!(
        r#"
        package.path = table.concat({{
          "{starter}/?.lua",
          "{starter}/?/init.lua",
          "{lua_root}/?.lua",
          "{lua_root}/?/init.lua",
          "{tool_gate}/?.lua",
          "{tool_gate}/?/init.lua",
          package.path,
        }}, ";")
        "#,
    );
    lua.load(&script).exec()
}
