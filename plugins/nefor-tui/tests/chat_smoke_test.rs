#[path = "../../../tests/support/tui_chat.rs"]
mod chat_harness;

use nefor_tui::engine::Engine;

#[test]
fn chat_lua_loads_and_renders_initial_frame() {
    let mut engine = Engine::new(80, 24).expect("engine");
    chat_harness::load_chat_scenario(&mut engine);
    let out = chat_harness::render_str(&mut engine);
    assert!(
        out.contains("mock-model"),
        "initial statusline should show configured default model: {out:?}"
    );
    assert!(
        !out.contains("Start chatting to see stats"),
        "configured defaults should replace pre-chat placeholder: {out:?}"
    );
    for needle in ["type a message", "ype a message", "/help for keys"] {
        assert!(
            !out.contains(needle),
            "input placeholder should be empty, found {needle:?} in: {out:?}"
        );
    }
}
