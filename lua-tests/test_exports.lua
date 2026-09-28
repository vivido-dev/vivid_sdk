-- The module, its type definitions, and the Rust constant table agree.
--
-- Values are never copied into Lua: the module reads every constant from the table at load time,
-- and `lua/types/vivid_sdk.lua` declares names and types only. These checks are what keep the
-- declarations honest when the table or the binding changes.

local vivid = require("vivid_sdk")
local t = require("support")

local tests = {}
local function test(name, fn)
  tests[#tests + 1] = { name, fn }
end

local here = (arg and arg[0] or ""):match("^(.*)[/\\]") or "."
local root = here .. "/.."

local function source(path)
  return t.read_file(root .. "/" .. path)
end

test("every table constant is a module field with the table's value", function()
  local count = 0
  for _, entry in ipairs(vivid.constant_table()) do
    count = count + 1
    local expected = entry.text or entry.number
    t.eq(vivid[entry.name], expected, entry.name)
    t.ok((entry.text == nil) ~= (entry.number == nil), entry.name .. " is text or a number")
  end
  t.ok(count > 90, "the constant table is complete")
end)

test("the type definitions declare exactly the table's constants", function()
  local text = source("lua/types/vivid_sdk.lua")
  local block = text:match("\n%-%-%-@class vivid_sdk\n(.-)\nlocal vivid = {}")
  t.ok(block, "the module class block")
  local declared = {}
  for name, kind in block:gmatch("%-%-%-@field ([%u%d_]+) (%a+)") do
    if name ~= "VERSION" then
      declared[name] = kind
    end
  end
  for _, entry in ipairs(vivid.constant_table()) do
    local kind = entry.text and "string" or "integer"
    t.eq(declared[entry.name], kind, "declared type of " .. entry.name)
    declared[entry.name] = nil
  end
  t.eq(next(declared), nil, "a declared constant the table does not have")
end)

-- Method names the binding registers, read from its Rust sources.
local function registered()
  local names = {}
  for _, file in ipairs({
    "automation", "desktop", "file_drop", "input", "lease", "overlay",
    "pane", "pipeline", "presenter", "session",
  }) do
    local text = source("lua-bindings/src/" .. file .. ".rs")
    for name in text:gmatch('add_method[%w_]*%(%s*"([%w_]+)"') do
      names[name] = true
    end
    for name in text:gmatch('add_function%(%s*"([%w_]+)"') do
      names[name] = true
    end
  end
  return names
end

test("every registered method is declared, and every declared method is registered", function()
  local text = source("lua/types/vivid_sdk.lua")
  local declared = {}
  for class, name in text:gmatch("\nfunction ([%w_]+):([%w_]+)%(") do
    declared[name] = class
  end
  local names = registered()
  for name in pairs(names) do
    t.ok(declared[name], "method " .. name .. " is registered but not declared")
  end
  for name, class in pairs(declared) do
    -- `Event:targets` is a function field of an event table rather than a userdata method.
    t.ok(names[name] or (class == "Event" and name == "targets"),
      class .. ":" .. name .. " is declared but not registered")
  end
end)

test("declared methods exist on live handles", function()
  local text = source("lua/types/vivid_sdk.lua")
  local session = vivid.connect({ dry_run = true })
  local surface = session:create_surface({ logical_width = 1, logical_height = 1 })
  local track = session:create_track(surface, { kind = "raster", width = 1, height = 1 })
  local handles = {
    Session = session,
    TrackChannel = session:open_track_channel(track),
    VideoRateControl = vivid.VideoRateControl.new(1000000),
    PaneSession = vivid.PaneSession.connect({ dry_run = true }),
    Presenter = vivid.presenter.start("tcp:127.0.0.1:0"),
    Path = vivid.overlay.Path.new(),
    Canvas = vivid.overlay.Canvas.new(),
  }
  for class, name in text:gmatch("\nfunction ([%w_]+):([%w_]+)%(") do
    local handle = handles[class]
    if handle then
      t.eq(type(handle[name]), "function", class .. ":" .. name)
    end
  end
  handles.PaneSession:close()
  handles.Presenter:close()
  session:close()
end)

test("submodules are reachable through require and the root", function()
  t.eq(require("vivid_sdk.presenter"), vivid.presenter)
  t.eq(require("vivid_sdk.overlay"), vivid.overlay)
  t.eq(require("vivid_sdk.automation"), vivid.automation)
  t.eq(vivid.overlay.Modifiers.SHIFT, 1)
  t.eq(vivid.overlay.MouseButton.MAXIMUM, 31)
  t.eq(vivid.overlay.Key.LAST_USAGE, 0xE7)
  t.eq(vivid.presenter.SKIP_NO_RETAINED_PIXELS, "no_retained_pixels")
end)

return tests
