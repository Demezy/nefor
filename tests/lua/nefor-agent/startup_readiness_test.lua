local readiness = require("libs.startup-readiness")

local function assert_eq(actual, expected, message)
  if actual ~= expected then
    error((message or "values differ") .. ": expected " .. tostring(expected)
      .. ", got " .. tostring(actual), 2)
  end
end

local required_plugins = { "provider", "mag", "tool-gate", "basic-tools", "chat-surface" }
local required_tools = { "read_file", "process.exec", "shell.script", "mag" }
local sources = {
  ["basic-tools"] = { "read_file", "process.exec", "shell.script" },
  ["lead-workflow"] = { "mag" },
}

-- Delayed and reordered announcements settle only after both liveness and the
-- complete replacement catalog have arrived.
do
  local ready = 0
  local barrier = readiness._new {
    required_plugins = required_plugins,
    required_tools = required_tools,
    tool_sources = sources,
    on_ready = function() ready = ready + 1 end,
  }
  barrier.observe({ kind = "tool.register", tools = {
    { name = "mag" }, { name = "shell.script" }, { name = "process.exec" }, { name = "read_file" },
  } }, "tool-gate")
  barrier.observe({ kind = "basic-tools.hello" }, "basic-tools")
  barrier.observe({ kind = "tool-gate.hello" }, "tool-gate")
  assert_eq(ready, 0, "catalog alone cannot release prompt")
  barrier.observe({ kind = "provider.hello" }, "provider")
  barrier.observe({ kind = "mag.hello" }, "mag")
  assert_eq(ready, 0, "backend readiness cannot release prompt before the TUI composition")
  barrier.observe({ kind = "chat.surface.ready" }, "nefor-tui")
  assert_eq(ready, 1, "TUI composition readiness releases prompt")
  barrier.observe({ kind = "mag.hello" }, "mag")
  assert_eq(ready, 1, "readiness fires once")
end

-- Model-selection acknowledgements, including restored/replayed ones, are not
-- process liveness. The selected provider must announce a current hello.
do
  local selected = "saved-provider"
  local ready = 0
  local barrier = readiness._new {
    required_plugins = { "mag" },
    required_provider = function() return selected end,
    required_tools = {},
    on_ready = function() ready = ready + 1 end,
  }
  barrier.observe({ kind = "chat.model.set_ack", provider = selected, model = "saved-model" })
  barrier.observe({ kind = selected .. ".hello" }, selected, true)
  barrier.observe({ kind = "mag.hello" }, "mag")
  assert_eq(ready, 0, "acknowledgements and replayed hellos cannot establish current readiness")
  barrier.observe({ kind = selected .. ".hello" }, selected)
  assert_eq(ready, 1, "current selected-provider hello establishes readiness")
end

-- Provider selection is sampled from the resumed conversation rather than
-- frozen to the composition default.
do
  local selected = "default-provider"
  local activated = false
  local barrier = readiness._new {
    required_plugins = {},
    required_provider = function() return selected end,
    required_tools = {},
    is_ready = function() return activated end,
  }
  barrier.observe({ kind = "default-provider.hello" }, "default-provider")
  selected = "saved-provider"
  activated = true
  assert_eq(barrier.snapshot().ready, false, "saved provider replaces default readiness requirement")
  barrier.observe({ kind = "saved-provider.hello" }, "saved-provider")
  assert_eq(barrier.snapshot().ready, true, "saved provider current hello releases readiness")
end

-- A partial catalog is replaced atomically. Seeing required names spread over
-- separate advertisements must not manufacture a complete catalog.
do
  local barrier = readiness._new {
    required_plugins = {},
    required_tools = required_tools,
    tool_sources = sources,
  }
  barrier.observe({ kind = "tool.register", tools = { { name = "mag" } } }, "tool-gate")
  barrier.observe({ kind = "tool.register", tools = {
    { name = "read_file" }, { name = "process.exec" }, { name = "shell.script" },
  } }, "tool-gate")
  local state = barrier.snapshot()
  assert_eq(state.ready, false, "replacement catalog remains incomplete")
  assert_eq(table.concat(state.missing_tools, ","), "mag", "latest catalog owns readiness")
end

-- Omitted advertisement times out with the absent tool and source named.
do
  local failure
  local barrier = readiness._new {
    required_plugins = required_plugins,
    required_tools = required_tools,
    tool_sources = sources,
    on_error = function(message) failure = message end,
  }
  for _, name in ipairs(required_plugins) do
    barrier.observe({ kind = name .. ".hello" }, name)
  end
  barrier.observe({ kind = "tool.register", tools = {
    { name = "read_file" }, { name = "process.exec" }, { name = "shell.script" },
  } }, "tool-gate")
  barrier.timeout()
  assert(failure and failure:find("mag", 1, true), "diagnostic names missing tool")
  assert(failure:find("lead-workflow", 1, true), "diagnostic names missing source")
  assert(failure:find("installed", 1, true), "diagnostic gives corrective direction")
end

print("startup_readiness_test: all assertions passed")
