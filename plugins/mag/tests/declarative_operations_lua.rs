use mlua::{Lua, Table};
use std::path::PathBuf;

#[test]
fn declarative_templates_validate_relocate_and_materialize_distinct_firings() {
    let lua = Lua::new();
    let package: Table = lua.globals().get("package").unwrap();
    let current: String = package.get("path").unwrap();
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let kernel = manifest.join("lua/mag-kernel");
    let shared = manifest.join("../../lua");
    package
        .set(
            "path",
            format!(
                "{0}/?.lua;{0}/?/init.lua;{1}/?.lua;{1}/?/init.lua;{current}",
                kernel.display(),
                shared.display()
            ),
        )
        .unwrap();
    lua.load(
        r#"
        local function stable_id(value)
          if value.kind == "primitive" then return "type:" .. value.name end
          return "type:" .. tostring(value.name or value.kind)
        end
        nefor = {
          json = {
            encode = function(value)
              if value.path then return table.concat(value.path, "/") .. ":" .. value.shape end
              if value.from then
                return value.from.actor .. "/" .. value.from.wire .. "->"
                  .. value.to.actor .. "/" .. value.to.wire
              end
              return "value"
            end,
          },
          semantic_type = {
            id = stable_id,
            accepts = function(left, right) return stable_id(left) == stable_id(right) end,
            validate_declarations = function() return true end,
            validate_value = function() return { ok = true } end,
          },
        }
        local operations = require("operations")
        local string_t = {kind="primitive",name="String"}
        local int_t = {kind="primitive",name="Int"}
        local trigger_t = {kind="record",name="Occurrence",fields={
          {name="index",type=int_t},{name="name",type=string_t},
        }}
        local local_actor_t = {kind="named",name="nefor.mag.LocalActorRef"}
        local existing_actor_t = {kind="named",name="nefor.mag.ExistingActorRef"}
        local fixed_path_t = {kind="named",name="nefor.mag.FixedPathSegment"}
        local bound_path_t = {kind="named",name="nefor.mag.BoundPathSegment"}
        local LOCAL = "type:LocalActorRef"
        local EXISTING = "type:ExistingActorRef"
        local FIXED = "type:FixedPathSegment"
        local BOUND = "type:BoundPathSegment"
        local function local_ref(slot) return {constructor="LocalActorRef",value={slot=slot}} end
        local function port(slot, wire)
          return {actor=local_ref(slot),type=string_t,type_id="type:String",wire=wire}
        end
        local registry = {
          declaration = function(_, factory)
            if factory == "join" then
              return {template={relocations={{path={"expected_senders"},shape="actor_id_list"}}}}
            end
            if factory == "leaf" then return {template={relocations={}}} end
          end,
        }
        local operation = {
          id="expand",on_actor="source",on_wire="Out",
          trigger_type=trigger_t,trigger_type_id="type:Occurrence",
          captures={prefix={semantic_type=string_t,semantic_type_id="type:String",value="worker."}},
          expressions={
            {constructor="Trigger",value={id="trigger",result_type="type:Occurrence"}},
            {constructor="Field",value={id="index",result_type="type:Int",record="trigger",field="index"}},
            {constructor="IntToDecimalString",value={id="index-text",result_type="type:String",value="index"}},
            {constructor="Field",value={id="name",result_type="type:String",record="trigger",field="name"}},
            {constructor="Capture",value={id="prefix-ref",result_type="type:String",capture="prefix"}},
            {constructor="ConcatStrings",value={id="actor-id",result_type="type:String",values={"prefix-ref","index-text"}}},
          },
          template={types={
            [LOCAL]=local_actor_t,[EXISTING]=existing_actor_t,
            [FIXED]=fixed_path_t,[BOUND]=bound_path_t,
          },actors={
            {slot="leaf",id="actor-id",factory="leaf",type_arguments={},params={label=""},
             input=port("leaf","In"),outputs={port("leaf","Out")},
             parameter_bindings={{path={"label"},value="name"}}},
            {slot="join",id="name",factory="join",type_arguments={},params={expected_senders={"leaf"}},
             input=port("join","In"),outputs={port("join","Out")},parameter_bindings={}},
          },routes={{from=port("leaf","Out"),to=port("join","In"),product_position=-1}},
          messages={},nodes={
            {path={{constructor="BoundPathSegment",value={value="actor-id"}}},members={{slot="leaf"}}},
            {path={{constructor="FixedPathSegment",value={value="actor-id"}}},members={}},
          },
          actor_reference_relocations={{actor={slot="join"},path={"expected_senders"},shape="actor_id_list"}}},
        }
        local initial={actors={{id="source",outputs={{wire="Out",type=trigger_t,type_id="type:Occurrence"}}},
          {id="result",outputs={{wire="Out",type=string_t,type_id="type:String"}}}},
          result={from={actor="result",wire="Out"}}}
        local checked,err=operations.preflight(initial,{operation},registry)
        assert(checked,err)
        local first=assert(operations.materialize(checked[1],{index=0,name="join.0"}))
        local second=assert(operations.materialize(checked[1],{index=1,name="join.1"}))
        assert(first.actors[1].id == "worker.0" and second.actors[1].id == "worker.1")
        assert(first.actors[1].params.label == "join.0")
        assert(first.actors[2].params.expected_senders[1] == "worker.0")
        local edge0=first.actors[1].routes.Out[1].edge_id
        local edge1=second.actors[1].routes.Out[1].edge_id
        assert(edge0 ~= edge1)
        assert(first.nodes[1].path[1] == "worker.0")
        assert(first.nodes[2].path[1] == "actor-id")

        local wrong_path=require("plain-data").copy(operation)
        wrong_path.template.nodes[1].path[1].constructor="Foreign"
        local _,wrong_path_error=operations.preflight(initial,{wrong_path},registry)
        assert(wrong_path_error:match("unknown constructor"),wrong_path_error)
        local wrong_ref=require("plain-data").copy(operation)
        wrong_ref.template.actors[1].input.actor.constructor="Foreign"
        local _,wrong_ref_error=operations.preflight(initial,{wrong_ref},registry)
        assert(wrong_ref_error:match("unknown actor reference constructor")
          or wrong_ref_error:match("unknown field"),wrong_ref_error)

        local unsupported=require("plain-data").copy(operation)
        unsupported.template.actors[1].factory="unsupported"
        local _,unsupported_error=operations.preflight(initial,{unsupported},registry)
        assert(unsupported_error:match("template factory"), unsupported_error)
        local unordered=require("plain-data").copy(operation)
        unordered.expressions[2].value.record="later"
        local _,unordered_error=operations.preflight(initial,{unordered},registry)
        assert(unordered_error:match("absent or unordered"))
        "#,
    )
    .exec()
    .unwrap();
}
