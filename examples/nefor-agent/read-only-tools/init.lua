-- Config-owned selection of repository instruction discovery. Ordinary skills
-- and custom read tools remain available through the shared build seam.
return require("libs.read-only-tools").build {
  include = { "discover_instruction_files" },
}
