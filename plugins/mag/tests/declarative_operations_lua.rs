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
            if factory == "conditional" then
              return {template={relocations={},parameter_equals={dynamic=false}}}
            end
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
            {path={{constructor="FixedPathSegment",value={value="actor-id"}}},members={{slot="join"}}},
          },
          actor_reference_relocations={{actor={slot="join"},path={"expected_senders"},shape="actor_id_list"}}},
        }
        local initial={actors={{id="source",outputs={{wire="Out",type=trigger_t,type_id="type:Occurrence"}}},
          {id="result",input={actor="result",wire="In",type=string_t,type_id="type:String"},
            outputs={{wire="Out",type=string_t,type_id="type:String"}}}},
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

        local external_target=require("plain-data").copy(operation)
        external_target.template.routes[2]={from=port("join","Out"),to={
          actor={constructor="ExistingActorRef",value={id="result"}},
          type=string_t,type_id="type:String",wire="In"},product_position=-1}
        local external_checked,external_error=operations.preflight(initial,{external_target},registry)
        assert(external_checked,external_error)
        local external_delta=assert(operations.materialize(external_checked[1],{index=2,name="join.2"}))
        assert(external_delta.actors[2].routes.Out[1].actor == "result")

        local wrong_path=require("plain-data").copy(operation)
        wrong_path.template.nodes[1].path[1].constructor="Foreign"
        local _,wrong_path_error=operations.preflight(initial,{wrong_path},registry)
        assert(wrong_path_error:match("unknown constructor"),wrong_path_error)
        local wrong_ref=require("plain-data").copy(operation)
        wrong_ref.template.actors[1].input.actor.constructor="Foreign"
        local _,wrong_ref_error=operations.preflight(initial,{wrong_ref},registry)
        assert(wrong_ref_error:match("unknown actor reference constructor")
          or wrong_ref_error:match("unknown field"),wrong_ref_error)

        local repeated_member=require("plain-data").copy(operation)
        repeated_member.template.nodes[2].members[1].slot="leaf"
        local _,repeated_member_error=operations.preflight(initial,{repeated_member},registry)
        assert(repeated_member_error:match("repeats a logical member"),repeated_member_error)
        local nonstring_path=require("plain-data").copy(operation)
        nonstring_path.template.nodes[1].path[1].value.value="index"
        local _,nonstring_path_error=operations.preflight(initial,{nonstring_path},registry)
        assert(nonstring_path_error:match("must produce String"),nonstring_path_error)
        local repeated_binding=require("plain-data").copy(operation)
        repeated_binding.template.actors[1].parameter_bindings[2]={path={"label"},value="name"}
        local _,repeated_binding_error=operations.preflight(initial,{repeated_binding},registry)
        assert(repeated_binding_error:match("overlap"),repeated_binding_error)
        local absent_binding=require("plain-data").copy(operation)
        absent_binding.template.actors[1].parameter_bindings[1].path={"absent"}
        local _,absent_binding_error=operations.preflight(initial,{absent_binding},registry)
        assert(absent_binding_error:match("path is absent"),absent_binding_error)

        local unsupported=require("plain-data").copy(operation)
        unsupported.template.actors[1].factory="unsupported"
        local _,unsupported_error=operations.preflight(initial,{unsupported},registry)
        assert(unsupported_error:match("template factory"), unsupported_error)
        local unordered=require("plain-data").copy(operation)
        unordered.expressions[2].value.record="later"
        local _,unordered_error=operations.preflight(initial,{unordered},registry)
        assert(unordered_error:match("absent or unordered"))

        local function expression_message_operation(descriptor, descriptor_id, trigger_value)
          local actor_id = "bound." .. descriptor_id
          local input = {actor=local_ref("worker"),type=descriptor,type_id=descriptor_id,wire="Input"}
          local output = {actor=local_ref("worker"),type=string_t,type_id="type:String",wire="Output"}
          local value = {
            id="message-" .. descriptor_id,on_actor="source",on_wire="Out",
            trigger_type=descriptor,trigger_type_id=descriptor_id,
            captures={actor={semantic_type=string_t,semantic_type_id="type:String",value=actor_id}},
            expressions={
              {constructor="Trigger",value={id="trigger",result_type=descriptor_id}},
              {constructor="Capture",value={id="actor-id",result_type="type:String",capture="actor"}},
            },
            template={types={
              [LOCAL]=local_actor_t,[EXISTING]=existing_actor_t,
              [FIXED]=fixed_path_t,[BOUND]=bound_path_t,
            },actors={{slot="worker",id="actor-id",factory="leaf",type_arguments={},params={},
              input=input,outputs={output},parameter_bindings={}}},routes={},
              messages={{to=input,semantic_type=descriptor,semantic_type_id=descriptor_id,
                content={constructor="Expression",value="trigger"}}},
              nodes={{path={{constructor="FixedPathSegment",value={value="worker"}}},members={{slot="worker"}}}},
              actor_reference_relocations={}},
          }
          local source_initial={actors={{id="source",outputs={{wire="Out",type=descriptor,type_id=descriptor_id}}},
            {id="result",outputs={{wire="Out",type=string_t,type_id="type:String"}}}},
            result={from={actor="result",wire="Out"}}}
          local checked_value,checked_error=operations.preflight(source_initial,{value},registry)
          assert(checked_value,checked_error)
          local materialized,materialize_error=operations.materialize(checked_value[1],trigger_value)
          assert(materialized,materialize_error)
          return value,source_initial,materialized
        end

        local record_operation,record_initial,record_delta=expression_message_operation(
          trigger_t,"type:Occurrence",{index=7,name="record"})
        assert(record_delta.messages[1].content.kind == "Input")
        assert(record_delta.messages[1].content.value.name == "record")
        local product_t={kind="product",items={string_t,int_t}}
        local _,_,product_delta=expression_message_operation(product_t,"type:product",{"left",9})
        assert(product_delta.messages[1].content.value[1] == "left")
        assert(product_delta.messages[1].content.value[2] == 9)
        local unit_t={kind="primitive",name="Unit"}
        local _,_,unit_delta=expression_message_operation(unit_t,"type:Unit",nil)
        assert(unit_delta.messages[1].content.kind == "Input")
        assert(unit_delta.messages[1].content.value == nil)

        local static=require("plain-data").copy(record_operation)
        static.template.messages[1].content={constructor="Static",value={kind="Input",value={index=3,name="static"}}}
        local static_checked,static_error=operations.preflight(record_initial,{static},registry)
        assert(static_checked,static_error)
        local static_delta=assert(operations.materialize(static_checked[1],{index=8,name="ignored"}))
        assert(static_delta.messages[1].content.value.name == "static")

        local forged=require("plain-data").copy(record_operation)
        forged.template.messages[1].content.value="missing"
        local _,forged_error=operations.preflight(record_initial,{forged},registry)
        assert(forged_error:match("unknown expression"),forged_error)
        local wrong_evidence=require("plain-data").copy(record_operation)
        wrong_evidence.template.messages[1].semantic_type=string_t
        wrong_evidence.template.messages[1].semantic_type_id="type:String"
        local _,wrong_evidence_error=operations.preflight(record_initial,{wrong_evidence},registry)
        assert(wrong_evidence_error:match("exactly match"),wrong_evidence_error)
        local external_route=require("plain-data").copy(operation)
        external_route.template.routes[1].to.actor={constructor="ExistingActorRef",value={id="outside"}}
        local _,external_route_error=operations.preflight(initial,{external_route},registry)
        assert(external_route_error:match("declared initial actor input"),external_route_error)
        local product_route=require("plain-data").copy(operation)
        product_route.template.routes[1].product_position=0
        local _,product_route_error=operations.preflight(initial,{product_route},registry)
        assert(product_route_error:match("product position"),product_route_error)
        local relocation_mismatch=require("plain-data").copy(operation)
        relocation_mismatch.template.actors[2].params.expected_senders={"outside"}
        local _,relocation_error=operations.preflight(initial,{relocation_mismatch},registry)
        assert(relocation_error:match("unknown local actor slot"),relocation_error)
        local streaming=require("plain-data").copy(record_operation)
        streaming.template.actors[1].factory="conditional"
        streaming.template.actors[1].params.dynamic=true
        local _,streaming_error=operations.preflight(record_initial,{streaming},registry)
        assert(streaming_error:match("requires params.dynamic = false"),streaming_error)
        "#,
    )
    .exec()
    .unwrap();
}
