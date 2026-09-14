local kinds = require("kinds")

local M = {}
local owner = { kind = "variable", name = "O" }
local first = { kind = "variable", name = "A" }
local second = { kind = "variable", name = "B" }
local FIRST = "nefor.adt.First"
local SECOND = "nefor.adt.Second"

M.declaration = {
  name = "adt-unpack",
  type_variables = { "O", "A", "B" },
  semantic = {
    input = owner,
    output = { kind = "primitive", name = "JsonValue" },
    inputs = {{ wire = "nefor.adt.Value", type = owner }},
    outputs = {
      { wire = FIRST, type = first },
      { wire = SECOND, type = second },
    },
  },
  params = { owner = "table", left_constructor = "string", right_constructor = "string" },
  inputs = { value = "nefor.adt.Value" },
  outputs = { FIRST, SECOND },
  signals = {},
}

function M.construct(id, params, emit)
  local owner_type = params and params.owner
  local first_constructor = params and params.left_constructor
  local second_constructor = params and params.right_constructor
  if type(owner_type) ~= "table" or owner_type.kind ~= "adt"
      or type(first_constructor) ~= "string" or type(second_constructor) ~= "string"
      or first_constructor == second_constructor then
    return nil, "adt-unpack requires an ADT owner and two distinct constructors"
  end
  local members = {}
  for _, candidate in ipairs(owner_type.constructors or {}) do members[candidate.name] = true end
  if not members[first_constructor] or not members[second_constructor] then
    return nil, "adt-unpack constructor is not owned by its ADT"
  end
  local instance = { id = id }
  function instance.deliver(activation)
    local one = ((activation or {}).messages or {})[1] or {}
    local message = one.message or {}
    local value = message.value
    if value == nil then value = message end
    if type(value) ~= "table" or type(value.constructor) ~= "string"
        or value.value == nil then
      return { status = "failed", failure = kinds.Failed,
        value = { kind = "malformed_adt_value", actor = id } }
    end
    local wire
    if value.constructor == first_constructor then wire = FIRST
    elseif value.constructor == second_constructor then wire = SECOND
    else
      return { status = "failed", failure = kinds.Failed,
        value = { kind = "unexpected_adt_constructor", actor = id,
          constructor = value.constructor } }
    end
    local output = { kind = wire, from = id, value = value.value }
    if message.semantic_value ~= nil then output.semantic_value = message.semantic_value end
    if message.dynamic ~= nil then output.dynamic = message.dynamic end
    emit(output)
    return { status = "ok" }
  end
  emit({ kind = kinds.ready, from = id })
  return instance
end

return M
