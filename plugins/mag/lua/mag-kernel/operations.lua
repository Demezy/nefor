-- Declarative runtime operations. This module owns validation, expression
-- evaluation, template materialization, and canonical route identity. The
-- caller owns only the serial run queue and the atomic inventory fold.
local plain_data = require("plain-data")

local M = {}

local function dense_list(value)
  if type(value) ~= "table" then return false end
  local count = 0
  for key in pairs(value) do
    if type(key) ~= "number" or key < 1 or key % 1 ~= 0 then return false end
    count = count + 1
  end
  for index = 1, count do if value[index] == nil then return false end end
  return true
end

local function exact_fields(value, allowed, label)
  if type(value) ~= "table" then return nil, label .. " must be an object" end
  for key in pairs(value) do
    if allowed[key] == nil then return nil, label .. " has unknown field " .. tostring(key) end
  end
  for key, required in pairs(allowed) do
    if required == true and value[key] == nil then return nil, label .. " requires " .. key end
  end
  return true
end

local function nonempty(value) return type(value) == "string" and value ~= "" end

local function type_id(descriptor)
  local host = nefor and nefor.semantic_type
  if type(descriptor) ~= "table" or type(host) ~= "table" or type(host.id) ~= "function" then
    return nil
  end
  local ok, result = pcall(host.id, descriptor)
  return ok and result or nil
end

local function validate_typed(descriptor, id, label)
  if not nonempty(id) or type_id(descriptor) ~= id then
    return nil, label .. " semantic descriptor identity is absent or mismatched"
  end
  return true
end

local function descriptor_field(descriptor, name)
  local current = descriptor
  if type(current) == "table" and current.kind == "named" then current = current.body end
  if type(current) ~= "table" or current.kind ~= "record" or not dense_list(current.fields) then
    return nil
  end
  for _, field in ipairs(current.fields) do
    if type(field) == "table" and field.name == name then return field.type end
  end
  return nil
end

local CONSTRUCTORS = {
  local_actor = "LocalActorRef",
  existing_actor = "ExistingActorRef",
  fixed_path = "FixedPathSegment",
  bound_path = "BoundPathSegment",
  trigger_path = "TriggerPathSegment",
}

local function constructor_ids()
  return CONSTRUCTORS
end

local function actor_ref(ref, constructors)
  if type(ref) ~= "table" or type(ref.constructor) ~= "string"
      or type(ref.value) ~= "table" then return nil end
  if ref.constructor == constructors.local_actor then return ref.value, "local" end
  if ref.constructor == constructors.existing_actor then return ref.value, "existing" end
  return nil
end

local function validate_ref(ref, slots, constructors, label)
  local ok, err = exact_fields(ref, {constructor=true,value=true}, label)
  if not ok then return nil, err end
  local value, kind = actor_ref(ref, constructors)
  if not value then return nil, label .. " has an unknown actor reference constructor" end
  ok, err = exact_fields(value, kind == "local" and {slot=true} or {id=true}, label .. ".value")
  if not ok then return nil, err end
  if kind == "local" and (not nonempty(value.slot) or not slots[value.slot]) then
    return nil, label .. " names an unknown local actor slot"
  end
  if kind == "existing" and not nonempty(value.id) then return nil, label .. " existing id must be non-empty" end
  return true
end

local function local_slot(ref, constructors)
  local value, kind = actor_ref(ref, constructors)
  return kind == "local" and value.slot or nil
end

local function validate_port(port, slots, constructors, label)
  local ok, err = exact_fields(port,
    { actor = true, type = true, type_id = true, wire = true }, label)
  if not ok then return nil, err end
  ok, err = validate_ref(port.actor, slots, constructors, label .. ".actor")
  if not ok then return nil, err end
  if not nonempty(port.wire) then return nil, label .. ".wire must be non-empty" end
  return validate_typed(port.type, port.type_id, label)
end

local function validate_path(path, label)
  if not dense_list(path) or #path == 0 then return nil, label .. " must be a non-empty string list" end
  for index, part in ipairs(path) do
    if not nonempty(part) then return nil, string.format("%s[%d] must be non-empty", label, index) end
  end
  return true
end

local function paths_overlap(left, right)
  for index = 1, math.min(#left, #right) do
    if left[index] ~= right[index] then return false end
  end
  return true
end

local function get_path(root, path)
  local current = root
  for _, part in ipairs(path) do
    if type(current) ~= "table" then return nil, false end
    current = current[part]
    if current == nil then return nil, false end
  end
  return current, true
end

local function same_port(left, right, constructors)
  local left_ref, left_kind = actor_ref(left.actor, constructors)
  local right_ref, right_kind = actor_ref(right.actor, constructors)
  if left_kind ~= "local" or right_kind ~= "local"
      or left_ref.slot ~= right_ref.slot then return false end
  return left.wire == right.wire and left.type_id == right.type_id
      and type_id(left.type) == type_id(right.type)
end

local function contract_relocations(registry, factory)
  local declaration = registry:declaration(factory)
  if not declaration then return nil, "unknown template factory " .. tostring(factory) end
  local template = declaration.template
  if type(template) ~= "table" or not dense_list(template.relocations) then
    return nil, "template factory " .. tostring(factory) .. " does not declare closed relocation metadata"
  end
  local allowed = {}
  for index, relocation in ipairs(template.relocations) do
    local ok, err = exact_fields(relocation, { path = true, shape = true },
      string.format("factory %s relocation %d", factory, index))
    if not ok then return nil, err end
    ok, err = validate_path(relocation.path, "factory relocation path")
    if not ok then return nil, err end
    if relocation.shape ~= "actor_id" and relocation.shape ~= "actor_id_list" then
      return nil, "factory relocation has unsupported shape " .. tostring(relocation.shape)
    end
    allowed[nefor.json.encode({ path = relocation.path, shape = relocation.shape })] = true
  end
  return allowed, nil, template.parameter_equals or {}
end

function M.preflight(initial, operations, registry)
  if not dense_list(operations) then return nil, "program.operations must be a dense list" end
  local initial_actors = {}
  for _, actor in ipairs((initial and initial.actors) or {}) do
    if type(actor) == "table" and nonempty(actor.id) then initial_actors[actor.id] = actor end
  end
  local ids = {}
  local owned = {}
  for op_index, operation in ipairs(operations) do
    local label = string.format("program.operations[%d]", op_index)
    local ok, err = exact_fields(operation, {
      id=true, on_actor=true, on_wire=true, trigger_type=true, trigger_type_id=true,
      captures=true, expressions=true, template=true,
    }, label)
    if not ok then return nil, err end
    if not nonempty(operation.id) then return nil, label .. ".id must be non-empty" end
    if ids[operation.id] then return nil, "duplicate operation id " .. operation.id end
    ids[operation.id] = true
    local source = initial_actors[operation.on_actor]
    if not source or not nonempty(operation.on_wire) then return nil, label .. " trigger is not an initial actor output" end
    local endpoint
    for _, output in ipairs(source.outputs or {}) do
      if output.wire == operation.on_wire then
        if endpoint then return nil, label .. " trigger output address is ambiguous" end
        endpoint = output
      end
    end
    if not endpoint or endpoint.type_id ~= operation.trigger_type_id then
      return nil, label .. " trigger does not match its declared initial output"
    end
    local boundary = initial and initial.result and initial.result.from
    if type(boundary) == "table" and boundary.actor == operation.on_actor
        and boundary.wire == operation.on_wire then
      return nil, label .. " may not bind the result boundary"
    end
    ok, err = validate_typed(operation.trigger_type, operation.trigger_type_id, label .. ".trigger")
    if not ok then return nil, err end
    if type(operation.captures) ~= "table" then return nil, label .. ".captures must be an object" end
    for capture_id, capture in pairs(operation.captures) do
      if not nonempty(capture_id) or type(capture) ~= "table" then return nil, label .. " has malformed capture" end
      ok, err = exact_fields(capture,
        { semantic_type=true, semantic_type_id=true, value=true }, label .. ".captures." .. capture_id)
      if not ok then return nil, err end
      ok, err = validate_typed(capture.semantic_type, capture.semantic_type_id,
        label .. ".captures." .. capture_id)
      if not ok then return nil, err end
      local host = nefor and nefor.semantic_type
      if type(host) ~= "table" or type(host.validate_value) ~= "function" then
        return nil, label .. " capture values cannot be verified"
      end
      local validation = host.validate_value(capture.semantic_type, capture.value)
      if not validation.ok then return nil, label .. ".captures." .. capture_id .. " value is malformed" end
    end
    if not dense_list(operation.expressions) or #operation.expressions == 0 then
      return nil, label .. ".expressions must be a non-empty dense list"
    end
    local expressions, descriptors = {}, {}
    for expr_index, envelope in ipairs(operation.expressions) do
      local expr_label = string.format("%s.expressions[%d]", label, expr_index)
      local envelope_ok, envelope_error = exact_fields(
        envelope, { constructor=true, value=true }, expr_label)
      if not envelope_ok then return nil, envelope_error end
      local kinds_by_constructor = {
        Trigger="trigger", Capture="capture", Field="field",
        IntToDecimalString="int_to_decimal", ConcatStrings="concat",
      }
      local kind = kinds_by_constructor[envelope.constructor]
      local expression = envelope.value
      if not kind or type(expression) ~= "table" or not nonempty(expression.id)
          or not nonempty(expression.result_type) or expressions[expression.id] then
        return nil, expr_label .. " has an invalid constructor or duplicate identity"
      end
      local allowed = kind == "trigger" and {id=true,result_type=true}
        or kind == "capture" and {id=true,result_type=true,capture=true}
        or kind == "field" and {id=true,result_type=true,record=true,field=true}
        or kind == "int_to_decimal" and {id=true,result_type=true,value=true}
        or {id=true,result_type=true,values=true}
      ok, err = exact_fields(expression, allowed, expr_label)
      if not ok then return nil, err end
      local descriptor
      if kind == "trigger" then
        if expression.id ~= "trigger" then return nil, expr_label .. " trigger id must be trigger" end
        descriptor = operation.trigger_type
      elseif kind == "capture" then
        local capture = operation.captures[expression.capture]
        if not capture then return nil, expr_label .. " references an unknown capture" end
        descriptor = capture.semantic_type
      elseif kind == "field" then
        if not expressions[expression.record] or not nonempty(expression.field) then
          return nil, expr_label .. " field dependency is absent or unordered"
        end
        descriptor = descriptor_field(descriptors[expression.record], expression.field)
        if not descriptor then return nil, expr_label .. " field is absent from the record type" end
      elseif kind == "int_to_decimal" then
        if not expressions[expression.value] then return nil, expr_label .. " dependency is absent or unordered" end
        if type_id(descriptors[expression.value]) ~= type_id({kind="primitive",name="Int"}) then
          return nil, expr_label .. " dependency must be Int"
        end
        descriptor = { kind = "primitive", name = "String" }
      else
        if not dense_list(expression.values) then return nil, expr_label .. ".values must be a dense list" end
        for _, dependency in ipairs(expression.values) do
          if not expressions[dependency] then return nil, expr_label .. " dependency is absent or unordered" end
          if type_id(descriptors[dependency]) ~= type_id({kind="primitive",name="String"}) then
            return nil, expr_label .. " dependencies must be String"
          end
        end
        descriptor = { kind = "primitive", name = "String" }
      end
      if type_id(descriptor) ~= expression.result_type then
        return nil, expr_label .. " result semantic type is incorrect"
      end
      expressions[expression.id], descriptors[expression.id] = kind, descriptor
    end

    local template = operation.template
    ok, err = exact_fields(template, {
      types=true, actors=true, routes=true, messages=true, nodes=true,
      actor_reference_relocations=true,
    }, label .. ".template")
    if not ok then return nil, err end
    for _, field in ipairs({"actors", "routes", "messages", "nodes", "actor_reference_relocations"}) do
      if not dense_list(template[field]) then return nil, label .. ".template." .. field .. " must be a dense list" end
    end
    if type(template.types) ~= "table" then return nil, label .. ".template.types must be an object" end
    local semantic_host = nefor and nefor.semantic_type
    if type(semantic_host) ~= "table" or type(semantic_host.validate_declarations) ~= "function" then
      return nil, label .. ".template types cannot be verified"
    end
    local declarations_ok, declarations_result = pcall(semantic_host.validate_declarations, template.types)
    if not declarations_ok or declarations_result ~= true then return nil, label .. ".template.types are invalid" end
    local constructors
    constructors, err = constructor_ids(template.types, label .. ".template.types")
    if not constructors then return nil, err end
    local slots, actor_by_slot, relocations_by_slot = {}, {}, {}
    for actor_index, actor in ipairs(template.actors) do
      local actor_label = string.format("%s.template.actors[%d]", label, actor_index)
      ok, err = exact_fields(actor, {slot=true,id=true,factory=true,type_arguments=true,params=true,
        input=true,outputs=true,parameter_bindings=true}, actor_label)
      if not ok then return nil, err end
      if not nonempty(actor.slot) or slots[actor.slot] then return nil, actor_label .. " slot is invalid or duplicate" end
      if not expressions[actor.id] then return nil, actor_label .. " id expression is absent" end
      if type_id(descriptors[actor.id]) ~= type_id({kind="primitive",name="String"}) then
        return nil, actor_label .. " id expression must produce String"
      end
      if type(actor.params) ~= "table" or not dense_list(actor.type_arguments)
          or not dense_list(actor.outputs) or not dense_list(actor.parameter_bindings) then
        return nil, actor_label .. " contains a non-closed collection"
      end
      local allowed_relocations, required_parameters
      allowed_relocations, err, required_parameters = contract_relocations(registry, actor.factory)
      if not allowed_relocations then return nil, actor_label .. ": " .. err end
      for parameter, expected in pairs(required_parameters) do
        if actor.params[parameter] ~= expected then
          return nil, actor_label .. " factory template contract requires params."
            .. parameter .. " = " .. tostring(expected)
        end
      end
      slots[actor.slot], actor_by_slot[actor.slot] = true, actor
      relocations_by_slot[actor.slot] = { allowed = allowed_relocations, seen = {} }
    end
    for _, actor in ipairs(template.actors) do
      local actor_label = label .. ".template actor " .. actor.slot
      ok, err = validate_port(actor.input, slots, constructors, actor_label .. ".input")
      if not ok then return nil, err end
      if local_slot(actor.input.actor, constructors) ~= actor.slot then return nil, actor_label .. ".input must belong to its actor" end
      for index, output in ipairs(actor.outputs) do
        ok, err = validate_port(output, slots, constructors, string.format("%s.outputs[%d]", actor_label, index))
        if not ok then return nil, err end
        if local_slot(output.actor, constructors) ~= actor.slot then return nil, actor_label .. " output must belong to its actor" end
      end
      local bound_paths = {}
      for index, binding in ipairs(actor.parameter_bindings) do
        ok, err = exact_fields(binding, {path=true,value=true}, string.format("%s.parameter_bindings[%d]", actor_label,index))
        if not ok then return nil, err end
        ok, err = validate_path(binding.path, actor_label .. " parameter path")
        if not ok then return nil, err end
        local previous, present = get_path(actor.params, binding.path)
        if not present then return nil, actor_label .. " parameter binding path is absent" end
        for _, path in ipairs(bound_paths) do
          if paths_overlap(path, binding.path) then return nil, actor_label .. " parameter bindings overlap" end
        end
        bound_paths[#bound_paths + 1] = binding.path
        local declaration = registry:declaration(actor.factory)
        for parameter in pairs(declaration.template.parameter_equals or {}) do
          if binding.path[1] == parameter then return nil, actor_label .. " parameter binding overrides a factory constraint" end
        end
        if not expressions[binding.value] then return nil, actor_label .. " parameter binding expression is absent" end
        local bound_descriptor=descriptors[binding.value]
        if type(bound_descriptor)~="table" or bound_descriptor.kind~="primitive"
            or (bound_descriptor.name~="String" and bound_descriptor.name~="Int") then
          return nil,actor_label.." parameter binding must be a String or Int scalar"
        end
        if (bound_descriptor.name == "String" and type(previous) ~= "string")
            or (bound_descriptor.name == "Int" and (type(previous) ~= "number" or previous % 1 ~= 0)) then
          return nil,actor_label.." parameter binding type differs from its parameter"
        end
      end
    end
    for index, relocation in ipairs(template.actor_reference_relocations) do
      local relocation_label = string.format("%s.template.actor_reference_relocations[%d]", label,index)
      ok, err = exact_fields(relocation, {actor=true,path=true,shape=true}, relocation_label)
      if not ok then return nil, err end
      ok, err = exact_fields(relocation.actor, {slot=true}, relocation_label .. ".actor")
      if not ok then return nil, err end
      local slot = relocation.actor.slot
      if not slots[slot] then return nil, relocation_label .. " names an unknown actor slot" end
      ok, err = validate_path(relocation.path, relocation_label .. ".path")
      if not ok then return nil, err end
      if relocation.shape ~= "actor_id" and relocation.shape ~= "actor_id_list" then
        return nil, relocation_label .. " has unsupported shape " .. tostring(relocation.shape)
      end
      local key = nefor.json.encode({path=relocation.path,shape=relocation.shape})
      local state = relocations_by_slot[slot]
      if not state.allowed[key] or state.seen[key] then
        return nil, relocation_label .. " is undeclared or duplicate for its factory"
      end
      for _, binding in ipairs(actor_by_slot[slot].parameter_bindings) do
        if paths_overlap(binding.path, relocation.path) then
          return nil, relocation_label .. " overlaps a scalar parameter binding"
        end
      end
      local referenced, present = get_path(actor_by_slot[slot].params, relocation.path)
      if not present then return nil, relocation_label .. " path is absent from actor params" end
      if relocation.shape == "actor_id" then
        if not nonempty(referenced) or not slots[referenced] then
          return nil, relocation_label .. " must name one local actor slot"
        end
      else
        if not dense_list(referenced) then
          return nil, relocation_label .. " must name a dense list of local actor slots"
        end
        for _, referenced_slot in ipairs(referenced) do
          if not nonempty(referenced_slot) or not slots[referenced_slot] then
            return nil, relocation_label .. " names an unknown local actor slot"
          end
        end
      end
      state.seen[key] = true
    end
    for slot, state in pairs(relocations_by_slot) do
      for key in pairs(state.allowed) do
        if not state.seen[key] then return nil, "template actor " .. slot .. " omits required factory relocation " .. key end
      end
    end
    local product_routes = {}
    for index, route in ipairs(template.routes) do
      local route_label=string.format("%s.template.routes[%d]",label,index)
      ok,err=exact_fields(route,{from=true,to=true,product_position=true},route_label)
      if not ok then return nil,err end
      ok,err=validate_port(route.from,slots,constructors,route_label..".from"); if not ok then return nil,err end
      ok,err=validate_port(route.to,slots,constructors,route_label..".to"); if not ok then return nil,err end
      local from_slot = local_slot(route.from.actor, constructors)
      if not from_slot then return nil,route_label.." may only originate at a local actor" end
      local source_output = false
      for _, output in ipairs(actor_by_slot[from_slot].outputs) do
        if same_port(route.from, output, constructors) then source_output = true break end
      end
      if not source_output then return nil,route_label..".from is not a declared actor output" end
      local to_ref, to_kind = actor_ref(route.to.actor, constructors)
      if to_kind == "local" then
        if not same_port(route.to, actor_by_slot[to_ref.slot].input, constructors) then
          return nil,route_label..".to is not the declared actor input"
        end
      elseif to_kind == "existing" then
        local target = initial_actors[to_ref.id]
        local input = target and target.input
        if type(input) ~= "table" or input.wire ~= route.to.wire
            or input.type_id ~= route.to.type_id or type_id(input.type) ~= type_id(route.to.type) then
          return nil,route_label..".to is not a declared initial actor input"
        end
      else
        return nil,route_label.." has an invalid target actor reference"
      end
      if type(route.product_position)~="number" or route.product_position%1~=0 or route.product_position < -1 then
        return nil,route_label.." product_position must be an integer >= -1"
      end
      if route.product_position == -1 and type(semantic_host.accepts) == "function"
          and not semantic_host.accepts(route.to.type, route.from.type) then
        return nil,route_label.." source type is incompatible with its target"
      end
      local target_body = route.to.type
      if target_body.kind == "named" then target_body = target_body.body end
      if route.product_position == -1 and target_body and target_body.kind == "product"
          and route.to.type_id ~= route.from.type_id then
        return nil,route_label.." product component route requires a product position"
      end
      if route.product_position >= 0 then
        local descriptor = route.to.type
        if descriptor.kind == "named" then descriptor = descriptor.body end
        local components = type(descriptor) == "table" and descriptor.kind == "product"
          and descriptor.items or nil
        local component = type(components) == "table" and components[route.product_position + 1] or nil
        if not component or type_id(component) ~= route.from.type_id then
          return nil,route_label.." product position does not match the routed source type"
        end
        local key = (to_kind == "local" and "local:" .. to_ref.slot or "existing:" .. to_ref.id)
          .. "/" .. route.to.wire
        local state = product_routes[key]
        if not state then state = {components=#components,seen={}}; product_routes[key]=state end
        if state.components ~= #components or state.seen[route.product_position] then
          return nil,route_label.." has duplicate or inconsistent product routing"
        end
        state.seen[route.product_position] = true
      end
    end
    for _, state in pairs(product_routes) do
      for position = 0, state.components - 1 do
        if not state.seen[position] then return nil,"template has an incomplete product route" end
      end
    end
    for index,message in ipairs(template.messages) do
      local message_label=string.format("%s.template.messages[%d]",label,index)
      ok,err=exact_fields(message,{to=true,semantic_type=true,semantic_type_id=true,content=true},message_label)
      if not ok then return nil,err end
      ok,err=validate_port(message.to,slots,constructors,message_label..".to"); if not ok then return nil,err end
      local to_slot = local_slot(message.to.actor, constructors)
      if not to_slot or not same_port(message.to, actor_by_slot[to_slot].input, constructors) then
        return nil,message_label..".to must be a declared local actor input"
      end
      ok,err=validate_typed(message.semantic_type,message.semantic_type_id,message_label); if not ok then return nil,err end
      if message.semantic_type_id ~= message.to.type_id
          or type_id(message.semantic_type) ~= type_id(message.to.type) then
        return nil,message_label.." semantic type must exactly match its target port"
      end
      ok,err=exact_fields(message.content,{constructor=true,value=true},message_label..".content")
      if not ok then return nil,err end
      if message.content.constructor == "Expression" then
        if not nonempty(message.content.value) or not expressions[message.content.value] then
          return nil,message_label.." expression payload references an unknown expression"
        end
        if type_id(descriptors[message.content.value]) ~= message.semantic_type_id then
          return nil,message_label.." expression payload semantic type does not match the message"
        end
      elseif message.content.constructor == "Static" then
        local content = message.content.value
        ok,err=exact_fields(content,{kind=true,value=false},message_label..".content.value")
        if not ok then return nil,err end
        if content.kind ~= message.to.wire then
          return nil,message_label.." static payload wire does not match its target port"
        end
        local validation = semantic_host.validate_value(message.semantic_type, content.value)
        if not validation.ok then return nil,message_label.." static payload value is malformed" end
      else
        return nil,message_label.." content has an unknown TemplatePayload constructor"
      end
    end
    local member_owners = {}
    for index,node in ipairs(template.nodes) do
      local node_label=string.format("%s.template.nodes[%d]",label,index)
      ok,err=exact_fields(node,{path=true,members=true},node_label); if not ok then return nil,err end
      if not dense_list(node.path) or #node.path==0 or not dense_list(node.members) then return nil,node_label.." has malformed lists" end
      for part,segment in ipairs(node.path) do
        ok,err=exact_fields(segment,{constructor=true,value=true},node_label..".path segment"); if not ok then return nil,err end
        if segment.constructor == constructors.trigger_path then
          ok,err=exact_fields(segment.value,{},node_label..".trigger path"); if not ok then return nil,err end
          if part ~= 1 then return nil,node_label.." trigger path must be the first segment" end
          local owners = 0
          for _, owner in ipairs((initial and initial.nodes) or {}) do
            for _, member in ipairs(owner.members or {}) do
              if member == operation.on_actor then owners = owners + 1 end
            end
          end
          if owners ~= 1 then return nil,node_label.." trigger actor must have one logical owner" end
        else
          ok,err=exact_fields(segment.value,{value=true},node_label..".path segment value"); if not ok then return nil,err end
          if segment.constructor ~= constructors.fixed_path
              and segment.constructor ~= constructors.bound_path then
            return nil,node_label.." path segment has an unknown constructor"
          end
          if not nonempty(segment.value.value) then return nil,node_label.." path segment must be non-empty" end
          if segment.constructor == constructors.bound_path then
            if not expressions[segment.value.value] then
              return nil,node_label.." bound path segment references an unknown expression"
            end
            if type_id(descriptors[segment.value.value]) ~= type_id({kind="primitive",name="String"}) then
              return nil,node_label.." bound path segment must produce String"
            end
          end
        end
      end
      for _,member in ipairs(node.members) do
        ok,err=exact_fields(member,{slot=true},node_label..".member"); if not ok then return nil,err end
        if not slots[member.slot] then return nil,node_label.." names an unknown member slot" end
        if member_owners[member.slot] then return nil,node_label.." repeats a logical member slot" end
        member_owners[member.slot] = true
      end
    end
    for slot in pairs(slots) do
      if not member_owners[slot] then return nil,"template actor "..slot.." has no logical owner" end
    end
    -- Resolve the explicit trigger-path reference from the immutable initial
    -- hierarchy. Node naming/composition may relocate that owner without
    -- changing opaque executable identities or rewriting the template.
    local normalized = plain_data.copy(operation)
    for _, node in ipairs(normalized.template.nodes) do
      if node.path[1].constructor == constructors.trigger_path then
        local path = {}
        for _, owner in ipairs(initial.nodes) do
          for _, member in ipairs(owner.members) do
            if member == operation.on_actor then
              for _, segment in ipairs(owner.path) do
                path[#path + 1] = {constructor=constructors.fixed_path,value={value=segment}}
              end
            end
          end
        end
        for index = 2, #node.path do path[#path + 1] = node.path[index] end
        node.path = path
      end
    end
    owned[#owned+1]=normalized
  end
  return owned
end

local function set_path(root, path, value)
  local current=root
  for index=1,#path-1 do
    local part=path[index]
    if type(current[part])~="table" then return nil,"parameter path does not resolve through an object" end
    current=current[part]
  end
  local leaf=path[#path]
  if current[leaf]==nil then return nil,"parameter path leaf is absent" end
  current[leaf]=plain_data.copy(value)
  return true
end

local function ref_id(ref, ids, constructors)
  local value, kind = actor_ref(ref, constructors)
  if kind == "local" then return ids[value.slot] end
  return value and value.id or nil
end

local function port_value(port, ids, constructors)
  return {actor=ref_id(port.actor,ids,constructors),type=plain_data.copy(port.type),
    type_id=port.type_id,wire=port.wire}
end

-- The one authority for dynamic edge identities. It exactly mirrors
-- nefor.graph.stored-route: canonical JSON of {from=StoredPort,to=StoredPort}.
local function edge_id(from_port,to_port)
  return nefor.json.encode({from=from_port,to=to_port})
end
M.edge_id=edge_id

function M.materialize(operation, trigger_value)
  local constructors=CONSTRUCTORS
  local values={}
  for _,envelope in ipairs(operation.expressions) do
    local expression=envelope.value
    local value
    if expression.capture~=nil then value=operation.captures[expression.capture].value
    elseif expression.record~=nil then value=values[expression.record][expression.field]
    elseif expression.values~=nil then
      local parts={}; for index,id in ipairs(expression.values) do parts[index]=values[id] end
      value=table.concat(parts)
    elseif expression.value~=nil then value=tostring(values[expression.value])
    else value=trigger_value end
    values[expression.id]=plain_data.copy(value)
  end
  local template=operation.template
  local ids={}
  for _,actor in ipairs(template.actors) do
    local id=values[actor.id]
    if not nonempty(id) then return nil,"actor slot "..actor.slot.." id expression did not produce a string" end
    if ids[actor.slot] then return nil,"duplicate actor slot" end
    for _,existing in pairs(ids) do if existing==id then return nil,"materialized actor id collision "..id end end
    ids[actor.slot]=id
  end
  local types=plain_data.copy(template.types)
  local function declare_port(port) types[port.type_id]=plain_data.copy(port.type) end
  local actors,actor_by_slot={},{ }
  for _,template_actor in ipairs(template.actors) do
    local actor={id=ids[template_actor.slot],factory=template_actor.factory,
      type_arguments=plain_data.copy(template_actor.type_arguments),
      params=plain_data.copy(template_actor.params),
      input=port_value(template_actor.input,ids,constructors),outputs={},routes={}}
    declare_port(actor.input)
    for index,output in ipairs(template_actor.outputs) do
      actor.outputs[index]=port_value(output,ids,constructors)
      declare_port(actor.outputs[index])
    end
    for _,binding in ipairs(template_actor.parameter_bindings) do
      local ok,err=set_path(actor.params,binding.path,values[binding.value]); if not ok then return nil,err end
    end
    actors[#actors+1]=actor; actor_by_slot[template_actor.slot]=actor
  end
  for _,relocation in ipairs(template.actor_reference_relocations) do
    local actor=actor_by_slot[relocation.actor.slot]
    local current,present=get_path(actor.params,relocation.path)
    if not present then return nil,"actor reference relocation path is absent" end
    if relocation.shape=="actor_id" then
      if not nonempty(current) or not ids[current] then return nil,"actor_id relocation must name a local slot" end
      set_path(actor.params,relocation.path,ids[current])
    else
      if not dense_list(current) then return nil,"actor_id_list relocation must contain local slots" end
      local relocated={}
      for index,slot in ipairs(current) do
        if not nonempty(slot) or not ids[slot] then return nil,"actor_id_list relocation names an unknown local slot" end
        relocated[index]=ids[slot]
      end
      set_path(actor.params,relocation.path,relocated)
    end
  end
  for _,route in ipairs(template.routes) do
    local from=port_value(route.from,ids,constructors); local to=port_value(route.to,ids,constructors)
    declare_port(from); declare_port(to)
    local source=actor_by_slot[local_slot(route.from.actor, constructors)]
    if not source then return nil,"template routes may only originate at local actors" end
    source.routes[from.wire]=source.routes[from.wire] or {}
    source.routes[from.wire][#source.routes[from.wire]+1]={actor=to.actor,wire=to.wire,
      edge_id=edge_id(from,to),source_type_id=from.type_id,destination_type_id=to.type_id,
      product_position=route.product_position}
  end
  local messages={}
  for index,message in ipairs(template.messages) do
    local to=port_value(message.to,ids,constructors)
    types[message.semantic_type_id]=plain_data.copy(message.semantic_type)
    local content
    if message.content.constructor == "Static" then
      content = plain_data.copy(message.content.value)
    else
      local value = values[message.content.value]
      local host = nefor and nefor.semantic_type
      local validation = type(host) == "table" and type(host.validate_value) == "function"
        and host.validate_value(message.semantic_type, value) or nil
      if type(validation) ~= "table" or not validation.ok then
        return nil,"message expression produced a malformed semantic value"
      end
      content = {kind=to.wire,value=plain_data.copy(value)}
    end
    messages[index]={to=to.actor,semantic_type=plain_data.copy(message.semantic_type),
      semantic_type_id=message.semantic_type_id,content=content}
  end
  local nodes={}
  for index,node in ipairs(template.nodes) do
    local path={}
    for part,segment in ipairs(node.path) do
      local value=segment.value.value
      if segment.constructor==constructors.bound_path then
        path[part]=values[value]
      else
        path[part]=value
      end
    end
    local members={}; for member,ref in ipairs(node.members) do members[member]=ids[ref.slot] end
    nodes[index]={path=path,members=members}
  end
  return {types=types,actors=actors,messages=messages,nodes=nodes,kills={}}
end

return M
