local tool_presentation = require("libs.chat.tool_presentation")

local M = {}

function M.initial()
  return { next_number = 1 }
end

function M.assign(model)
  model = model or M.initial()
  local number = model.next_number or 1
  return { next_number = number + 1 }, number
end

function M.resolve(entries, requested)
  if type(requested) ~= "string" or requested == "" then return nil end
  local numeric = requested:match("^[1-9]%d*$") and tonumber(requested) or nil
  if numeric ~= nil then
    for _, entry in ipairs(entries or {}) do
      if entry.kind == "tool_call" and entry.raw_number == numeric then return entry end
    end
  end
  for _, entry in ipairs(entries or {}) do
    if entry.kind == "tool_call" and entry.id == requested then return entry end
  end
  return nil
end

function M.candidates(entries, prefix)
  local out = {}
  local query = prefix or ""
  for index = #(entries or {}), 1, -1 do
    local entry = entries[index]
    if entry.kind == "tool_call" and entry.raw_number ~= nil then
      local number = tostring(entry.raw_number)
      if number:sub(1, #query) == query then
        out[#out + 1] = {
          name = "raw " .. number,
          hint = tool_presentation.title(entry),
          takes_args = false,
          raw_number = entry.raw_number,
        }
      end
    end
  end
  return out
end

function M.hint(entry, raw)
  local selector = tostring(entry.raw_number or "?")
  return raw
    and "  raw: visible (/raw " .. selector .. " to hide)"
    or "  raw: hidden (/raw " .. selector .. " to reveal)"
end

return M
