-- lua/libs/mag-workspace/init.lua — MAG workspace management and preview formatting.
--
-- Provides two things:
--   1. Workspace lifecycle: init an empty per-session MAG source directory.
--   2. Preview formatting: render a graph modification (the shape the
--      mag plugin replies with on `mag.loaded`) into a human-readable
--      string the lead can inspect before executing.
--
-- Compilation itself lives in the mag plugin: the lead emits `mag.load`
-- and reads the modification off the `mag.loaded` reply
-- (examples/nefor-agent/lead-workflow/init.lua). The `mag` CLI binary remains a dev
-- tool for humans; nothing here shells out to it.

local M = {}
local configured_sessions_root = nil

function M.configure(options)
  options = options or {}
  configured_sessions_root = options.sessions_root or configured_sessions_root
end

local function sh_quote(value)
  return "'" .. tostring(value):gsub("'", "'\\''") .. "'"
end

local function sessions_root()
  return configured_sessions_root
end

local function mkdir_p(path)
  if nefor and nefor.fs and type(nefor.fs.mkdir_p) == "function" then
    local ok = pcall(nefor.fs.mkdir_p, path)
    if ok then return true end
  end
  local ok = os.execute("mkdir -p " .. sh_quote(path) .. " >/dev/null 2>&1")
  return ok == true or ok == 0
end

-- Get the MAG workspace directory for a session.
function M.workspace_dir(session_id)
  local root = sessions_root()
  if not root then return nil end
  return root .. "/" .. session_id .. "/mag"
end

-- Initialize an empty writable workspace. Canonical and config-owned module
-- roots stay where the package/config materialized them and are supplied to
-- the compiler explicitly; copying them here would create stale session state.
-- Returns the workspace path on success, nil + error on failure.
function M.init_workspace(session_id, _config_dir)
  local ws = M.workspace_dir(session_id)
  if not ws then return nil, "no data root available" end

  if not mkdir_p(ws) then
    return nil, "failed to create workspace: " .. ws
  end

  return ws, nil
end

-- Render one param value compactly: quoted strings (truncated), inline
-- arrays, `{…}` for nested maps.
local MAX_STR = 48

local function format_value(value)
  local t = type(value)
  if t == "string" then
    local s = value
    if #s > MAX_STR then s = s:sub(1, MAX_STR - 1) .. "…" end
    return string.format("%q", s)
  end
  if t == "table" then
    -- Array: render inline. Map: elide (params summaries stay one line).
    if #value > 0 or next(value) == nil then
      local parts = {}
      for _, v in ipairs(value) do parts[#parts + 1] = tostring(v) end
      return "[" .. table.concat(parts, ", ") .. "]"
    end
    return "{…}"
  end
  return tostring(value)
end

local function format_params(params)
  if type(params) ~= "table" or next(params) == nil then return "" end
  local keys = {}
  for k in pairs(params) do keys[#keys + 1] = tostring(k) end
  table.sort(keys)
  local parts = {}
  for _, k in ipairs(keys) do
    parts[#parts + 1] = k .. ": " .. format_value(params[k])
  end
  return " {" .. table.concat(parts, ", ") .. "}"
end

local function format_routes(routes)
  if type(routes) ~= "table" or next(routes) == nil then return nil end
  local keys = {}
  for k in pairs(routes) do keys[#keys + 1] = tostring(k) end
  table.sort(keys)
  local parts = {}
  for _, ty in ipairs(keys) do
    local dests = routes[ty]
    local names = {}
    if type(dests) == "table" then
      for _, destination in ipairs(dests) do
        names[#names + 1] = tostring(destination.actor) .. "/" .. tostring(destination.wire)
      end
    end
    parts[#parts + 1] = ty .. " -> " .. table.concat(names, ", ")
  end
  return table.concat(parts, "; ")
end

local function initial_actor_address(id)
  id = tostring(id or "")
  return "actor:" .. tostring(#id) .. ":" .. id
end

local function template_actor_address(operation_id, slot)
  operation_id, slot = tostring(operation_id or ""), tostring(slot or "")
  return "operation:" .. tostring(#operation_id) .. ":" .. operation_id
    .. ":template:" .. tostring(#slot) .. ":" .. slot
end

local function copy(value)
  if type(value) ~= "table" then return value end
  local out = {}
  for key, child in pairs(value) do out[key] = copy(child) end
  return out
end

local function unpack(value, context)
  local fields = 0
  if type(value) == "table" then
    for _ in pairs(value) do fields = fields + 1 end
  end
  if fields ~= 2 or value["$mag"] ~= "packed-value" or value.value == nil then
    return nil, tostring(context) .. " must be a compiler-owned packed value"
  end
  return copy(value.value)
end

local function unpack_modification(modification, context)
  local materialized = copy(modification)
  for index, actor in ipairs(materialized.actors or {}) do
    local value, err = unpack(actor.params, context .. ".actors[" .. index .. "].params")
    if not value then return nil, err end
    actor.params = value
  end
  for index, message in ipairs(materialized.messages or {}) do
    local value, err = unpack(message.content, context .. ".messages[" .. index .. "].content")
    if not value then return nil, err end
    message.content = value
  end
  return materialized
end

local function unpack_operations(operations)
  local materialized = copy(operations)
  for operation_index, operation in ipairs(materialized) do
    for capture_id, capture in pairs(operation.captures or {}) do
      local value, err = unpack(capture.value, "program.operations[" .. operation_index
        .. "].captures." .. tostring(capture_id) .. ".value")
      if not value then return nil, err end
      capture.value = value
    end
    local template, err = unpack_modification(operation.template or {},
      "program.operations[" .. operation_index .. "].template")
    if not template then return nil, err end
    operation.template = template
  end
  return materialized
end

function M.decode_artifact(artifact)
  if type(artifact) ~= "table" or artifact.format ~= "nefor.mag" or artifact.version ~= 1 then
    return nil, "artifact must be a nefor.mag version 1 envelope"
  end
  if artifact.kind == "program" and type(artifact.program) == "table"
      and type(artifact.program.initial) == "table"
      and type(artifact.program.operations) == "table" then
    local modification, modification_error = unpack_modification(
      artifact.program.initial, "program.initial")
    if not modification then return nil, modification_error end
    local operations, operations_error = unpack_operations(artifact.program.operations)
    if not operations then return nil, operations_error end
    return { kind = "program", modification = modification, operations = operations }
  end
  if artifact.kind == "delta" and type(artifact.delta) == "table" then
    local modification, modification_error = unpack_modification(artifact.delta, "delta")
    if not modification then return nil, modification_error end
    return { kind = "delta", modification = modification, operations = {} }
  end
  return nil, "artifact envelope has an unsupported or malformed variant"
end

function M.actor_inventory(decoded)
  local entries = {}
  for _, actor in ipairs(decoded.modification.actors or {}) do
    entries[#entries + 1] = {
      address = initial_actor_address(actor.id), actor = actor, kind = "initial",
    }
  end
  for operation_index, operation in ipairs(decoded.operations or {}) do
    local template = type(operation) == "table" and operation.template or nil
    for actor_index, actor in ipairs(type(template) == "table" and template.actors or {}) do
      entries[#entries + 1] = {
        address = template_actor_address(operation.id, actor.slot), actor = actor,
        kind = "template", operation = operation, operation_index = operation_index,
        actor_index = actor_index,
      }
    end
  end
  return entries
end

M.initial_actor_address = initial_actor_address
M.template_actor_address = template_actor_address

local function template_ref(ref)
  local value = type(ref) == "table" and (ref.value or ref) or {}
  if value.slot ~= nil then return "slot:" .. tostring(value.slot) end
  if value.id ~= nil then return "existing:" .. tostring(value.id) end
  return "<invalid-ref>"
end

local function template_port(port)
  if type(port) ~= "table" then return "<invalid-port>" end
  return template_ref(port.actor) .. "/" .. tostring(port.wire)
end

local function append_actor(lines, prefix, actor)
  lines[#lines + 1] = string.format("  %s%s (%s)%s", prefix or "",
    tostring(actor.id or actor.slot), tostring(actor.factory), format_params(actor.params))
  local routes = format_routes(actor.routes)
  if routes then lines[#lines + 1] = "    routes: " .. routes end
end

-- Format a versioned immutable program or delta envelope without mutating it.
function M.preview(artifact, hash, factories)
  local decoded, error = M.decode_artifact(artifact)
  if not decoded then return "(invalid MAG artifact: " .. tostring(error) .. ")" end
  local modification = decoded.modification
  local actors, messages = modification.actors or {}, modification.messages or {}
  local operations = decoded.operations or {}
  local lines = {}
  lines[#lines + 1] = string.format("%s envelope: %d initial actors, %d initial messages, %d operations",
    decoded.kind == "program" and "Program" or "Delta", #actors, #messages, #operations)
  lines[#lines + 1] = "Hash: " .. tostring(hash)
  lines[#lines + 1] = ""
  lines[#lines + 1] = "Initial actors:"
  for _, actor in ipairs(actors) do append_actor(lines, "", actor) end

  for index, operation in ipairs(operations) do
    local template = type(operation.template) == "table" and operation.template or {}
    lines[#lines + 1] = ""
    lines[#lines + 1] = string.format("Operation %d: %s on %s/%s", index,
      tostring(operation.id), tostring(operation.on_actor), tostring(operation.on_wire))
    lines[#lines + 1] = string.format("  Template: %d actors, %d routes, %d messages",
      #(template.actors or {}), #(template.routes or {}), #(template.messages or {}))
    for _, actor in ipairs(template.actors or {}) do
      append_actor(lines, "[" .. template_actor_address(operation.id, actor.slot) .. "] ", actor)
    end
    for _, route in ipairs(template.routes or {}) do
      lines[#lines + 1] = string.format("    route: %s -> %s [position %s]",
        template_port(route.from), template_port(route.to), tostring(route.product_position))
    end
    for _, message in ipairs(template.messages or {}) do
      lines[#lines + 1] = "    message -> " .. template_port(message.to)
    end
  end

  if #messages > 0 then
    lines[#lines + 1] = ""
    lines[#lines + 1] = "Initial messages:"
    for _, msg in ipairs(messages) do
      local kind = type(msg.content) == "table" and msg.content.kind or nil
      lines[#lines + 1] = string.format("  -> %s (%s)", tostring(msg.to), tostring(kind or "message"))
    end
  end
  local result = type(modification.result) == "table" and modification.result.from or nil
  if type(result) == "table" then
    lines[#lines + 1] = ""
    lines[#lines + 1] = string.format("Result: %s (%s)", tostring(result.actor), tostring(result.wire))
  end
  if type(factories) == "table" and #factories > 0 then
    local names = {}
    for _, factory in ipairs(factories) do names[#names + 1] = tostring(factory) end
    table.sort(names)
    lines[#lines + 1] = ""
    lines[#lines + 1] = "Registry factories: " .. table.concat(names, ", ")
  end
  return table.concat(lines, "\n")
end

return M
