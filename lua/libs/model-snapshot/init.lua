local M = {}

local function copy_resolved_model(model, label)
  if type(model) ~= "table" then
    return nil, label .. " must be a table"
  end
  local copy = {}
  for key, value in pairs(model) do
    if key ~= "provider" and key ~= "model" and key ~= "reasoning_effort" then
      return nil, label .. " has unknown field " .. tostring(key)
    end
    copy[key] = value
  end
  if type(copy.provider) ~= "string" or copy.provider == "" then
    return nil, label .. " provider must be a non-empty string"
  end
  if type(copy.model) ~= "string" or copy.model == "" then
    return nil, label .. " model must be a non-empty string"
  end
  if copy.reasoning_effort ~= nil
      and (type(copy.reasoning_effort) ~= "string" or copy.reasoning_effort == "") then
    return nil, label .. " reasoning_effort must be a non-empty string when present"
  end
  return copy
end

function M.copy(snapshot)
  if type(snapshot) ~= "table" then
    return nil, "model snapshot callback must return a table"
  end
  local current = {}
  for key, value in pairs(snapshot) do
    if key ~= "provider" and key ~= "model" and key ~= "reasoning_effort"
        and key ~= "profiles" then
      return nil, "model snapshot has unknown field " .. tostring(key)
    end
    if key ~= "profiles" then current[key] = value end
  end
  local copy, current_error = copy_resolved_model(current, "model snapshot")
  if copy == nil then return nil, current_error end
  if snapshot.profiles == nil then return copy end
  if type(snapshot.profiles) ~= "table" then
    return nil, "model snapshot profiles must be a table when present"
  end
  copy.profiles = {}
  for name, model in pairs(snapshot.profiles) do
    if type(name) ~= "string" or name == "" then
      return nil, "model snapshot profile names must be non-empty strings"
    end
    local resolved, profile_error = copy_resolved_model(
      model, "model snapshot profile " .. string.format("%q", name))
    if resolved == nil then return nil, profile_error end
    copy.profiles[name] = resolved
  end
  return copy
end

return M
