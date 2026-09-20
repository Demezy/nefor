use mlua::{Lua, LuaSerdeExt, Table};
use std::path::PathBuf;

fn lua() -> Lua {
    let lua = Lua::new();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let package: Table = lua.globals().get("package").unwrap();
    package
        .set(
            "path",
            format!(
                "{0}/lua/mag-kernel/?.lua;{0}/../../lua/?/init.lua",
                root.display()
            ),
        )
        .unwrap();
    let json = lua.create_table().unwrap();
    json.set(
        "encode",
        lua.create_function(|lua, value: mlua::Value| {
            let value: serde_json::Value = lua.from_value(value)?;
            serde_json::to_string(&value).map_err(mlua::Error::external)
        })
        .unwrap(),
    )
    .unwrap();
    let nefor = lua.create_table().unwrap();
    nefor.set("json", json).unwrap();
    lua.globals().set("nefor", nefor).unwrap();
    lua
}

#[test]
fn every_utf8_prefix_boundary_preserves_complete_characters() {
    lua()
        .load(
            r#"
        local context = require("model-context")
        for _, text in ipairs({"abcdef", "абвг", "漢字仮名", "😀🚀🌍", "aб漢😀z"}) do
          for budget = 0, #text + 1 do
            local expected = ""
            for start, code in utf8.codes(text) do
              local finish = start + #utf8.char(code) - 1
              if finish <= budget then expected = text:sub(1, finish) end
            end
            local head = context._utf8_head(text, budget)
            assert(utf8.len(head), "invalid UTF-8 at budget " .. budget)
            assert(head == expected, "wrong prefix at budget " .. budget)
          end
        end
    "#,
        )
        .exec()
        .unwrap();
}

#[test]
fn adapter_truncates_unicode_results_into_valid_json_with_exact_omission_ranges() {
    lua().load(r#"
        local context = require("model-context")
        local adapter = require("factories.adapter")
        for _, character in ipairs({"б", "漢", "😀"}) do
          local text = character:rep(context.ITEM_LIMIT)
          local rendered = context._render(text, context.ITEM_LIMIT, "/saved/output")
          assert(utf8.len(rendered), "truncated result is invalid UTF-8")
          assert(#rendered <= context.ITEM_LIMIT)
          local first, last, original, omitted, start, finish = rendered:find(
            "\n\n%[output truncated: original (%d+) bytes; omitted (%d+) bytes at zero%-based half%-open range %[(%d+), (%d+)%). .-%]\n\n")
          assert(first, "missing truncation marker")
          original, omitted, start, finish = tonumber(original), tonumber(omitted), tonumber(start), tonumber(finish)
          assert(original == #text and omitted == finish - start)
          assert(rendered:sub(1, first - 1) == text:sub(1, start))
          assert(rendered:sub(last + 1) == text:sub(finish + 1))
          assert(start + omitted + #rendered:sub(last + 1) == #text)
          nefor.json.encode(rendered)
          local emitted = {}
          local instance = adapter.construct("entry", {}, function(value)
            nefor.json.encode(value)
            emitted[#emitted + 1] = value
          end)
          instance.deliver({messages={{message={value=text, output_path="/saved/output"}}}})
          local output = emitted[#emitted]
          assert(output.kind == "generic-provider.ProviderOut")
          assert(type(output.value.content) == "string")
          assert(output.value.content:find("output truncated", 1, true))
          assert(utf8.len(output.value.content))
          assert(#output.value.content <= context.ITEM_LIMIT)
        end
    "#).exec().unwrap();
}
