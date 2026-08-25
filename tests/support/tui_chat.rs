#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use nefor_tui::engine::Engine;
use nefor_tui::mouse::{MouseKind, MouseMessage};
use serde_json::{json, Map as JsonMap, Value as JsonValue};

static TEST_DATA_HOME: OnceLock<tempfile::TempDir> = OnceLock::new();

pub(crate) fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|candidate| {
            candidate
                .join("examples/nefor-agent/chat/init.lua")
                .is_file()
        })
        .expect("test manifest must be beneath the Nefor repository")
        .to_path_buf()
}

pub(crate) fn ensure_test_data_home() {
    let dir = TEST_DATA_HOME
        .get_or_init(|| tempfile::tempdir().expect("create per-process test data home"));
    if std::env::var_os("NEFOR_DATA_DIR").is_none() {
        std::env::set_var("NEFOR_DATA_DIR", dir.path());
    }
}

pub(crate) fn chat_lua_source() -> String {
    let default_data_home = TEST_DATA_HOME
        .get_or_init(|| tempfile::tempdir().expect("create per-process test data home"));
    let data_home = std::env::var_os("NEFOR_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| default_data_home.path().to_path_buf());
    let root = repository_root();
    let source = std::fs::read_to_string(root.join("examples/nefor-agent/chat/init.lua"))
        .expect("read canonical chat entry");
    format!(
        r#"
        local real_getenv = os.getenv
        local overrides = {{
          NEFOR_DATA_DIR = {data:?},
          NEFOR_STARTER_CONFIG_DIR = {config:?},
          NEFOR_STARTER_CHAT_DIR = {chat:?},
          NEFOR_TUI_LUA_DIR = {tui:?},
          NEFOR_DEFAULT_PROVIDER = "mock-plugin",
          NEFOR_DEFAULT_MODEL = "mock-model",
        }}
        os.getenv = function(name)
          if overrides[name] ~= nil then return overrides[name] end
          return real_getenv(name)
        end
        {source}
        "#,
        data = data_home.display().to_string(),
        config = root.join("examples/nefor-agent").display().to_string(),
        chat = root.join("examples/nefor-agent/chat").display().to_string(),
        tui = root.join("plugins/nefor-tui/lua").display().to_string(),
    )
}

pub(crate) fn canonical_chat_lua_source_for_config(config_dir: &Path) -> String {
    let default_data_home = TEST_DATA_HOME
        .get_or_init(|| tempfile::tempdir().expect("create per-process test data home"));
    let data_home = std::env::var_os("NEFOR_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| default_data_home.path().to_path_buf());
    let root = repository_root();
    let source = std::fs::read_to_string(root.join("examples/nefor-agent/chat/init.lua"))
        .expect("read canonical chat entry");
    format!(
        r#"
        local real_getenv = os.getenv
        local overrides = {{
          NEFOR_DATA_DIR = {data:?},
          NEFOR_CONFIG_DIR = {config:?},
          NEFOR_STARTER_CONFIG_DIR = {config:?},
          NEFOR_STARTER_CHAT_DIR = {chat:?},
          NEFOR_LOCAL_DIR = {repo:?},
          NEFOR_TUI_LUA_DIR = {tui:?},
          NEFOR_LUA_DIR = {lua:?},
          NEFOR_DEFAULT_PROVIDER = "mock-plugin",
          NEFOR_DEFAULT_MODEL = "mock-model",
        }}
        os.getenv = function(name)
          if overrides[name] ~= nil then return overrides[name] end
          return real_getenv(name)
        end
        os.execute = function() return true end
        {source}
        local config_ok, config_error = pcall(function() return require("config").active end)
        assert(config_ok, tostring(config_error))
        "#,
        data = data_home.display().to_string(),
        config = config_dir.display().to_string(),
        chat = root.join("examples/nefor-agent/chat").display().to_string(),
        repo = root.display().to_string(),
        tui = root.join("plugins/nefor-tui/lua").display().to_string(),
        lua = root.join("lua").display().to_string(),
    )
}

pub(crate) fn load_chat_scenario(engine: &mut Engine) {
    engine.load_scenario(&chat_lua_source()).expect("load");
    let emits = engine.take_emit_queue();
    assert_eq!(
        emits.len(),
        1,
        "chat startup should emit readiness exactly once"
    );
    let (target_hint, body) = &emits[0];
    assert_eq!(target_hint, &None, "chat readiness is broadcast");
    assert_eq!(
        body.get("kind").and_then(JsonValue::as_str),
        Some("chat.surface.ready"),
        "chat startup should emit only its readiness signal"
    );
}

pub(crate) fn render_str(engine: &mut Engine) -> String {
    engine
        .render_if_dirty()
        .expect("render")
        .map(|bytes| String::from_utf8(bytes).expect("ansi is utf-8"))
        .unwrap_or_default()
}

pub(crate) fn dispatch_event_from(engine: &mut Engine, source: &str, body: JsonValue) {
    let map: JsonMap<String, JsonValue> = body.as_object().expect("event body").clone();
    engine
        .dispatch_envelope_from(&map, source)
        .expect("dispatch event");
}

pub(crate) fn dispatch_event(engine: &mut Engine, body: JsonValue) {
    let source = match body.get("kind").and_then(JsonValue::as_str) {
        Some("mag.run_started") => "mag",
        Some("chat.instruction.notice") => "engine",
        _ => "test",
    };
    dispatch_event_from(engine, source, body);
}

pub(crate) fn activate_conversation(engine: &mut Engine, conversation_id: &str) {
    dispatch_event(
        engine,
        json!({ "kind": "conversation.active.changed", "conversation_id": conversation_id }),
    );
}

pub(crate) fn seed_completed_assistant_text(engine: &mut Engine, text: &str) {
    activate_conversation(engine, "live-clipboard");
    for change in [
        json!({"kind":"turn_started","turn_id":"turn","run_id":"turn"}),
        json!({"kind":"message_started","turn_id":"turn","message":{"id":"message","turn_id":"turn","role":"assistant"}}),
        json!({"kind":"content_chunk_appended","turn_id":"turn","message_id":"message","chunk":{"kind":"text","data":text}}),
        json!({"kind":"message_completed","turn_id":"turn","message":{"id":"message","turn_id":"turn","role":"assistant","text":text,"terminal":{"model":"test","duration_ms":1}}}),
        json!({"kind":"turn_completed","turn_id":"turn","run_id":"turn","terminal":{"model":"test","duration_ms":1}}),
    ] {
        dispatch_event(
            engine,
            json!({"kind":"conversation.projection.delta","conversation_id":"live-clipboard","change":change}),
        );
    }
}

pub(crate) fn drag_token(engine: &mut Engine, token: &str) {
    let snapshot = engine.snapshot();
    let row = snapshot
        .lines()
        .position(|line| line.contains(token))
        .expect("token row");
    let column = snapshot
        .lines()
        .nth(row)
        .expect("token line")
        .find(token)
        .expect("token column");
    let start = column as u16;
    let end = (column + token.len() - 1) as u16;
    for kind in [MouseKind::Click, MouseKind::Drag, MouseKind::Up] {
        engine
            .handle_mouse(MouseMessage {
                kind,
                x: if kind == MouseKind::Click { start } else { end },
                y: row as u16,
                button: Some("left"),
                mods: vec![],
            })
            .expect("mouse selection");
    }
}
