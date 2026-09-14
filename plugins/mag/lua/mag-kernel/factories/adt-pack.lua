local kinds = require("kinds")

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
  },
  params = { owner = "table", constructor = "string" },
  inputs = { value = "nefor.adt.Payload" },
  outputs = { "nefor.adt.Value" },
  signals = {},
}

function M.construct(id, params, emit)
  local owner_type = params and params.owner
  local constructor = params and params.constructor
  if type(owner_type) ~= "table" or owner_type.kind ~= "adt"
      or type(constructor) ~= "string" then
    return nil, "adt-pack requires an ADT owner and constructor name"
  end
  local member = false
  for _, candidate in ipairs(owner_type.constructors or {}) do
    if candidate.name == constructor then member = true break end
  end
  if not member then return nil, "adt-pack constructor is not owned by its ADT" end
  local instance = { id = id }
  function instance.deliver(activation)
    local one = ((activation or {}).messages or {})[1] or {}
    local message = one.message or {}
    local value = message.value
    if value == nil then value = message end
    emit({ kind = "nefor.adt.Value", from = id,
      value = { constructor = constructor, value = value } })
    return { status = "ok" }
  end
  emit({ kind = kinds.ready, from = id })
  return instance
end

return M
