-- Print this SDK's canonical conformance report.
--
-- Runs the same scenarios every binding runs and prints the same report, so the comparison is
-- between languages rather than between expectations. Run with the staged module on the C path:
--
--   LUA_CPATH="lua/?.so;;" luajit conformance/scenario.lua

local vivid = require("vivid_sdk")

local PIXELS = string.char(255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255)
local PANE = 1

local function constants()
  local values = {}
  for _, entry in ipairs(vivid.constant_table()) do
    values[entry.name] = vivid[entry.name]
  end
  return values
end

local function raster()
  local running = vivid.presenter.start("tcp:127.0.0.1:0")
  local ok, result = pcall(function()
    local capability = running:issue_pane_capability(PANE)
    running:update_metrics(PANE, { columns = 80, rows = 24, cell_width = 8, cell_height = 16 })
    local pane = vivid.PaneSession.connect({
      endpoint_control = running.endpoint,
      root_secret = capability,
      target_profile = vivid.PROFILE_TERMINAL_SURFACE,
    })
    local captured, err = pcall(function()
      pane:show_rgba(2, 2, PIXELS)
      local retained = running:wait_for_media(PANE, 5)
      local capture = running:capture_pane(PANE)
      local layer = capture.layers[1]
      -- The report carries the pixels, so the comparison is over bytes, not a summary of them.
      local kind, pixels = "none", {}
      if layer and layer.content.kind == "raster" then
        kind = "raster"
        pixels = { layer.content.rgba:byte(1, -1) }
      elseif layer then
        kind = "encodedImage"
      end
      return {
        retained = retained,
        layers = #capture.layers,
        skipped = #capture.skipped,
        contentKind = kind,
        pixels = pixels,
      }
    end)
    pane:close()
    if not captured then
      error(err, 0)
    end
    return err
  end)
  running:close()
  if not ok then
    error(result, 0)
  end
  return result
end

local function validation()
  local session = vivid.connect({ dry_run = true })
  local surface = session:create_surface({ logical_width = 2, logical_height = 2 })
  local result = {}
  for _, case in ipairs({ { "zeroRasterWidth", 0 }, { "oversizedRasterWidth", 8193 } }) do
    local ok, err = pcall(session.create_track, session, surface,
      { kind = "raster", width = case[2], height = 2 })
    result[case[1]] = not ok and vivid.error_info(err) ~= nil
  end
  session:close()
  return result
end

-- JSON with sorted keys. Integers print exactly on every Lua, including those whose numbers are
-- doubles; an empty table is an array, which is what the pixel list is when there are none.
local function encode(value)
  local kind = type(value)
  if kind == "boolean" then
    return tostring(value)
  elseif kind == "number" then
    if math.type and math.type(value) == "integer" then
      return tostring(value)
    end
    return value % 1 == 0 and string.format("%.0f", value) or string.format("%.17g", value)
  elseif kind == "string" then
    return '"' .. value:gsub('[%c"\\]', function(c)
      return string.format("\\u%04x", c:byte())
    end) .. '"'
  end
  if next(value) == nil or #value > 0 then
    local parts = {}
    for index = 1, #value do
      parts[index] = encode(value[index])
    end
    return "[" .. table.concat(parts, ",") .. "]"
  end
  local keys = {}
  for key in pairs(value) do
    keys[#keys + 1] = key
  end
  table.sort(keys)
  local parts = {}
  for index, key in ipairs(keys) do
    parts[index] = encode(key) .. ":" .. encode(value[key])
  end
  return "{" .. table.concat(parts, ",") .. "}"
end

io.write(encode({ constants = constants(), raster = raster(), validation = validation() }), "\n")
