local kinds = require("kinds")

local M = {}
local INPUT = "nefor.node.ProductInput"
local LEFT = "nefor.node.ProductLeft"
local RIGHT = "nefor.node.ProductRight"
local left = { kind = "variable", name = "A" }
local right = { kind = "variable", name = "B" }
local product = { kind = "product", items = { left, right } }

M.declaration = {
  name = "product-split",
  type_variables = { "A", "B" },
  semantic = {
    input = product,
    output = product,
    inputs = { { wire = INPUT, type = product } },
    outputs = {
      { wire = LEFT, type = left },
      { wire = RIGHT, type = right },
    },
  },
  template = { relocations = {} },
  params = {},
  inputs = { value = INPUT },
  outputs = { LEFT, RIGHT },
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
    local left_value, left_semantic
    local right_value, right_semantic
    if activation and activation.shape == "product" and not activation.whole then
      local left_message = messages[1] or {}
      local right_message = messages[2] or {}
      left_value = (left_message.message or {}).value
      if left_value == nil then left_value = left_message.message end
      right_value = (right_message.message or {}).value
      if right_value == nil then right_value = right_message.message end
      left_semantic, right_semantic = left_value, right_value
    else
      left_value, left_semantic = component(messages[1], 1)
      right_value, right_semantic = component(messages[1], 2)
    end
    if left_value == nil or right_value == nil then
      return {
        status = "failed",
        failure = kinds.Failed,
        value = { kind = "product_split_missing_component", actor = id },
      }
    end
    emit({ kind = LEFT, from = id, value = left_value, semantic_value = left_semantic })
    emit({ kind = RIGHT, from = id, value = right_value, semantic_value = right_semantic })
    return { status = "ok" }
  end

  emit({ kind = kinds.ready, from = id })
  return instance
end

return M
