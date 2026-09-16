use mlua::{Lua, Table};
use std::path::PathBuf;

fn run(script: &str) {
    let lua = Lua::new();
    let package: Table = lua.globals().get("package").unwrap();
    let current: String = package.get("path").unwrap();
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.join("lua/mag-kernel");
    let shared_lua = manifest.join("../../lua");
    package
        .set(
            "path",
            format!(
                "{0}/?.lua;{0}/?/init.lua;{1}/?.lua;{1}/?/init.lua;{current}",
                root.display(),
                shared_lua.display()
            ),
        )
        .unwrap();
    lua.load(
        r#"
        events = {}
        nefor = {
          log = function() end,
          now_ms = function() return 0 end,
          emit = function(event) events[#events + 1] = event end,
          opaque_id = (function()
            local sequence = 0
            return function()
              sequence = sequence + 1
              return "opaque-test-id-" .. sequence
            end
          end)(),
          semantic_type = {
            id = function(value) return value.name end,
            accepts = function(left, right) return left.name == right.name end,
            validate_declarations = function() return true end,
            validate_value = function() return { ok = true } end,
            input_covered_by = function() return true end,
          },
          json = {
            encode = function(value)
              if value.path then return table.concat(value.path, "/") .. ":" .. value.shape end
              return value.from.actor .. "/" .. value.from.wire .. "->"
                .. value.to.actor .. "/" .. value.to.wire
            end,
          },
        }
        "#,
    )
    .exec()
    .unwrap();
    let kernel_path = root.join("init.lua");
    let kernel_source = std::fs::read_to_string(kernel_path).unwrap();
    let kernel: Table = lua.load(&kernel_source).eval().unwrap();
    lua.globals().set("kernel", kernel).unwrap();
    lua.load(script).exec().unwrap();
}

#[test]
fn terminal_settlement_is_first_write_wins_even_after_host_take() {
    run(r#"
        assert(kernel.begin_run({run_id="race", run_name="race", session_id="s"}).ok)
        assert(kernel.start("race", {
          actors={{id="result", factory="nefor.factory.stub", type_arguments={}, params={greeting="first"}, routes={}}},
          messages={{to="result", content={kind="stub.In"}}}, kills={},
          result={from={actor="result", wire="stub.Out"}}
        }).ok)
        local emit = kernel.context("race").router:emitter("result")
        emit({kind="stub.Out", greeting="second"})
        local first = assert(kernel.take_run_complete("race"))
        assert(first.result.greeting == "first")
        emit({kind="stub.Out", greeting="third"})
        assert(kernel.take_run_complete("race") == nil)
        local ignored, completed = 0, 0
        for _, event in ipairs(events) do
          if event.kind == "mag.terminal_settlement_ignored" then ignored = ignored + 1 end
          if event.kind == "mag.run_complete" then completed = completed + 1 end
        end
        assert(ignored == 2)
        assert(completed == 1)
        assert(kernel.context("race").terminal_settlement.completion.result.greeting == "first")
        "#);
}

#[test]
fn synchronous_initial_output_drains_declarative_operations_before_start_returns() {
    run(r#"
        local S={kind="primitive",name="String"}
        local L={kind="named",name="nefor.mag.LocalActorRef"}
        local E={kind="named",name="nefor.mag.ExistingActorRef"}
        local F={kind="named",name="nefor.mag.FixedPathSegment"}
        local B={kind="named",name="nefor.mag.BoundPathSegment"}
        local LOCAL="type:LocalActorRef"
        local EXISTING="type:ExistingActorRef"
        local FIXED="type:FixedPathSegment"
        local BOUND="type:BoundPathSegment"
        local template_types={String=S,[LOCAL]=L,[EXISTING]=E,[FIXED]=F,[BOUND]=B}
        local function local_ref(slot) return {constructor="LocalActorRef",value={slot=slot}} end
        local function existing_ref(id) return {constructor="ExistingActorRef",value={id=id}} end
        local function bound(value) return {constructor="BoundPathSegment",value={value=value}} end
        local function port(actor,wire) return {actor=actor,wire=wire,type=S,type_id="String"} end
        local operation={
          id="spawn",on_actor="source",on_wire="stub.Out",trigger_type=S,trigger_type_id="String",
          captures={},expressions={{constructor="Trigger",value={id="trigger",result_type="String"}}},
          template={types=template_types,actors={{slot="spawned",id="trigger",factory="nefor.factory.stub",
            type_arguments={},params={value="child"},input={actor=local_ref("spawned"),type=S,type_id="String",wire="stub.In"},
            outputs={{actor=local_ref("spawned"),type=S,type_id="String",wire="stub.Out"}},parameter_bindings={}}},
            routes={},messages={{to={actor=local_ref("spawned"),type=S,type_id="String",wire="stub.In"},
              semantic_type=S,semantic_type_id="String",content={constructor="Static",
                value={kind="stub.In",value="go"}}}},
            nodes={{path={bound("trigger")},members={{slot="spawned"}}}},actor_reference_relocations={}}
        }
        local second=require("plain-data").copy(operation)
        second.id="spawn-again"
        second.captures.suffix={semantic_type=S,semantic_type_id="String",value="-again"}
        second.expressions[2]={constructor="Capture",value={id="suffix",result_type="String",capture="suffix"}}
        second.expressions[3]={constructor="ConcatStrings",value={id="second-id",result_type="String",values={"trigger","suffix"}}}
        second.template.actors[1].id="second-id"
        second.template.nodes[1].path={bound("second-id")}
        assert(kernel.begin_run({run_id="sync-op",run_name="sync-op",session_id="s"}).ok)
        local outcome=kernel.start("sync-op",{
          types={String=S},actors={
            {id="source",factory="nefor.factory.stub",type_arguments={},params={value="spawned"},
             input=port("source","stub.In"),outputs={port("source","stub.Out")},routes={}},
            {id="result",factory="nefor.factory.stub",type_arguments={},params={},
             input=port("result","stub.In"),outputs={port("result","stub.Out")},routes={}}
          },messages={{to="source",semantic_type=S,semantic_type_id="String",content={kind="stub.In",value="go"}}},
          kills={},nodes={{path={"source"},members={"source"}},{path={"result"},members={"result"}}},
          result={from=port("result","stub.Out")}
        },{operation,second})
        assert(outcome.ok,outcome.error)
        local ctx=kernel.context("sync-op")
        assert(ctx.inventory.state_of("spawned")=="alive")
        assert(ctx.inventory.get("spawned").semantic_strict==true)
        assert(ctx.router.type_declarations[LOCAL]~=nil)
        assert(#ctx.operations==2 and #ctx.operation_queue==0)
        assert(ctx.inventory.state_of("spawned-again")=="alive")

        kernel.end_run("sync-op")
        local failing=require("plain-data").copy(operation)
        failing.template.routes={{
          from={actor=local_ref("spawned"),type=S,type_id="String",wire="stub.Out"},
          to={actor=existing_ref("missing"),type=S,type_id="String",wire="stub.In"},
          product_position=-1,
        }}
        assert(kernel.begin_run({run_id="failure-wins",run_name="failure-wins",session_id="s"}).ok)
        local failed_start=kernel.start("failure-wins",{
          types={String=S},actors={
            {id="result",factory="nefor.factory.stub",type_arguments={},params={value="success"},
             input=port("result","stub.In"),outputs={port("result","stub.Out")},routes={}},
            {id="source",factory="nefor.factory.stub",type_arguments={},params={value="spawned"},
             input=port("source","stub.In"),outputs={port("source","stub.Out")},routes={}}
          },messages={
            {to="result",semantic_type=S,semantic_type_id="String",content={kind="stub.In",value="go"}},
            {to="source",semantic_type=S,semantic_type_id="String",content={kind="stub.In",value="go"}}
          },kills={},nodes={{path={"result"},members={"result"}},{path={"source"},members={"source"}}},
          result={from=port("result","stub.Out")}
        },{failing})
        assert(not failed_start.ok)
        assert(failed_start.error:match("declared initial actor input"),failed_start.error)
        assert(kernel.take_run_complete("failure-wins")==nil,"rejected operation produced a result")
        "#);
}

#[test]
fn rejected_typed_delta_does_not_leak_declarations_or_mutate_input() {
    run(r#"
        local S={kind="primitive",name="String"}
        local function port(actor,wire)
          return {actor=actor,wire=wire,type=S,type_id="String"}
        end
        assert(kernel.begin_run({run_id="typed-atomic",run_name="typed-atomic",session_id="s"}).ok)
        assert(kernel.start("typed-atomic",{
          types={String=S},actors={
            {id="result",factory="nefor.factory.stub",type_arguments={},params={},
             input=port("result","stub.In"),outputs={port("result","stub.Out")},routes={}}
          },messages={},kills={},nodes={{path={"result"},members={"result"}}},
          result={from=port("result","stub.Out")}
        }).ok)
        local rejected={
          types={Rejected={kind="primitive",name="Rejected"}},actors={
            {id="candidate",factory="nefor.factory.stub",type_arguments={},params={},
             input=port("candidate","stub.In"),outputs={port("candidate","stub.Out")},routes={}}
          },messages={{to="missing",semantic_type=S,semantic_type_id="String",
            content={kind="stub.In",value="no"}}},kills={},
          nodes={{path={"candidate"},members={"candidate"}}}
        }
        local outcome=kernel.apply("typed-atomic",rejected)
        assert(not outcome.ok)
        local ctx=kernel.context("typed-atomic")
        assert(ctx.router.type_declarations.Rejected==nil)
        assert(rejected.actors[1].semantic_strict==nil)
        assert(ctx.inventory.state_of("candidate")=="never-existed")
        local accepted=kernel.apply("typed-atomic",{
          types={Accepted={kind="primitive",name="Accepted"}},
          actors={},messages={},kills={},nodes={}
        })
        assert(accepted.ok,accepted.error)
        assert(ctx.router.type_declarations.Accepted.name=="Accepted")
        assert(ctx.router.type_declarations.Rejected==nil)
        "#);
}

#[test]
fn killed_generation_cannot_route_or_settle() {
    run(r#"
        assert(kernel.begin_run({run_id="stale", run_name="stale", session_id="s"}).ok)
        assert(kernel.start("stale", {
          actors={
            {id="source", factory="nefor.factory.stub", type_arguments={}, params={value="first"},
             routes={["stub.Out"]={{actor="downstream", wire="stub.In"}}}},
            {id="downstream", factory="nefor.factory.stub", type_arguments={}, params={}, routes={}},
            {id="result", factory="nefor.factory.stub", type_arguments={}, params={}, routes={}}
          },
          messages={{to="source", content={kind="stub.In"}}}, kills={},
          result={from={actor="result", wire="stub.Out"}}
        }).ok)
        local ctx = kernel.context("stale")
        local stale_emit = ctx.router:emitter("source")
        assert(kernel.apply("stale", {types={},actors={},messages={},nodes={},kills={"source"}}).ok)
        stale_emit({kind="stub.Out", value="late"})
        assert(kernel.take_run_complete("stale") == nil)
        local ignored = 0
        for _, event in ipairs(events) do
          if event.kind == "mag.emission_ignored" and event.from == "source" then
            ignored = ignored + 1
            assert(event.reason == "actor_dead")
          end
        end
        assert(ignored == 1)
        "#);
}
