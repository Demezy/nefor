local kinds = require("kinds")

local M = {}
local INPUT = "nefor.dynamic.IndexInput"
local OUTPUT = "nefor.dynamic.Indexed"
local item = { kind = "variable", name = "T" }
local indexed = { kind = "named", name = "nefor.dynamic.Indexed", arguments = { item } }

M.declaration = {
  name = "dynamic-index",
  type_variables = { "T" },
  semantic = {
    input = item,
    output = indexed,
    inputs = { { wire = INPUT, type = item } },
    outputs = { { wire = OUTPUT, type = indexed } },
  },
  params = { collection = "string", index = "number" },
  inputs = { value = INPUT },
  outputs = { OUTPUT },
  signals = {},
}

function M.construct(id, params, emit)
  if type(params) ~= "table" or type(params.collection) ~= "string"
      or type(params.index) ~= "number" or params.index < 0 or params.index % 1 ~= 0 then
    return nil, string.format("dynamic-index '%s': invalid collection/index", tostring(id))
  end
  local instance = { id = id }
  function instance.deliver(activation)
    local envelope = ((activation or {}).messages or {})[1] or {}
    local message = envelope.message or {}
    local value = message.value
    if value == nil then value = message end
    local semantic = message.semantic_value or value
    local arrival = envelope.arrival
    if type(arrival) == "table" and type(arrival.declared_type) == "table"
        and arrival.declared_type.kind == "union"
        and type(arrival.constructor_id) == "string" then
      semantic = { type = arrival.constructor_id, value = semantic }
    end
    emit({ kind = OUTPUT, from = id, value = {
      collection = params.collection, index = params.index, value = value,
    }, semantic_value = {
      collection = params.collection, index = params.index,
      value = semantic,
    } })
    return { status = "ok" }
  end
  emit({ kind = kinds.ready, from = id })
  return instance
end

return M
