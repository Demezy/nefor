-- Deterministic recurrent typed fan-in. Sender identity comes from the kernel
-- activation triple, never from the payload. Per-sender FIFOs preserve
-- overlapping activation cohorts and each complete cohort emits in declaration
-- order.
local kinds = require("kinds")
local M = {}

local ITEM = "nefor.dynamic.Item"
local COLLECTED = "nefor.dynamic.Collected"

M.declaration = {
  name = "collector",
  type_variables = { "T" },
  semantic = {
    input={kind="variable",name="T"},
    output={kind="list",item={kind="variable",name="T"}},
    inputs={{wire=ITEM,type={kind="variable",name="T"}}},
    outputs={{wire=COLLECTED,type={kind="list",item={kind="variable",name="T"}}}},
  },
  params = { expected_senders = "table" },
  template = { relocations = {
    { path = { "expected_senders" }, shape = "actor_id_list" },
  } },
  inputs = { item = ITEM },
  outputs = { COLLECTED },
  signals = { "kill", "drain" },
}

function M.construct(id, params, emit)
  local expected = params and params.expected_senders
  if type(expected) ~= "table" or #expected == 0 then
    return nil, string.format("collector '%s': expected_senders must be a non-empty list", tostring(id))
  end
  local queues = {}
  for index, sender in ipairs(expected) do
    if type(sender) ~= "string" or sender == "" then
      return nil, string.format("collector '%s': expected_senders[%d] must be an id", tostring(id), index)
    end
    if queues[sender] then
      return nil, string.format("collector '%s': duplicate expected sender '%s'", tostring(id), sender)
    end
    queues[sender] = {}
  end

  local closed = false
  local instance = { id = id }

  local function clear()
    for _, sender in ipairs(expected) do queues[sender] = {} end
  end

  local function failure(code, sender)
    closed = true
    clear()
    return { status = "failed", failure = kinds.Failed,
      value = { kind = code, collector = id, sender = sender } }
  end

  function instance.deliver(activation)
    local one = activation and activation.messages and activation.messages[1] or {}
    local sender = one.from
    local queue = queues[sender]
    if closed then return failure("collector_closed", sender) end
    if not queue then return failure("collector_unexpected_sender", sender) end
    local message = type(one.message) == "table" and one.message or {}
    if message.value == nil then return failure("collector_missing_value", sender) end
    local semantic = message.semantic_value
    if semantic == nil then semantic = message.value end
    queue[#queue + 1] = { value = message.value, semantic = semantic }

    for _, expected_sender in ipairs(expected) do
      if #queues[expected_sender] == 0 then return { status = "ok" } end
    end
    local ordered, semantic_ordered = {}, {}
    for index, expected_sender in ipairs(expected) do
      local next_value = table.remove(queues[expected_sender], 1)
      ordered[index] = next_value.value
      semantic_ordered[index] = next_value.semantic
    end
    emit({ kind = COLLECTED, from = id,
      value = ordered, semantic_value = semantic_ordered })
    return { status = "ok" }
  end

  function instance.handle_kill()
    closed = true
    clear()
  end

  function instance.handle_drain()
    local incomplete = false
    for _, sender in ipairs(expected) do
      if #queues[sender] > 0 then incomplete = true break end
    end
    if not closed and incomplete then
      emit({ kind = kinds.failed, from = id, failure = kinds.Failed,
        value = { kind = "collector_drained_incomplete", collector = id } })
    end
    closed = true
    clear()
  end

  emit({ kind = kinds.ready, from = id })
  return instance
end

return M
