use mlua::{Lua, LuaSerdeExt, Table};
use std::path::PathBuf;

fn harness() -> Lua {
    let lua = Lua::new();
    let package: Table = lua.globals().get("package").unwrap();
    let current: String = package.get("path").unwrap();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lua/mag-kernel");
    package
        .set(
            "path",
            format!("{0}/?.lua;{0}/?/init.lua;{current}", root.display()),
        )
        .unwrap();
    let nefor = lua.create_table().unwrap();
    let json = lua.create_table().unwrap();
    let array_metatable = lua.array_metatable();
    json.set(
        "mark_array",
        lua.create_function(move |_, table: Table| {
            table.set_metatable(Some(array_metatable.clone()));
            Ok(table)
        })
        .unwrap(),
    )
    .unwrap();
    let array_metatable = lua.array_metatable();
    json.set(
        "is_array",
        lua.create_function(move |_, table: Table| {
            Ok(table
                .metatable()
                .is_some_and(|value| value.to_pointer() == array_metatable.to_pointer()))
        })
        .unwrap(),
    )
    .unwrap();
    nefor.set("json", json).unwrap();
    lua.globals().set("nefor", nefor).unwrap();
    lua
}

#[test]
fn collector_orders_recurrent_overlapping_cohorts_by_trusted_sender() {
    harness()
        .load(
            r#"
            local factory = require("factories.collector")
            local emitted = {}
            local actor = assert(factory.construct("join", {
              expected_senders = { "worker.0", "worker.1", "worker.2" }
            }, function(message) emitted[#emitted + 1] = message end))
            actor.deliver({ messages = {{ from = "worker.2", message = { value = "c", from = "forged" } }} })
            actor.deliver({ messages = {{ from = "worker.0", message = { value = "a" } }} })
            actor.deliver({ messages = {{ from = "worker.0", message = { value = "A" } }} })
            actor.deliver({ messages = {{ from = "worker.1", message = { value = "b" } }} })
            assert(#emitted == 2) -- ready + one output
            assert(emitted[2].kind == "nefor.dynamic.Collected")
            assert(table.concat(emitted[2].value, "") == "abc")
            actor.deliver({ messages = {{ from = "worker.1", message = { value = "B" } }} })
            local completion = actor.deliver({ messages = {{ from = "worker.2", message = { value = "C" } }} })
            assert(completion.status == "ok")
            assert(#emitted == 3)
            assert(table.concat(emitted[3].value, "") == "ABC")
            "#,
        )
        .exec()
        .unwrap();
}

#[test]
fn collector_rejects_bad_topology_and_arrivals_and_clears_on_kill() {
    harness()
        .load(
            r#"
            local factory = require("factories.collector")
            assert(factory.construct("zero", { expected_senders = {} }, function() end) == nil)
            assert(factory.construct("dup", { expected_senders = { "a", "a" } }, function() end) == nil)
            local actor = assert(factory.construct("join", { expected_senders = { "a", "b" } }, function() end))
            local unexpected = actor.deliver({ messages = {{ from = "x", message = { value = 1 } }} })
            assert(unexpected.status == "failed")
            assert(unexpected.value.kind == "collector_unexpected_sender")
            local drained_out = {}
            local drained = assert(factory.construct("drained", { expected_senders = { "a", "b" } },
              function(message) drained_out[#drained_out + 1] = message end))
            drained.deliver({ messages = {{ from = "a", message = { value = 1 } }} })
            drained.handle_drain()
            assert(drained_out[#drained_out].kind == "mag.failed")
            assert(drained_out[#drained_out].value.kind == "collector_drained_incomplete")
            local killed = assert(factory.construct("killed", { expected_senders = { "a" } }, function() end))
            killed.handle_kill()
            local after = killed.deliver({ messages = {{ from = "a", message = { value = 1 } }} })
            assert(after.status == "failed")
            "#,
        )
        .exec()
        .unwrap();
}

#[test]
fn empty_sequence_emits_the_fixed_empty_list_after_input() {
    harness()
        .load(
            r#"
            local factory = require("factories.sequence-empty")
            local emitted = {}
            local actor = assert(factory.construct("empty", {},
              function(message) emitted[#emitted + 1] = message end))
            local completion = actor.deliver({ messages = {{ message = { value = "trigger" } }} })
            assert(completion.status == "ok")
            assert(#emitted == 2) -- ready + the empty list
            assert(emitted[2].kind == "nefor.node.SequenceOutput")
            assert(type(emitted[2].value) == "table" and #emitted[2].value == 0)
            assert(type(emitted[2].semantic_value) == "table" and #emitted[2].semantic_value == 0)
            assert(nefor.json.is_array(emitted[2].value))
            assert(nefor.json.is_array(emitted[2].semantic_value))
            "#,
        )
        .exec()
        .unwrap();
}

#[test]
fn product_split_projects_a_whole_product_in_order() {
    harness()
        .load(
            r#"
            local factory = require("factories.product-split")
            local emitted = {}
            local actor = assert(factory.construct("split", {},
              function(message) emitted[#emitted + 1] = message end))
            local completion = actor.deliver({
              shape = "product",
              whole = true,
              messages = {{
                message = { value = { "left", 42 } },
                arrival = { declared_type = { kind = "product", items = {
                  { kind = "primitive", name = "String" },
                  { kind = "primitive", name = "Int" },
                } } },
              }},
            })
            assert(completion.status == "ok")
            assert(#emitted == 3) -- ready + two ordered projections
            assert(emitted[2].kind == "nefor.node.ProductLeft")
            assert(emitted[2].value == "left")
            assert(emitted[3].kind == "nefor.node.ProductRight")
            assert(emitted[3].value == 42)
            "#,
        )
        .exec()
        .unwrap();
}

#[test]
fn product_join_uses_declared_sender_order() {
    harness()
        .load(
            r#"
            local factory = require("factories.product-join")
            local emitted = {}
            local actor = assert(factory.construct("join", {
              expected_senders = { "left", "right" },
            }, function(message) emitted[#emitted + 1] = message end))
            local completion = actor.deliver({ shape = "product", messages = {
              { from = "right", message = { value = 42 } },
              { from = "left", message = { value = "first" } },
            } })
            assert(completion.status == "ok")
            assert(#emitted == 2)
            assert(emitted[2].kind == "nefor.node.ProductOutput")
            assert(emitted[2].value[1] == "first")
            assert(emitted[2].value[2] == 42)
            "#,
        )
        .exec()
        .unwrap();
}

#[test]
fn discard_turns_successful_delivery_into_kernel_unit() {
    harness()
        .load(
            r#"
            local factory = require("factories.discard")
            local emitted = {}
            local actor = assert(factory.construct("void", {},
              function(message) emitted[#emitted + 1] = message end))
            local completion = actor.deliver({ messages = {{ message = { value = "ignored" } }} })
            assert(completion.status == "ok")
            assert(#emitted == 1 and emitted[1].kind == "mag.ready")
            "#,
        )
        .exec()
        .unwrap();
}
