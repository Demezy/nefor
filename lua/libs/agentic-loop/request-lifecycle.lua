-- Whole-request accounting for accepted chat submissions.
-- Execution remains owned by agentic-loop and lead-workflow; this registry only
-- records explicit obligations and emits a terminal request fact once every
-- owner has settled its part.

local M = {}
M.__index = M

local function copy_ids(ids)
  local out, seen = {}, {}
  for _, id in ipairs(ids or {}) do
    if type(id) == "string" and id ~= "" and not seen[id] then
      seen[id] = true
      out[#out + 1] = id
    end
  end
  return out
end

function M.new(opts)
  opts = opts or {}
  return setmetatable({
    requests = {},
    emit = assert(opts.emit, "request-lifecycle: emit is required"),
    blocked = opts.blocked or function() return false end,
  }, M)
end

function M:accept(request_id, session_id)
  assert(type(request_id) == "string" and request_id ~= "",
    "request-lifecycle: request_id is required")
  local request = self.requests[request_id]
  if request then return request end
  request = {
    request_id = request_id,
    session_id = session_id,
    obligations = {},
    terminal = nil,
    completed = false,
  }
  self.requests[request_id] = request
  return request
end

function M:get(request_id)
  return self.requests[request_id]
end

function M:acquire(request_id, obligation_id, session_id)
  local request = self:accept(request_id, session_id)
  if request.completed then return false end
  request.obligations[obligation_id] = true
  return true
end

function M:acquire_all(request_ids, obligation_id, session_id)
  for _, request_id in ipairs(copy_ids(request_ids)) do
    self:acquire(request_id, obligation_id, session_id)
  end
end

function M:set_terminal(request_ids, status, answer, err)
  for _, request_id in ipairs(copy_ids(request_ids)) do
    local request = self.requests[request_id]
    if request and not request.completed then
      if request.forced ~= nil then
        request.terminal = request.forced
      elseif request.outcome ~= nil then
        request.terminal = {
          status = request.outcome.status,
          answer = answer,
          error = request.outcome.error,
        }
      else
        request.terminal = { status = status, answer = answer, error = err }
      end
    end
  end
end

function M:record_outcome(request_ids, status, err)
  for _, request_id in ipairs(copy_ids(request_ids)) do
    local request = self.requests[request_id]
    if request and not request.completed and request.forced == nil then
      if request.outcome == nil or status == "interrupted" then
        request.outcome = { status = status, error = err }
        if request.terminal then
          request.terminal = { status = status, answer = request.terminal.answer, error = err }
        end
      end
    end
  end
end

function M:force(request_id, status, err)
  local request = self.requests[request_id]
  if not request or request.completed then return false end
  request.forced = { status = status, answer = "", error = err }
  request.terminal = request.forced
  return true
end

function M:is_forced(request_id)
  local request = self.requests[request_id]
  return request ~= nil and request.forced ~= nil
end

function M:release(request_id, obligation_id)
  local request = self.requests[request_id]
  if not request or request.completed or not request.obligations[obligation_id] then
    return false
  end
  request.obligations[obligation_id] = nil
  self:recheck(request_id)
  return true
end

function M:release_all(request_ids, obligation_id)
  for _, request_id in ipairs(copy_ids(request_ids)) do
    self:release(request_id, obligation_id)
  end
end

function M:recheck(request_id)
  local request = self.requests[request_id]
  if not request or request.completed or request.terminal == nil then return false end
  if next(request.obligations) ~= nil or self.blocked(request_id) then return false end
  request.completed = true
  local terminal = request.terminal
  local body = {
    kind = "agentic_loop.request_completed",
    request_id = request.request_id,
    session_id = request.session_id,
    status = terminal.status,
    answer = terminal.answer or "",
  }
  if terminal.error ~= nil then body.error = terminal.error end
  self.emit(body)
  return true
end

function M:recheck_all()
  for request_id in pairs(self.requests) do self:recheck(request_id) end
end

function M:reset()
  self.requests = {}
end

M.copy_ids = copy_ids

return M
