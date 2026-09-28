-- Verify retained pixels through a real loopback presenter; no terminal required.
package.path = (arg[0]:match("^(.*)[/\\]") or ".") .. "/?.lua;" .. package.path
local vivid = require("vivid_sdk")
local support = require("support")

local running = vivid.presenter.start("tcp:127.0.0.1:0")
support.finally(function()
  running:update_metrics(1, { columns = 80, rows = 24, cell_width = 8, cell_height = 16 })
  -- Hand the capability directly to the producer; never log it or put it in argv.
  local capability = running:issue_pane_capability(1)
  local pane = vivid.PaneSession.connect({
    endpoint_control = running.endpoint,
    root_secret = capability,
    target_profile = vivid.PROFILE_TERMINAL_SURFACE,
  })
  support.finally(function()
    pane:show_rgba(2, 2, support.PIXELS)
    assert(running:wait_for_media(1, 5), "timed out waiting for retained pixels")
    local capture = running:capture_pane(1)
    assert(#capture.layers == 1 and #capture.skipped == 0, "expected exactly one retained layer")
    local frame = capture.layers[1].content
    assert(frame.kind == "raster", "expected raster content")
    assert(frame.width == 2 and frame.height == 2 and frame.rgba == support.PIXELS,
      "captured dimensions or RGBA pixels differ")
    print("Verified one 2 x 2 raster with exact red, green, blue, white pixels.")
  end, function()
    pane:close()
  end)
end, function()
  running:close()
end)
