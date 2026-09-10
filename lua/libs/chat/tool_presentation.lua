local display = require("libs.chat.tool_display")

local M = {}

local function delayed_mag_delivery(entry, args)
  if type(entry.display) ~= "table" or entry.display.lifecycle ~= "delayed" then return nil end
  local is_detached_dispatch = entry.name == "mag-apply"
    and type(args) == "table" and args.run_id == nil
  if not is_detached_dispatch or entry.output == nil then return nil end
  if entry.completion_delivery == "sync" or entry.completion_delivery == "async" then
    return entry.completion_delivery
  end
  return nil
end

function M.projection(entry)
  local args = entry.raw_input
  if args == nil then args = entry.input_table or entry.input end
  local contract = entry.display or display.generic(entry.name)
  local projected, err = display.project(contract, args, entry.output, entry.error)
  if not projected then error("tool display invariant: " .. tostring(err)) end
  local delivery = delayed_mag_delivery(entry, args)
  if delivery then projected.label = projected.label .. " [" .. delivery .. "]" end
  return projected
end

function M.title(entry)
  local projection = M.projection(entry)
  local title = projection.label
  if projection.primary and projection.primary ~= "" then
    title = title .. " · " .. projection.primary
  end
  if entry.output == nil and not entry.error then title = title .. " …" end
  return title
end

return M
