-- Startup policy shared by agent distributions, not an engine argument API.
local M = {}
local MODES = { safe = true, auto = true, yolo = true }

function M.parse(argv)
  local opts = { frontend = "tui", format = "text" }
  local i = 1
  local format_selected = false
  local function value(flag, description)
    local v = argv[i + 1]
    if type(v) ~= "string" or not v:find("%S") or v:sub(1, 2) == "--" then
      error(flag .. " requires " .. description)
    end
    i = i + 2
    return v
  end
  while i <= #argv do
    local arg = argv[i]
    if arg == "--session" or arg == "--resume" then
      local v = value(arg, "a session id")
      if opts.session_id and opts.session_id ~= v then error("contradictory session selectors") end
      if v:find("[/\\]") or v == "." or v == ".." then error("invalid session id") end
      opts.session_id = v
    elseif arg == "--prompt" then
      opts.prompt = value(arg, "a prompt")
    elseif arg == "--frontend" then
      opts.frontend = value(arg, "tui or cli")
      if opts.frontend ~= "tui" and opts.frontend ~= "cli" then error("invalid frontend: " .. opts.frontend) end
    elseif arg == "--format" then
      format_selected = true
      opts.format = value(arg, "text or json")
      if opts.format ~= "text" and opts.format ~= "json" then error("invalid format: " .. opts.format) end
    elseif arg == "--mode" then
      opts.mode = value(arg, "one of: safe, auto, yolo")
      if not MODES[opts.mode] then error("invalid startup mode: " .. opts.mode) end
    elseif arg == "--yolo" then
      opts.mode = "yolo"
      i = i + 1
    else
      error("unknown startup arg: " .. tostring(arg))
    end
  end
  if opts.frontend == "cli" and not opts.prompt then error("CLI requires a nonempty --prompt") end
  if opts.frontend ~= "cli" and format_selected then error("--format is only supported by the cli frontend") end
  return opts
end

function M.apply_mode(options, loop)
  if options.mode ~= nil then loop.set_mode(options.mode) end
end
return M
