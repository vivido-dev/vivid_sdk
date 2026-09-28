-- Run the Lua test suite against the staged native module.
--
-- From the SDK directory, after `lua/build.sh`:
--
--   LUA_CPATH="lua/?.so;;" luajit lua-tests/run.lua [filter]
--
-- The module is loaded with a plain `require`, exactly as a user loads it. A filter runs only the
-- tests whose "file: name" contains it. Exits non-zero when any test fails.

local here = (arg and arg[0] or ""):match("^(.*)[/\\]") or "."
package.path = here .. "/?.lua;" .. package.path

local support = require("support")
local vivid = require("vivid_sdk")

local FILES = {
  "test_exports",
  "test_sdk",
  "test_presenter",
  "test_overlay",
  "test_automation",
}

local filter = arg and arg[1]
local passed, failed, skipped = 0, 0, 0

io.write(string.format("vivid_sdk %s on %s\n", vivid.VERSION, jit and jit.version or _VERSION))
for _, file in ipairs(FILES) do
  local tests = require(file)
  for _, entry in ipairs(tests) do
    local name = file .. ": " .. entry[1]
    if not filter or name:find(filter, 1, true) then
      local ok, err = xpcall(entry[2], function(err)
        if support.is_skip(err) then
          return err
        end
        return debug.traceback(tostring(err), 2)
      end)
      if ok then
        passed = passed + 1
        io.write("ok    ", name, "\n")
      elseif support.is_skip(err) then
        skipped = skipped + 1
        io.write("skip  ", name, " (", err.reason, ")\n")
      else
        failed = failed + 1
        io.write("FAIL  ", name, "\n", err, "\n")
      end
    end
  end
end
io.write(string.format("%d passed, %d failed, %d skipped\n", passed, failed, skipped))
os.exit(failed == 0 and 0 or 1)
