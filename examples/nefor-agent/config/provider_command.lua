local M = {}

function M.chatgpt(binary, descriptor)
  local command = { binary, "--name", descriptor.name }
  if descriptor.base_url then
    command[#command + 1] = "--base-url"
    command[#command + 1] = descriptor.base_url
  end
  for _, argument in ipairs(descriptor.extra_args or {}) do
    command[#command + 1] = argument
  end
  return command
end

return M
