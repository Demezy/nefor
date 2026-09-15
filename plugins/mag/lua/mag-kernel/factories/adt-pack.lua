local kinds = require("kinds")
local type_node = require("type-node")

local M = {}
local payload = { kind = "variable", name = "T" }
local owner = { kind = "variable", name = "O" }

M.declaration = {
  name = "adt-pack",
  type_variables = { "T", "O" },
  semantic = {
    input = payload,
    output = owner,
    inputs = {{ wire = "nefor.adt.Payload", type = payload }},
    outputs = {{ wire = "nefor.adt.Value", type = owner }},
    params = { owner = owner, payload = payload },
  },
  params = { owner = "table", payload = "table", constructor = "string" },
  inputs = { value = "nefor.adt.Payload" },
  outputs = { "nefor.adt.Value" },
  signals = {},
}

function M.construct(id, params, emit)
  local owner_type = params and params.owner
  local payload_type = params and params.payload
  local constructor = params and params.constructor
  if type(owner_type) ~= "table" or owner_type.kind ~= "adt"
      or type(payload_type) ~= "table" or type(constructor) ~= "string" then
    return nil, "adt-pack requires an ADT owner, payload type, and constructor name"
  end
  local member = nil
  for _, candidate in ipairs(owner_type.constructors or {}) do
    if candidate.name == constructor then member = candidate break end
  end
  if not member then return nil, "adt-pack constructor is not owned by its ADT" end
  if not type_node.equal(member.payload, payload_type) then
    return nil, "adt-pack payload type does not match its constructor"
  end
  local instance = { id = id }
  function instance.deliver(activation)
    local one = ((activation or {}).messages or {})[1] or {}
    local message = one.message or {}
    local value = message.value
    if value == nil then value = message end
    local output = { kind = "nefor.adt.Value", from = id,
      value = { constructor = constructor, value = value } }
    if message.semantic_value ~= nil then
      if message.dynamic ~= nil then output.semantic_value = message.semantic_value
      else output.semantic_value = { constructor = constructor, value = message.semantic_value } end
    end
    if message.dynamic ~= nil then output.dynamic = message.dynamic end
    emit(output)
    return { status = "ok" }
  end
  emit({ kind = kinds.ready, from = id })
  return instance
end

return M
