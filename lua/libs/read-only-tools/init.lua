-- Config-selected instruction discovery and skill tools.
--
-- Advertised through tool-gate as source `read-only-tools`. Base tools are
-- opt-in through `build{ include = {...} }`: discover_instruction_files finds
-- repository AGENTS.md/CLAUDE.md paths, while skill loads an ordinary workflow
-- skill from the config. Neither is enabled by default.
--
-- Config-specific tools use `extra_tools = { { schema, handler } }`, where
-- handler(args, emit) settles with emit.ok(text) or emit.err(message). Included
-- tools and extras share advertisement and dispatch on the first gate hello.

local json = nefor.json

local envelope = require("core.envelope")
local emit_as  = envelope.emit_as
local instruction_files = require("libs.instruction-files")
local tool_display = require("libs.chat.tool_display")

local function field(label, source, path, kind, extra)
  local value = { label = label, select = { source = source, path = path }, kind = kind }
  for key, item in pairs(extra or {}) do value[key] = item end
  return value
end

local function display(label, primary, fields, result_kind, result_text, result_fields, lifecycle)
  return {
    compact = { label = label, primary = primary },
    expanded = { label = label, fields = tool_display.fields(fields) },
    result = { kind = result_kind, text = result_text, fields = tool_display.fields(result_fields) },
    lifecycle = lifecycle,
  }
end

local SOURCE_NAME = "read-only-tools"

local function emit_ok(firing_id, text)
  emit_as(SOURCE_NAME, nil, {
    kind   = "tool.result",
    id     = firing_id,
    output = tostring(text or ""),
  })
end

local function emit_err(firing_id, err)
  emit_as(SOURCE_NAME, nil, {
    kind  = "tool.result",
    id    = firing_id,
    error = tostring(err),
  })
end

local function tool_discover_instruction_files(firing_id, args)
  args = args or {}
  local path = type(args.path) == "string" and args.path or "."
  local scope = args.scope == "subfolders" and "subfolders" or "auto"
  local unread_only = args.unread_only == true
  local result = instruction_files.discover(path, {
    scope = scope,
    unread_only = unread_only,
  })
  emit_ok(firing_id, instruction_files.format_discovery(result))
end

-- Load an ordinary workflow skill from the config-owned skills directory.
local SKILLS_DIR = (rawget(_G, "NEFOR_CONFIG_DIR") or ".") .. "/skills"

local function read_one_skill(raw_name)
  local name = raw_name:gsub("/skill%.md$", ""):gsub("%.md$", "")
  local path = SKILLS_DIR .. "/" .. name .. "/skill.md"
  local f, err = io.open(path, "r")
  if not f then
    return nil, tostring(err or ("no skill at " .. path))
  end
  local content = f:read("*a")
  f:close()
  if not content or #content == 0 then
    return nil, "empty skill at " .. path
  end
  return content, nil
end

local function tool_skill(firing_id, args)
  local name = args and args.name
  if type(name) == "string" and #name > 0 then
    local content, err = read_one_skill(name)
    if not content then emit_err(firing_id, "skill: " .. err); return end
    emit_ok(firing_id, content)
    return
  end
  if type(name) == "table" and #name > 0 then
    local parts = {}
    for _, n in ipairs(name) do
      if type(n) == "string" and #n > 0 then
        local content, err = read_one_skill(n)
        parts[#parts + 1] = "--- skill: " .. n .. " ---\n" ..
          (content or ("[error: " .. tostring(err) .. "]"))
      end
    end
    if #parts == 0 then
      emit_err(firing_id, "skill: name array contained no valid entries")
      return
    end
    emit_ok(firing_id, table.concat(parts, "\n\n"))
    return
  end
  emit_err(firing_id, "skill: args.name must be a non-empty string or array of strings")
end

local BASE_HANDLERS = {
  discover_instruction_files = tool_discover_instruction_files,
  skill                      = tool_skill,
}

local function base_schemas()
  return {
    {
      name = "discover_instruction_files",
      display = display("discover instructions", field("path", "args", "path", "path", { omit = "missing" }), { field("scope", "args", "scope", "scalar", { omit = "missing" }), field("unread only", "args", "unread_only", "scalar", { omit = "missing" }) }, "content", nil, { field("status", "result", "$", "text", { max_lines = 80, max_bytes = 6400 }) }),
      description =
        "List AGENTS.md and CLAUDE.md instruction files available near " ..
        "a path. Does not read file contents. Use ordinary read_file on " ..
        "any file that seems relevant.",
      parameters = {
        type = "object",
        properties = {
          path = {
            type = "string",
            description = "Directory or file path to inspect. Defaults to '.'.",
          },
          scope = {
            type = "string",
            enum = { "auto", "subfolders" },
            description =
              "auto: git repo when inside one, otherwise subfolders. " ..
              "subfolders: only below path.",
          },
          unread_only = {
            type = "boolean",
            description = "Only show instruction files not read this session.",
          },
        },
      },
    },
    {
      name = "skill",
      display = display("load skill", field("source", "args", "name", "list"), {}, "receipt", "skill loaded", { field("status", "result", "$", "status", { sensitive = "omit" }) }),
      description =
        "Load a workflow skill by name from the config's skills directory " ..
        "(" .. SKILLS_DIR .. "/<name>/skill.md). Read the skill BEFORE acting on a " ..
        "task that matches its description — it carries the workflow, CLI " ..
        "usage, and conventions. Pass an array to load several at once.",
      parameters = {
        type = "object",
        properties = {
          name = {
            oneOf = {
              { type = "string" },
              { type = "array", items = { type = "string" } },
            },
            description = "Skill name or array of skill names.",
          },
        },
        required = { "name" },
      },
    },
  }
end

-- Wrap a config-registered handler so it receives a firing-bound `emit`
-- table ({ ok = fn(text), err = fn(msg) }) and never has to touch the
-- envelope layer or SOURCE_NAME.
local function wrap_extra(handler)
  return function(firing_id, args)
    handler(args, {
      ok  = function(text) emit_ok(firing_id, text) end,
      err = function(msg) emit_err(firing_id, msg) end,
    })
  end
end

-- build{ include = { "skill", ... }, extra_tools = { { schema, handler } } }
--   -> actor spec.
--
-- Base tools are OPT-IN: only the names listed in `include` are registered and
-- advertised. Adding a new base tool to this lib therefore cannot leak into any
-- config until that config lists it — no silent contamination. `extra_tools`
-- registers config-specific tools through the same seam.
local function build(opts)
  opts = opts or {}
  local include = opts.include or {}
  local extra_tools = opts.extra_tools or {}

  local base_by_name = {}
  for _, schema in ipairs(base_schemas()) do
    base_by_name[schema.name] = schema
  end

  local handlers = {}
  local schemas = {}
  local function validate_schema(schema)
    local ok, err = tool_display.validate(schema.display)
    if not ok then error("read-only-tools.build: tool '" .. tostring(schema.name) .. "' has invalid display: " .. tostring(err)) end
  end

  for _, name in ipairs(include) do
    local schema = base_by_name[name]
    if not schema then
      error("read-only-tools.build: unknown base tool '" .. tostring(name) ..
        "' in include (known: discover_instruction_files, skill)")
    end
    if handlers[name] then
      error("read-only-tools.build: base tool '" .. name .. "' listed twice in include")
    end
    validate_schema(schema)
    handlers[name] = BASE_HANDLERS[name]
    schemas[#schemas + 1] = schema
  end

  for _, spec in ipairs(extra_tools) do
    local schema = spec.schema
    if type(schema) ~= "table" or type(schema.name) ~= "string" then
      error("read-only-tools.build: extra tool needs a schema with a string name")
    end
    if type(spec.handler) ~= "function" then
      error("read-only-tools.build: extra tool '" .. schema.name .. "' needs a handler function")
    end
    if handlers[schema.name] then
      error("read-only-tools.build: tool '" .. schema.name .. "' already registered")
    end
    validate_schema(schema)
    handlers[schema.name] = wrap_extra(spec.handler)
    schemas[#schemas + 1] = schema
  end

  local function handle_tool_invoke(body)
    local firing_id = body.id
    if type(firing_id) ~= "string" then return end
    local handler = handlers[body.name]
    if not handler then
      emit_err(firing_id, "read-only-tools: unknown tool '" ..
        tostring(body.name) .. "'")
      return
    end
    -- We advertised the tool; the caller is owed a tool.result. A handler
    -- crash without this wrapper produces no envelope on the bus, which
    -- the agent reasoner reads as "still running" and hangs forever.
    local ok, err = pcall(handler, firing_id, body.args or {})
    if not ok then
      emit_err(firing_id, "read-only-tools." .. tostring(body.name) ..
        ": handler raised: " .. tostring(err))
    end
  end

  local advertised = false
  local function advertise_tools(gate_name)
    if advertised then return end
    advertised = true
    emit_as(SOURCE_NAME, nil, {
      kind   = (gate_name or "tool-gate") .. ".tools.advertise",
      source = SOURCE_NAME,
      tools  = schemas,
    })
  end

  local function receive_msg(entry)
    if entry.origin == "step" and entry.target ~= nil then return end
    local payload = entry.payload
    if type(payload) ~= "string" or payload == "" then return end
    local ok, decoded = pcall(json.decode, payload)
    if not ok or type(decoded) ~= "table" or type(decoded.body) ~= "table" then return end
    local body = decoded.body
    local kind = body.kind
    if type(kind) ~= "string" then return end

    if kind == SOURCE_NAME .. ".tool.invoke" then
      handle_tool_invoke(body)
      return
    end
    if kind == "tool-gate.hello" then
      advertise_tools("tool-gate")
      return
    end
  end

  return {
    name        = SOURCE_NAME,
    receive_msg = receive_msg,
    send_msg    = function(_) end,
  }
end

return {
  build = build,
}
