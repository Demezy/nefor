local kinds = require("kinds")

local M = {}
local INPUT = "nefor.node.ProductInput"
local OUTPUT = "nefor.node.ProductOutput"
local left = { kind = "variable", name = "A" }
local right = { kind = "variable", name = "B" }
local product = { kind = "product", items = { left, right } }

M.declaration = {
  name = "product-join",
  type_variables = { "A", "B" },
  semantic = {
    input = product,
    output = product,
    inputs = { { wire = INPUT, type = product } },
    outputs = { { wire = OUTPUT, type = product } },
  },
  params = { expected_senders = "table" },
  template = { relocations = {
    { path = { "expected_senders" }, shape = "actor_id_list" },
  } },
  inputs = { value = INPUT },
  outputs = { OUTPUT },
  signals = {},
}

function M.construct(id, params, emit)
  local expected = params and params.expected_senders
  if type(expected) ~= "table" or #expected ~= 2
      or type(expected[1]) ~= "string" or type(expected[2]) ~= "string"
      or expected[1] == expected[2] then
    return nil, string.format(
      "product-join '%s': expected_senders must contain two distinct ids", tostring(id))
  end
  local positions = { [expected[1]] = 1, [expected[2]] = 2 }
  local instance = { id = id }

  function instance.deliver(activation)
    local values, semantic_values = {}, {}
    for _, message in ipairs((activation or {}).messages or {}) do
      local position = positions[message.from]
      if not position or values[position] ~= nil then
        return {
          status = "failed",
          failure = kinds.Failed,
          value = { kind = "product_join_unexpected_sender", actor = id,
            sender = message.from },
        }
      end
      local payload = message.message or {}
      local value = payload.value
      if value == nil then value = payload end
      values[position] = value
      local semantic = payload.semantic_value or value

      semantic_values[position] = semantic
    end
    if values[1] == nil or values[2] == nil then
      return {
        status = "failed",
        failure = kinds.Failed,
        value = { kind = "product_join_missing_component", actor = id },
      }
    end
    emit({ kind = OUTPUT, from = id, value = values, semantic_value = semantic_values })
    return { status = "ok" }
  end

  emit({ kind = kinds.ready, from = id })
  return instance
end

return M
