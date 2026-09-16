local kinds = require("kinds")

local M = {}
local INPUT = "nefor.node.DiscardInput"
local UNIT = "mag.Unit"
local value = { kind = "variable", name = "T" }
local unit = { kind = "primitive", name = "Unit" }

M.declaration = {
  name = "discard",
  type_variables = { "T" },
  semantic = {
    input = value,
    output = unit,
    inputs = { { wire = INPUT, type = value } },
    outputs = { { wire = UNIT, type = unit } },
  },
  template = { relocations = {} },
  params = {},
  inputs = { value = INPUT },
  outputs = { UNIT },
  signals = {},
}

function M.construct(id, _, emit)
  local instance = { id = id }
  function instance.deliver(_)
    -- Successful completion is the Unit occurrence; the kernel emits it.
    return { status = "ok" }
  end
  emit({ kind = kinds.ready, from = id })
  return instance
end

return M
