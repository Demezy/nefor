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
    if not allowed[key] then return nil, label .. " has unknown field " .. tostring(key) end
  end
  for key, required in pairs(allowed) do
    if required and value[key] == nil then return nil, label .. " requires " .. key end
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

local function actor_ref(ref)
  if type(ref) ~= "table" then return nil end
  if ref.type ~= nil or ref.value ~= nil then
    if type(ref.type) ~= "string" or type(ref.value) ~= "table" then return nil end
    if ref.value.slot ~= nil and ref.value.id == nil then return ref.value, "local" end
    if ref.value.id ~= nil and ref.value.slot == nil then return ref.value, "existing" end
    return nil
  end
  if ref.slot ~= nil and ref.id == nil then return ref, "local" end
  if ref.id ~= nil and ref.slot == nil then return ref, "existing" end
  return nil
end

local function validate_ref(ref, slots, label)
  local value, kind = actor_ref(ref)
  if not value then return nil, label .. " must be exactly one local or existing actor reference: " .. nefor.json.encode(ref) end
  local ok, err = exact_fields(value, kind == "local" and {slot=true} or {id=true}, label)
  if not ok then return nil, err end
  if kind == "local" and (not nonempty(value.slot) or not slots[value.slot]) then
    return nil, label .. " names an unknown local actor slot"
  end
  if kind == "existing" and not nonempty(value.id) then return nil, label .. " existing id must be non-empty" end
  return true
end

local function local_slot(ref)
  local value, kind = actor_ref(ref)
  return kind == "local" and value.slot or nil
end

local function validate_port(port, slots, label)
  local ok, err = exact_fields(port,
    { actor = true, type = true, type_id = true, wire = true }, label)
  if not ok then return nil, err end
  ok, err = validate_ref(port.actor, slots, label .. ".actor")
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
  return allowed
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
    for expr_index, expression in ipairs(operation.expressions) do
      local expr_label = string.format("%s.expressions[%d]", label, expr_index)
      if type(expression) ~= "table" or not nonempty(expression.id)
          or not nonempty(expression.result_type) or expressions[expression.id] then
        return nil, expr_label .. " has an invalid or duplicate identity"
      end
      local kind
      if expression.capture ~= nil then kind = "capture"
      elseif expression.record ~= nil or expression.field ~= nil then kind = "field"
      elseif expression.values ~= nil then kind = "concat"
      elseif expression.value ~= nil then kind = "int_to_decimal"
      else kind = "trigger" end
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
      local allowed_relocations
      allowed_relocations, err = contract_relocations(registry, actor.factory)
      if not allowed_relocations then return nil, actor_label .. ": " .. err end
      slots[actor.slot], actor_by_slot[actor.slot] = true, actor
      relocations_by_slot[actor.slot] = { allowed = allowed_relocations, seen = {} }
    end
    for _, actor in ipairs(template.actors) do
      local actor_label = label .. ".template actor " .. actor.slot
      ok, err = validate_port(actor.input, slots, actor_label .. ".input")
      if not ok then return nil, err end
      if local_slot(actor.input.actor) ~= actor.slot then return nil, actor_label .. ".input must belong to its actor" end
      for index, output in ipairs(actor.outputs) do
        ok, err = validate_port(output, slots, string.format("%s.outputs[%d]", actor_label, index))
        if not ok then return nil, err end
        if local_slot(output.actor) ~= actor.slot then return nil, actor_label .. " output must belong to its actor" end
      end
      for index, binding in ipairs(actor.parameter_bindings) do
        ok, err = exact_fields(binding, {path=true,value=true}, string.format("%s.parameter_bindings[%d]", actor_label,index))
        if not ok then return nil, err end
        ok, err = validate_path(binding.path, actor_label .. " parameter path")
        if not ok then return nil, err end
        if not expressions[binding.value] then return nil, actor_label .. " parameter binding expression is absent" end
        local bound_descriptor=descriptors[binding.value]
        if type(bound_descriptor)~="table" or bound_descriptor.kind~="primitive"
            or (bound_descriptor.name~="String" and bound_descriptor.name~="Int") then
          return nil,actor_label.." parameter binding must be a String or Int scalar"
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
      state.seen[key] = true
    end
    for slot, state in pairs(relocations_by_slot) do
      for key in pairs(state.allowed) do
        if not state.seen[key] then return nil, "template actor " .. slot .. " omits required factory relocation " .. key end
      end
    end
    for index, route in ipairs(template.routes) do
      local route_label=string.format("%s.template.routes[%d]",label,index)
      ok,err=exact_fields(route,{from=true,to=true,product_position=true},route_label)
      if not ok then return nil,err end
      ok,err=validate_port(route.from,slots,route_label..".from"); if not ok then return nil,err end
      ok,err=validate_port(route.to,slots,route_label..".to"); if not ok then return nil,err end
      if type(route.product_position)~="number" or route.product_position%1~=0 or route.product_position < -1 then
        return nil,route_label.." product_position must be an integer >= -1"
      end
    end
    for index,message in ipairs(template.messages) do
      local message_label=string.format("%s.template.messages[%d]",label,index)
      ok,err=exact_fields(message,{to=true,semantic_type=true,semantic_type_id=true,content=true},message_label)
      if not ok then return nil,err end
      ok,err=validate_port(message.to,slots,message_label..".to"); if not ok then return nil,err end
      ok,err=validate_typed(message.semantic_type,message.semantic_type_id,message_label); if not ok then return nil,err end
    end
    for index,node in ipairs(template.nodes) do
      local node_label=string.format("%s.template.nodes[%d]",label,index)
      ok,err=exact_fields(node,{path=true,members=true},node_label); if not ok then return nil,err end
      if not dense_list(node.path) or #node.path==0 or not dense_list(node.members) then return nil,node_label.." has malformed lists" end
      for _,segment in ipairs(node.path) do
        local segment_value = segment
        if type(segment)=="table" and segment.type~=nil then
          ok,err=exact_fields(segment,{type=true,value=true},node_label..".path segment"); if not ok then return nil,err end
          segment_value=segment.value
        end
        ok,err=exact_fields(segment_value,{value=true},node_label..".path segment value"); if not ok then return nil,err end
        if not nonempty(segment_value.value) then return nil,node_label.." path segment must be non-empty" end
        -- Sum constructors serialize with semantic type ids. A segment whose
        -- value resolves to an expression is bound; every other segment is fixed.
      end
      for _,member in ipairs(node.members) do
        ok,err=exact_fields(member,{slot=true},node_label..".member"); if not ok then return nil,err end
        if not slots[member.slot] then return nil,node_label.." names an unknown member slot" end
      end
    end
    owned[#owned+1]=plain_data.copy(operation)
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

local function get_path(root,path)
  local current=root
  for _,part in ipairs(path) do
    if type(current)~="table" then return nil,false end
    current=current[part]
    if current==nil then return nil,false end
  end
  return current,true
end

local function ref_id(ref, ids)
  local value, kind = actor_ref(ref)
  if kind == "local" then return ids[value.slot] end
  return value and value.id or nil
end

local function port_value(port, ids)
  return {actor=ref_id(port.actor,ids),type=plain_data.copy(port.type),
    type_id=port.type_id,wire=port.wire}
end

-- The one authority for dynamic edge identities. It exactly mirrors
-- nefor.graph.stored-route: canonical JSON of {from=StoredPort,to=StoredPort}.
local function edge_id(from_port,to_port)
  return nefor.json.encode({from=from_port,to=to_port})
end
M.edge_id=edge_id

function M.materialize(operation, trigger_value)
  local values={}
  for _,expression in ipairs(operation.expressions) do
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
      input=port_value(template_actor.input,ids),outputs={},routes={}}
    declare_port(actor.input)
    for index,output in ipairs(template_actor.outputs) do
      actor.outputs[index]=port_value(output,ids)
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
    local from=port_value(route.from,ids); local to=port_value(route.to,ids)
    declare_port(from); declare_port(to)
    local source=actor_by_slot[local_slot(route.from.actor)]
    if not source then return nil,"template routes may only originate at local actors" end
    source.routes[from.wire]=source.routes[from.wire] or {}
    source.routes[from.wire][#source.routes[from.wire]+1]={actor=to.actor,wire=to.wire,
      edge_id=edge_id(from,to),source_type_id=from.type_id,destination_type_id=to.type_id,
      product_position=route.product_position}
  end
  local messages={}
  for index,message in ipairs(template.messages) do
    local to=port_value(message.to,ids)
    types[message.semantic_type_id]=plain_data.copy(message.semantic_type)
    messages[index]={to=to.actor,semantic_type=plain_data.copy(message.semantic_type),
      semantic_type_id=message.semantic_type_id,content=plain_data.copy(message.content)}
  end
  local nodes={}
  for index,node in ipairs(template.nodes) do
    local path={}
    for part,segment in ipairs(node.path) do
      local value=segment.value
      local bound=false
      if segment.type~=nil then value=segment.value.value end
      bound=values[value]~=nil
      path[part]=bound and values[value] or value
    end
    local members={}; for member,ref in ipairs(node.members) do members[member]=ids[ref.slot] end
    nodes[index]={path=path,members=members}
  end
  return {types=types,actors=actors,messages=messages,nodes=nodes,kills={}}
end

return M
