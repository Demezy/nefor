mod support;

#[path = "../../support/tui_chat.rs"]
mod chat_harness;

use nefor_tui::engine::Engine;

#[test]
fn mouse_drag_copies_selection_and_shows_toast() {
    support::require_explicit_capability();
    let mut engine = Engine::new(80, 24).expect("engine");
    chat_harness::load_chat_scenario(&mut engine);
    chat_harness::seed_completed_assistant_text(&mut engine, "selectable-token");
    let frame = chat_harness::render_str(&mut engine);
    assert!(
        frame.contains("selectable-token"),
        "expected token in pre-drag frame: {frame:?}"
    );
    chat_harness::drag_token(&mut engine, "selectable-token");
    let _ = chat_harness::render_str(&mut engine);
    let _ = engine.take_emit_queue();
    let post = engine.snapshot();
    assert!(
        post.contains("copied "),
        "expected 'copied N chars' toast after drag, got: {post:?}"
    );
    let needle = format!("copied {} chars", "selectable-token".len());
    assert!(
        post.contains(&needle),
        "expected exact toast `{needle}`, got: {post:?}"
    );
}

#[test]
fn mouse_drag_toast_overlays_input_and_statusline() {
    support::require_explicit_capability();
    let mut engine = Engine::new(80, 24).expect("engine");
    chat_harness::load_chat_scenario(&mut engine);
    chat_harness::seed_completed_assistant_text(&mut engine, "selectable-token");
    let _ = chat_harness::render_str(&mut engine);
    let pre = engine.snapshot();
    assert!(
        pre.lines()
            .any(|line| line.contains("Start chatting to see stats")),
        "expected statusline placeholder before toast: {pre:?}"
    );
    chat_harness::drag_token(&mut engine, "selectable-token");
    let _ = chat_harness::render_str(&mut engine);
    let _ = engine.take_emit_queue();
    let post = engine.snapshot();
    let label = format!("copied {} chars", "selectable-token".len());
    let bottom_rows = post.lines().rev().take(5).collect::<Vec<_>>().join("\n");
    assert!(
        bottom_rows.contains(&label),
        "expected toast label `{label}` in the bottom rows: {bottom_rows:?}"
    );
}
