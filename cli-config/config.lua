-- cli-config/config.lua — deterministic settings for the agentic-cli example.

local M = {}

-- Binary path resolver. Tests may select Cargo's effective target directory;
-- normal in-tree use keeps the repository's `target/debug` default.
do
  local CONFIG_ROOT = NEFOR_CONFIG_DIR or "."
  local PROJECT_ROOT = CONFIG_ROOT:match("^(.*)/[^/]+$") or "."
  local BIN_ROOT = os.getenv("NEFOR_TEST_BIN_DIR") or (PROJECT_ROOT .. "/target/debug")
  M.bin = function(name) return BIN_ROOT .. "/" .. name end
end

M.active = {
  provider = {
    name        = "mock-plugin",
    model       = "mock-model",
    -- Resolved against STARTER_ROOT in init.lua at load time.
    mock_script = "mock-provider/init.lua",
  },
  tool_gate = {
    -- CLI surface has no permission-prompt UI in v1. Default `auto`
    -- keeps the agent unblocked. --yolo on agentic-cli is the
    -- documented user override (currently a placeholder).
    default_action = "auto",
    prompt_tools   = {},
  },
  log_level = "warn",
}

return M
