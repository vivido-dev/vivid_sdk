-- surface and track: explicit Vivid 1.5 lifecycle.
package.path = (arg[0]:match("^(.*)[/\\]") or ".") .. "/?.lua;" .. package.path
local vivid = require("vivid_sdk")
local support = require("support")

local args = support.arguments("surface and track: explicit Vivid 1.5 lifecycle.")
local session = vivid.connect()
support.finally(function()
  local surface = session:create_surface({
    logical_width = 2,
    logical_height = 2,
    role = vivid.ROLE_FIGURE,
    title = "SDK raster example",
  })
  support.finally(function()
    -- Lua placement takes ordinary cell numbers, at the top-left of the terminal.
    session:place_terminal_surface(surface, { node_id = 1, width = 16, height = 8 })
    local track = session:create_track(surface, { kind = "raster", width = 2, height = 2 })
    local channel = session:open_track_channel(track)
    support.finally(function()
      channel:send_raster(support.PIXELS, { frame_id = 1 })
      session:wait_track(track, vivid.WAIT_MILESTONE_SET, vivid.MILESTONE_OUTPUT_READY, 5)
      session:activate_track(surface, track)
      channel:eos()
      support.hold(args.duration)
    end, function()
      channel:close()
    end)
  end, function()
    -- Destroying the surface also retires its tracks. Close still runs on failure.
    session:delete_node(surface.context_id, 1)
    session:destroy_surface(surface)
  end)
end, function()
  session:close()
end)
