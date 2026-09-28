-- The automation client, against fake servers that speak the real wire.
--
-- Lua has no sockets of its own, so the servers are `fake_automation_server.py` (Python standard
-- library); these tests skip where no Python is available. Lua cannot set environment variables
-- either, so the scenarios that resolve an endpoint through `XDG_RUNTIME_DIR` and
-- `VIVIDO_SESSION` run `automation_child.lua` in a child interpreter with a controlled
-- environment, against real registry files in a real (temporary) runtime directory.

local vivid = require("vivid_sdk")
local t = require("support")

local auto = vivid.automation
local q = t.shell_quote

local tests = {}
local function test(name, fn)
  tests[#tests + 1] = { name, fn }
end

local here = (arg and arg[0] or ""):match("^(.*)[/\\]") or "."
local python = t.python()

-- The interpreter running this suite, so the child runs on the same Lua.
local interpreter = (function()
  local index = -1
  while arg and arg[index - 1] do
    index = index - 1
  end
  return arg and arg[index] or "lua"
end)()

local function need_unix_and_python()
  if package.config:sub(1, 1) ~= "/" then
    t.skip("the automation client is Unix only")
  end
  if not python then
    t.skip("no python3 for the fake servers; set VIVID_TEST_PYTHON")
  end
end

-- A runtime root holding owner-only `vivido` and `vvmux` directories, as the runtimes make them.
local function runtime_root()
  local root = t.tempdir()
  assert(t.execute("mkdir -m 700 " .. q(root .. "/vivido") .. " " .. q(root .. "/vvmux")))
  return root
end

-- Start a fake server and return a function that stops it.
local function serve(root, config)
  config.stop = root .. "/stop"
  local path = root .. "/server.json"
  t.write_file(path, t.json(config))
  local handle = assert(io.popen(q(python) .. " " .. q(here .. "/fake_automation_server.py")
    .. " " .. q(path), "r"))
  local line = handle:read("*l")
  if line ~= "ready" then
    handle:close()
    error("the fake server did not start: " .. tostring(line), 2)
  end
  return function()
    t.write_file(config.stop, "")
    handle:read("*a")
    handle:close()
  end
end

-- Run one client scenario in a child interpreter with a controlled environment.
local function child(root, scenario, environment)
  local command = "env -u VIVIDO_SESSION -u VIVIDO_SOCKET XDG_RUNTIME_DIR=" .. q(root)
    .. " " .. (environment or "") .. " " .. q(interpreter) .. " "
    .. q(here .. "/automation_child.lua") .. " " .. q(scenario) .. " " .. q(root) .. " 2>&1"
  local handle = assert(io.popen(command, "r"))
  local output = handle:read("*a")
  handle:close()
  if not output:find("passed\n$") then
    error("scenario " .. scenario .. " failed:\n" .. output, 0)
  end
end

local function scenario(name, config, environment)
  test(name, function()
    need_unix_and_python()
    local root = runtime_root()
    local stop = config and serve(root, config(root))
    local ok, err = pcall(child, root, name, environment)
    if stop then
      stop()
    end
    t.remove_tree(root)
    if not ok then
      error(err, 0)
    end
  end)
end

-- In-process tests connect to an explicit socket, which needs no runtime directory.
local function with_server(config, fn)
  need_unix_and_python()
  local root = runtime_root()
  config.socket = root .. "/s.sock"
  local stop = serve(root, config)
  local ok, err = pcall(fn, config.socket, root)
  stop()
  t.remove_tree(root)
  if not ok then
    error(err, 0)
  end
end

test("hello and a request round trip", function()
  with_server({ mode = "ndjson", capabilities = { methods = { "inspect" } },
    methods = { inspect = { window = { sequences = { screen = 3 } } } } }, function(socket)
    local session = auto.vivido_connect({ socket = socket, timeout = t.TIMEOUT })
    t.eq(session.capabilities.methods[1], "inspect")
    t.eq(session:request("inspect", { window_id = 1 }).window.sequences.screen, 3)
    session:close()
    t.ok(session.closed)
    t.raises("closed", nil, session.request, session, "inspect")
  end)
end)

test("an interleaved event frame does not answer a request", function()
  with_server({ mode = "ndjson", interleave = true, methods = { typing = { accepted = true } } },
    function(socket)
      local session = auto.vivido_connect({ socket = socket, timeout = t.TIMEOUT })
      t.eq(session:request("typing", { text = "x" }).accepted, true)
      session:close()
    end)
end)

test("a refused request raises a typed error", function()
  with_server({ mode = "ndjson", errors = { inspect = { code = "window_not_found",
    message = "no window 9", data = { window_id = 9 } } } }, function(socket)
    local session = auto.vivido_connect({ socket = socket, timeout = t.TIMEOUT })
    local info = t.raises("automation", "no window 9", session.request, session, "inspect",
      { window_id = 9 })
    t.eq(info.code, "window_not_found")
    t.eq(info.data.window_id, 9)
    session:close()
  end)
end)

test("a bad method or bad params never reach the wire", function()
  with_server({ mode = "ndjson" }, function(socket)
    local session = auto.vivido_connect({ socket = socket, timeout = t.TIMEOUT })
    t.eq(t.raises("automation", nil, session.request, session, "bad-name").code, "invalid_request")
    t.eq(t.raises("automation", nil, session.request, session, "").code, "invalid_request")
    t.eq(t.raises("automation", nil, session.request, session, "typing", { 1, 2 }).code,
      "invalid_request")
    t.raises("invalid", "JSON", session.request, session, "typing", { callback = print })
    session:close()
  end)
end)

test("arrays, nulls, and nesting cross the wire as JSON", function()
  with_server({ mode = "ndjson" }, function(socket)
    local session = auto.vivido_connect({ socket = socket, timeout = t.TIMEOUT })
    local echoed = session:request("echo", {
      mods = auto.array(),
      keys = { "a", "b" },
      nothing = auto.null,
      target = { window_id = 1 },
    })
    t.eq(echoed.method, "echo")
    t.eq(next(echoed.params.mods), nil, "an empty array")
    t.eq(getmetatable(echoed.params.mods), getmetatable(auto.array()), "still an array")
    t.eq(echoed.params.keys[2], "b")
    t.eq(echoed.params.nothing, nil, "a JSON null reads back as nil")
    t.eq(echoed.params.target.window_id, 1)
    t.eq(echoed.version, auto.PROTOCOL_VERSION)
    session:close()
  end)
end)

test("a symlinked or missing socket is declined before connecting", function()
  need_unix_and_python()
  local root = runtime_root()
  assert(t.execute("ln -s " .. q(root .. "/real.sock") .. " " .. q(root .. "/link.sock")))
  local info = t.raises("automation", nil, auto.vivido_connect, { socket = root .. "/link.sock" })
  t.eq(info.code, "endpoint_unsafe")
  info = t.raises("automation", nil, auto.vivido_connect, { socket = root .. "/absent.sock" })
  t.eq(info.code, "endpoint_not_found")
  t.remove_tree(root)
end)

test("session names follow the runtime rule", function()
  for _, name in ipairs({ "", ".a", string.rep("a", 65), "sp ace", "sl/ash" }) do
    local info = t.raises("automation", nil, auto.validate_session_name, name)
    t.eq(info.code, "invalid_session_name")
  end
  t.eq(auto.validate_session_name("dev-1.2_a"), "dev-1.2_a")
  t.eq(t.raises("automation", nil, auto.vvmux_connect, "bad name").code, "invalid_session_name")
end)

local function vivido_session(name, extra)
  return function(root)
    local config = {
      mode = "ndjson",
      session = { root = root, product = "vivido", name = name },
      capabilities = { methods = {} },
      registries = { { root = root, name = name } },
    }
    for key, value in pairs(extra or {}) do
      config[key] = value
    end
    return config
  end
end

scenario("named_target", vivido_session("scratch"))

scenario("registry_points_elsewhere", function(root)
  return { mode = "ndjson", socket = root .. "/other.sock",
    registries = { { root = root, name = "scratch", socket = root .. "/other.sock" } } }
end)

scenario("stale_registry", function(root)
  return { mode = "ndjson", socket = root .. "/unused.sock",
    registries = { { root = root, name = "scratch", live = false } } }
end)

scenario("gone_target", vivido_session("other"))
scenario("gone_session_environment", vivido_session("other"), "VIVIDO_SESSION=missing")
scenario("sole_instance", vivido_session("only"))

scenario("instances", function(root)
  return {
    mode = "ndjson",
    session = { root = root, product = "vivido", name = "alive" },
    registries = {
      { root = root, name = "alive" },
      { root = root, name = "gone", live = false },
      -- Alive, but its socket does not match its name, so it is not an instance.
      { root = root, name = "liar", socket = root .. "/other-place.sock" },
    },
  }
end)

scenario("unsafe_runtime", nil)

local function vvmux_session(name, extra)
  return function(root)
    local config = { mode = "vvmx", session = { root = root, product = "vvmux", name = name } }
    for key, value in pairs(extra or {}) do
      config[key] = value
    end
    return config
  end
end

scenario("vvmux_round_trip", vvmux_session("default",
  { methods = { list_panes = { { pane_id = 1, command = "vim" } } } }))
scenario("vvmux_envelope", vvmux_session("work"))
scenario("vvmux_version", vvmux_session("bumped",
  { version = auto.VVMX_VERSION + 1, connections = 2, methods = { capabilities = { version = 21 } } }))
scenario("vvmux_refusal", vvmux_session("no", { refuse = true }))
scenario("vvmux_no_verb", vvmux_session("q"))

return tests
