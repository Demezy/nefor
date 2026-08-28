use mlua::{Lua, Table};
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
    lua
}

#[test]
fn dynamic_input_requires_an_ordered_complete_stream() {
    harness()
        .load(
            r#"
            local factory = require("factories.dynamic-input")
            local emitted = {}
            local actor = assert(factory.construct("input", {},
              function(message) emitted[#emitted + 1] = message end))

            assert(actor.deliver({ messages = {{ message = {
              value = "a", dynamic = { kind = "item", collection = "c", index = 0 }
            }}}}) == nil)
            assert(actor.deliver({ messages = {{ message = {
              value = "b", dynamic = { kind = "item", collection = "c", index = 1 }
            }}}}) == nil)
            local done = actor.deliver({ messages = {{ message = {
              dynamic = { kind = "complete", collection = "c", count = 2 }
            }}}})

            assert(done.status == "ok")
            assert(#emitted == 4) -- ready, two indexed items, completion count
            assert(emitted[2].value.collection == "c" and emitted[2].value.index == 0)
            assert(emitted[2].value.value == "a")
            assert(emitted[3].value.index == 1 and emitted[3].value.value == "b")
            assert(emitted[4].kind == "nefor.dynamic.Complete")
            assert(emitted[4].value.count == 2)

            local bad = assert(factory.construct("bad", {}, function() end))
            local failure = bad.deliver({ messages = {{ message = {
              value = "late", dynamic = { kind = "item", collection = "x", index = 1 }
            }}}})
            assert(failure.status == "failed")
            assert(failure.value.kind == "dynamic_input_noncontiguous_item")
            "#,
        )
        .exec()
        .unwrap();
}

#[test]
fn dynamic_output_restores_source_order_and_supports_empty_collections() {
    harness()
        .load(
            r#"
            local factory = require("factories.dynamic-output")
            local emitted = {}
            local actor = assert(factory.construct("output", {},
              function(message) emitted[#emitted + 1] = message end))

            actor.deliver({ messages = {{ message = { value = {
              collection = "c", index = 1, value = "second"
            }}}}})
            actor.deliver({ messages = {{ message = { value = {
              collection = "c", count = 2
            }}}}})
            local done = actor.deliver({ messages = {{ message = { value = {
              collection = "c", index = 0, value = "first"
            }}}}})

            assert(done.status == "ok")
            assert(#emitted == 4) -- ready, two ordered items, completion
            assert(emitted[2].value == "first" and emitted[2].dynamic.index == 0)
            assert(emitted[3].value == "second" and emitted[3].dynamic.index == 1)
            assert(emitted[4].dynamic.kind == "complete" and emitted[4].dynamic.count == 2)

            local empty_out = {}
            local empty = assert(factory.construct("empty", {},
              function(message) empty_out[#empty_out + 1] = message end))
            local empty_done = empty.deliver({ messages = {{ message = { value = {
              collection = "none", count = 0
            }}}}})
            assert(empty_done.status == "ok")
            assert(#empty_out == 2)
            assert(empty_out[2].dynamic.kind == "complete" and empty_out[2].dynamic.count == 0)
            "#,
        )
        .exec()
        .unwrap();
}

#[test]
fn dynamic_index_preserves_occurrence_identity() {
    harness()
        .load(
            r#"
            local factory = require("factories.dynamic-index")
            assert(factory.construct("bad", { collection = "c", index = -1 }, function() end) == nil)
            local emitted = {}
            local actor = assert(factory.construct("index", { collection = "c", index = 4 },
              function(message) emitted[#emitted + 1] = message end))
            local done = actor.deliver({ messages = {{ message = {
              value = { answer = 42 }, semantic_value = { answer = 42 }
            }}}})
            assert(done.status == "ok")
            assert(emitted[2].value.collection == "c" and emitted[2].value.index == 4)
            assert(emitted[2].value.value.answer == 42)
            assert(emitted[2].semantic_value.value.answer == 42)

            local sum_out = {}
            local sum = assert(factory.construct("sum", { collection = "s", index = 0 },
              function(message) sum_out[#sum_out + 1] = message end))
            sum.deliver({ messages = {{
              message = { value = { answer = 7 } },
              arrival = {
                declared_type = { kind = "union", items = {} },
                constructor_id = "result-tag",
              },
            }} })
            assert(sum_out[2].semantic_value.value.type == "result-tag")
            assert(sum_out[2].semantic_value.value.value.answer == 7)
            "#,
        )
        .exec()
        .unwrap();
}

#[test]
fn strict_routing_enforces_the_whole_dynamic_protocol() {
    harness()
        .load(
            r#"
            nefor = { semantic_type = {
              validate_value = function(descriptor, value)
                return { ok = descriptor.kind == "primitive"
                  and descriptor.name == "String" and type(value) == "string" }
              end,
            }}
            local routing = require("routing")
            local item = { kind = "primitive", name = "String" }
            local dynamic = { kind = "named", name = "nefor.dynamic.DynamicList",
              arguments = { item }, body = { kind = "record", fields = {} } }
            local actor = { state = "alive", semantic_strict = true, outputs = {{
              wire = "Out", type_id = "dynamic-tag", type = dynamic,
            }} }
            local function router()
              return routing.new({
                inventory = { get = function(id) return id == "producer" and actor or nil end },
                registry = {},
              })
            end

            local ordered = router()
            assert(ordered:factory_arrival("producer", "Out", {
              value = "a", dynamic = { kind = "item", collection = "c", index = 0 }
            }))
            assert(ordered:factory_arrival("producer", "Out", {
              value = "b", dynamic = { kind = "item", collection = "c", index = 1 }
            }))
            assert(ordered:factory_arrival("producer", "Out", {
              dynamic = { kind = "complete", collection = "c", count = 2 }
            }))
            local after, after_error = ordered:factory_arrival("producer", "Out", {
              value = "late", dynamic = { kind = "item", collection = "c", index = 2 }
            })
            assert(after == nil and after_error:find("after DynamicList completion"))

            local skipped = router()
            local arrival, error = skipped:factory_arrival("producer", "Out", {
              value = "b", dynamic = { kind = "item", collection = "c", index = 1 }
            })
            assert(arrival == nil and error:find("noncontiguous"))

            local wrong_count = router()
            assert(wrong_count:factory_arrival("producer", "Out", {
              value = "a", dynamic = { kind = "item", collection = "c", index = 0 }
            }))
            local completion, completion_error = wrong_count:factory_arrival("producer", "Out", {
              dynamic = { kind = "complete", collection = "c", count = 2 }
            })
            assert(completion == nil and completion_error:find("invalid DynamicList completion"))

            local malformed = router()
            local bad_value, bad_value_error = malformed:factory_arrival("producer", "Out", {
              value = 4, dynamic = { kind = "item", collection = "c", index = 0 }
            })
            assert(bad_value == nil and bad_value_error:find("malformed DynamicList item"))
            "#,
        )
        .exec()
        .unwrap();
}
