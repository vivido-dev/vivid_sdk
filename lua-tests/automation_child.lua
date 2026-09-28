-- The client side of one automation scenario, run by `test_automation.lua` in a child
-- interpreter whose environment it controls: `XDG_RUNTIME_DIR` is the scenario's runtime root and
-- `VIVIDO_SESSION` and `VIVIDO_SOCKET` are unset unless the scenario sets them.
--
-- Usage: automation_child.lua SCENARIO ROOT. Prints `passed` on success.

local here = arg[0]:match("^(.*)[/\\]") or "."
package.path = here .. "/?.lua;" .. package.path

local vivid = require("vivid_sdk")
local t = require("support")

local auto = vivid.automation
local scenario, root = arg[1], arg[2]

local function code(fn, ...)
  return t.raises("automation", nil, fn, ...).code
end

local scenarios = {}

function scenarios.named_target()
  local session = auto.vivido_connect({ target = "scratch", timeout = t.TIMEOUT })
  t.eq(#session.capabilities.methods, 0)
  session:close()
end

function scenarios.registry_points_elsewhere()
  -- The socket path is derived from the name, so a registry naming another path is refused.
  t.eq(code(auto.vivido_connect, { target = "scratch", timeout = t.TIMEOUT }), "endpoint_unsafe")
end

function scenarios.stale_registry()
  local info = t.raises("automation", "no longer running", auto.vivido_connect,
    { target = "scratch", timeout = t.TIMEOUT })
  t.eq(info.code, "endpoint_not_found")
end

function scenarios.gone_target()
  -- A live instance exists, but the named one does not: an error, never a fall-through.
  t.eq(code(auto.vivido_connect, { target = "missing", timeout = t.TIMEOUT }), "endpoint_not_found")
end

function scenarios.gone_session_environment()
  t.eq(os.getenv("VIVIDO_SESSION"), "missing")
  t.eq(code(auto.vivido_connect, { timeout = t.TIMEOUT }), "endpoint_not_found")
end

function scenarios.sole_instance()
  local session = auto.vivido_connect({ timeout = t.TIMEOUT })
  t.eq(#session.capabilities.methods, 0)
  session:close()
end

function scenarios.instances()
  local names = {}
  for _, instance in ipairs(auto.vivido_instances()) do
    names[#names + 1] = instance.name
  end
  t.same(names, { "alive" })
end

function scenarios.unsafe_runtime()
  assert(t.execute("chmod 755 " .. t.shell_quote(root .. "/vivido")))
  t.eq(code(auto.vivido_connect, { timeout = t.TIMEOUT }), "endpoint_unsafe")
  assert(t.execute("chmod 700 " .. t.shell_quote(root .. "/vivido")))
end

function scenarios.vvmux_round_trip()
  local session = auto.vvmux_connect("default", { timeout = t.TIMEOUT })
  local panes = session:request({ method = "list_panes" })
  t.eq(#panes, 1)
  t.eq(panes[1].pane_id, 1)
  t.eq(panes[1].command, "vim")
  session:close()
end

function scenarios.vvmux_envelope()
  local session = auto.vvmux_connect("work", { timeout = t.TIMEOUT })
  local sent = session:request({ method = "echo", max_bytes = 4096 },
    { pane_id = 3, allow_focused = true }).automation
  -- The verb and its own parameters stay together; the envelope fields ride beside them.
  t.eq(sent.method, "echo")
  t.eq(sent.max_bytes, 4096)
  t.eq(sent.pane_id, 3)
  t.eq(sent.allow_focused, true)
  t.eq(sent.id, 1)
  local second = session:request({ method = "echo" }).automation
  t.eq(second.id, 2)
  t.eq(second.allow_focused, nil)
  session:close()
end

function scenarios.vvmux_version()
  -- A server rebuilt across a preface bump: the first connection learns its version and the
  -- retry speaks it.
  local session = auto.vvmux_connect("bumped", { timeout = t.TIMEOUT })
  t.eq(session:request({ method = "capabilities" }).version, 21)
  session:close()
end

function scenarios.vvmux_refusal()
  local session = auto.vvmux_connect("no", { timeout = t.TIMEOUT })
  local info = t.raises("automation", "no pane here", session.request, session,
    { method = "capture" }, { pane_id = 9 })
  t.eq(info.code, "pane_not_found")
  session:close()
end

function scenarios.vvmux_no_verb()
  local session = auto.vvmux_connect("q", { timeout = t.TIMEOUT })
  t.eq(code(session.request, session, { pane_id = 1 }), "invalid_request")
  local info = t.raises("automation", "belong on the request", session.request, session,
    { method = "capture", pane_id = 1 })
  t.eq(info.code, "invalid_request")
  t.raises("invalid", "unknown request envelope field", session.request, session,
    { method = "capture" }, { pane = 1 })
  session:close()
end

local run = assert(scenarios[scenario], "unknown scenario " .. tostring(scenario))
local ok, err = xpcall(run, function(err)
  return debug.traceback(tostring(err), 2)
end)
if ok then
  io.write("passed\n")
else
  io.write(tostring(err), "\n")
  os.exit(1)
end
