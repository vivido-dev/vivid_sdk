-- Draw an interactive overlay panel and pump its input lane until it is dismissed.
--
-- Unlike examples 01-05 this presents a vector display list rather than a raster: the host
-- shapes the text and rasterizes the paths, so moving the window never reuploads anything.
package.path = (arg[0]:match("^(.*)[/\\]") or ".") .. "/?.lua;" .. package.path
local vivid = require("vivid_sdk")
local support = require("support")

local overlay = vivid.overlay
local Brush, Canvas, Path, point, rect = overlay.Brush, overlay.Canvas, overlay.Path, overlay.point, overlay.rect

local WIDTH, HEIGHT = 320, 180
-- Application-chosen hit region ID. Nonzero and unique within one display list.
local PANEL = 1

local function panel(label)
  local face = Path.rounded_rectangle(rect(0, 0, WIDTH, HEIGHT), 12)
  return Canvas.new()
    :fill(face, Brush.solid(0x203050FF))
    :stroke(face, Brush.solid(0x66CCFFFF), 2)
    -- An empty family asks the host for its default; custom font bytes are not supported.
    :text(label, point(20, 24), 18, 0xFFFFFFFF)
    -- Declaring a region lets the host report which part was pressed; the default hit area is
    -- the whole window rectangle.
    :hit(PANEL, face)
end

local args = support.arguments("Draw an interactive overlay panel and pump its input lane.")
local deadline = args.duration and vivid.monotonic() + args.duration
local session = overlay.connect()
support.finally(function()
  local window = session:create_window({ bounds = rect(40, 40, WIDTH, HEIGHT) })
  support.finally(function()
    window:present(panel("Click the panel, or press Escape."))
    window:center()
    window:request_focus()
    while not deadline or vivid.monotonic() < deadline do
      local event = session:wait_event(0.2)
      if event then
        if event.kind == "connection-lost" then
          print("overlay connection lost: " .. event.diagnostic)
          break
        end
        -- One session can own many windows, so every event names the one it belongs to.
        if event:targets(window) then
          if event.kind == "pointer" and event.application_id == PANEL and event.down then
            break
          end
          if event.kind == "dismissed" then
            break
          end
        end
      end
    end
  end, function()
    window:close()
  end)
end, function()
  session:close()
end)
