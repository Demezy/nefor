local kinds = require("kinds")

local M = {}
local INPUT = "nefor.node.SequenceInput"
local OUTPUT = "nefor.node.SequenceOutput"
local input = { kind = "variable", name = "I" }
local item = { kind = "variable", name = "O" }
local list = { kind = "list", item = item }

M.declaration = {
  name = "sequence-empty",
  type_variables = { "I", "O" },
  semantic = {
    input = input,
    output = list,
    inputs = { { wire = INPUT, type = input } },
    outputs = { { wire = OUTPUT, type = list } },
  },
  template = { relocations = {} },
  params = {},
  inputs = { value = INPUT },
  outputs = { OUTPUT },
  signals = {},
}

function M.construct(id, _, emit)
  local instance = { id = id }

  function instance.deliver(_)
    emit({ kind = OUTPUT, from = id,
      value = nefor.json.mark_array({}), semantic_value = nefor.json.mark_array({}) })
    return { status = "ok" }
  end

  emit({ kind = kinds.ready, from = id })
  return instance
end

return M
