local M = {}

function M.structured_text(data)
  if type(data) ~= "table" or data.value == nil then return nil end
  local json = type(nefor) == "table" and nefor.json or nil
  if type(json) ~= "table" or type(json.encode) ~= "function" then return nil end
  local ok, encoded = pcall(json.encode, data.value)
  if not ok or type(encoded) ~= "string" then return nil end
  return encoded
end

return M
