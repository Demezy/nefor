-- Ordered model context for MAG-capable agents.
--
-- Configuration owns every guide, package root, and trailing ambient section.
-- This module only reads that immutable selection and composes it after an
-- agent-authored system prompt. Tool schemas remain provider-native data.

local M = {}
local Context = {}
Context.__index = Context

local function dense_list(value, label)
  if type(value) ~= "table" then error(label .. " must be a list", 3) end
  local count = 0
  for key in pairs(value) do
    if type(key) ~= "number" or key < 1 or key % 1 ~= 0 then
      error(label .. " must be a dense list", 3)
    end
    count = count + 1
  end
  for index = 1, count do
    if value[index] == nil then error(label .. " must be a dense list", 3) end
  end
  return value
end

local function nonempty(value, label)
  if type(value) ~= "string" or value == "" then
    error(label .. " must be a non-empty string", 3)
  end
  return value
end

local function read_file(path)
  if nefor and nefor.fs and type(nefor.fs.read_file) == "function" then
    local result = nefor.fs.read_file(path)
    if type(result) == "table" and result.ok and type(result.content) == "string" then
      return result.content
    end
    error("mag-context: cannot read " .. path .. ": " .. tostring(result and result.error), 3)
  end
  local file, open_error = io.open(path, "r")
  if not file then error("mag-context: cannot read " .. path .. ": " .. tostring(open_error), 3) end
  local content = file:read("*a")
  file:close()
  return content
end

local function list_dir(path)
  if nefor and nefor.fs and type(nefor.fs.list_dir) == "function" then
    return nefor.fs.list_dir(path) or {}
  end
  return {}
end

local function collect_modules(root, relative, out)
  local path = relative == "" and root or (root .. "/" .. relative)
  for _, entry in ipairs(list_dir(path)) do
    local child = relative == "" and entry.name or (relative .. "/" .. entry.name)
    if entry.is_dir then
      collect_modules(root, child, out)
    elseif entry.name:sub(-4) == ".mag" then
      out[#out + 1] = child:sub(1, -5):gsub("/", ".")
    end
  end
end

local function inventory(packages)
  local lines = { "Available MAG modules:" }
  for _, package in ipairs(packages) do
    local modules = {}
    collect_modules(package.path, "", modules)
    table.sort(modules)
    if #modules > 0 then
      lines[#lines + 1] = ""
      lines[#lines + 1] = package.name .. ":"
      for _, name in ipairs(modules) do lines[#lines + 1] = "  " .. name end
    end
  end
  return table.concat(lines, "\n")
end

local function validated_packages(packages, label)
  local out = {}
  for index, package in ipairs(dense_list(packages or {}, label)) do
    if type(package) ~= "table" then error(label .. " entries must be tables", 3) end
    out[index] = {
      name = nonempty(package.name, label .. "[" .. index .. "].name"),
      path = nonempty(package.path, label .. "[" .. index .. "].path"),
    }
  end
  return out
end

local function append_nonempty(parts, value)
  if type(value) == "string" and value:match("%S") then parts[#parts + 1] = value end
end

local function headed_guide(guide)
  local authored_title = guide.content:match("^%s*#%s+([^\r\n]+)")
  if authored_title == guide.title then return guide.content end
  return "# " .. guide.title .. "\n\n" .. guide.content
end

function Context:compose(base_system, opts)
  opts = opts or {}
  if type(opts) ~= "table" then error("mag-context compose options must be a table", 2) end
  local parts = {}
  append_nonempty(parts, base_system)

  for _, guide in ipairs(self.guides) do
    parts[#parts + 1] = headed_guide(guide)
  end

  local packages = {}
  for _, package in ipairs(self.module_roots) do packages[#packages + 1] = package end
  if opts.workspace ~= nil then
    nonempty(opts.workspace, "mag-context workspace")
    packages[#packages + 1] = { name = "session", path = opts.workspace .. "/lib" }
  end
  for _, package in ipairs(validated_packages(opts.module_roots or {},
      "mag-context compose module_roots")) do
    packages[#packages + 1] = package
  end

  parts[#parts + 1] = "# MAG references\n\nFull MAG Book: `" .. self.book_path ..
    "`\n\nRead its index or the relevant chapter when the quick guides are insufficient.\n\n" ..
    inventory(packages)

  if opts.workspace ~= nil then
    parts[#parts + 1] = "# MAG workspace\n\nWritable source directory: `" .. opts.workspace ..
      "`\n\nPrograms may add session-local modules under `lib/`; configured package roots remain immutable."
  end

  for _, section in ipairs(self.trailing_sections) do append_nonempty(parts, section) end
  for _, section in ipairs(dense_list(opts.trailing_sections or {},
      "mag-context compose trailing_sections")) do
    append_nonempty(parts, section)
  end
  return table.concat(parts, "\n\n---\n\n")
end

function M.new(opts)
  if type(opts) ~= "table" then error("mag-context options must be a table", 2) end
  local guides = {}
  for index, guide in ipairs(dense_list(opts.guides or {}, "mag-context guides")) do
    if type(guide) ~= "table" then error("mag-context guide entries must be tables", 2) end
    local path = nonempty(guide.path, "mag-context guides[" .. index .. "].path")
    guides[index] = {
      title = nonempty(guide.title, "mag-context guides[" .. index .. "].title"),
      path = path,
      content = read_file(path),
    }
  end
  if #guides == 0 then error("mag-context requires at least one guide", 2) end

  local trailing = {}
  for index, section in ipairs(dense_list(opts.trailing_sections or {},
      "mag-context trailing_sections")) do
    trailing[index] = nonempty(section, "mag-context trailing_sections[" .. index .. "]")
  end

  return setmetatable({
    guides = guides,
    book_path = nonempty(opts.book_path, "mag-context book_path"),
    module_roots = validated_packages(opts.module_roots or {}, "mag-context module_roots"),
    trailing_sections = trailing,
  }, Context)
end

return M
