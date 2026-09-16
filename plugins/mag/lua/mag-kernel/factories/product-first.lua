local kinds = require("kinds")

local M = {}
local INPUT = "nefor.node.ProductInput"
local OUTPUT = "nefor.node.ProductLeft"
local left = { kind = "variable", name = "A" }
local right = { kind = "variable", name = "B" }
local product = { kind = "product", items = { left, right } }

M.declaration = {
  name = "product-first",
  type_variables = { "A", "B" },
  semantic = {
    input = product,
    output = left,
    inputs = { { wire = INPUT, type = product } },
    outputs = { { wire = OUTPUT, type = left } },
  },
  template = { relocations = {} },
  params = {},
  inputs = { value = INPUT },
  outputs = { OUTPUT },
  signals = {},
}

local function component(message, index)
  local payload = message and message.message or {}
  local values = payload.value
  if type(values) ~= "table" then return nil, nil end
  local value = values[index]
  local semantic_values = payload.semantic_value
  local semantic = type(semantic_values) == "table" and semantic_values[index] or value
  return value, semantic
end

function M.construct(id, _, emit)
  local instance = { id = id }

  function instance.deliver(activation)
    local messages = (activation or {}).messages or {}
    local value, semantic = component(messages[1], 1)
    if value == nil then
      return {
        status = "failed",
        failure = kinds.Failed,
        value = { kind = "product_first_missing_component", actor = id },
      }
    end
    emit({ kind = OUTPUT, from = id, value = value, semantic_value = semantic })
    return { status = "ok" }
  end

  emit({ kind = kinds.ready, from = id })
  return instance
end

return M
