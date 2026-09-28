-- raster animation: explicit Vivid 1.5 lifecycle.
package.path = (arg[0]:match("^(.*)[/\\]") or ".") .. "/?.lua;" .. package.path
local vivid = require("vivid_sdk")
local support = require("support")

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
      -- One sequential sender bounds work even when flow control blocks: every send returns
      -- before the next frame is made.
      for frame_id = 2, 90 do
        vivid.sleep(0.034)
        local offset = (frame_id % 4) * 4
        local frame = support.PIXELS:sub(offset + 1) .. support.PIXELS:sub(1, offset)
        channel:send_raster(frame, { frame_id = frame_id })
      end
      channel:eos()
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
