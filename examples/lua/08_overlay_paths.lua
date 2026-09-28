-- Draw freeform paths: curves, an even-odd hole, and a stroke with caps, joins and dashes.
--
-- Example 07 used the shape constructors - rectangles, rounded rectangles, ellipses - and those
-- cover a user interface. Everything else is a path you build: a chart, a map, a signature, a
-- gauge. The four segment kinds compose into all of it, and the even-odd rule is what puts a hole
-- in a shape rather than a second layer over it.
--
-- The same builder exists in every language, with the same segment kinds and the same options,
-- so the geometry below is written the same way in each.
package.path = (arg[0]:match("^(.*)[/\\]") or ".") .. "/?.lua;" .. package.path
local vivid = require("vivid_sdk")
local support = require("support")

local overlay = vivid.overlay
local Brush, Canvas, Path, point, rect = overlay.Brush, overlay.Canvas, overlay.Path, overlay.point, overlay.rect

local WIDTH, HEIGHT = 400, 220
-- Application-chosen hit region ID. Nonzero and unique within one display list.
local CANVAS = 1
-- The circle constant, for the cubic approximation of a round shape.
local KAPPA = 0.5522847498307936

-- A circle as four cubics, appended to a path that may already have subpaths. A ring needs two of
-- these in one builder, which is what a builder is for. Path.ellipse does the same arithmetic for
-- a whole ellipse.
local function circle(path, cx, cy, radius)
  local k = radius * KAPPA
  return path:move_to(cx, cy - radius)
    :cubic_to(cx + k, cy - radius, cx + radius, cy - k, cx + radius, cy)
    :cubic_to(cx + radius, cy + k, cx + k, cy + radius, cx, cy + radius)
    :cubic_to(cx - k, cy + radius, cx - radius, cy + k, cx - radius, cy)
    :cubic_to(cx - radius, cy - k, cx - k, cy - radius, cx, cy - radius)
    :close()
end

-- A star, filled: ten corners alternating between two radii, walked with line_to.
local function star(cx, cy, radius)
  local path = Path.new()
  for corner = 0, 9 do
    local reach = corner % 2 == 0 and radius or radius * 0.45
    local angle = -math.pi / 2 + corner * math.pi / 5
    local x, y = cx + reach * math.cos(angle), cy + reach * math.sin(angle)
    if corner == 0 then
      path:move_to(x, y)
    else
      path:line_to(x, y)
    end
  end
  return path:close()
end

-- The whole scene: a filled star, a curved stroke, a ring with a hole, and a dashed rule.
local function scene(label)
  local face = Path.rounded_rectangle(rect(0, 0, WIDTH, HEIGHT), 10)
  -- A filled star: ten corners, alternating radii, one line_to each.
  local badge = star(58, 72, 34)
  -- One cubic through two control points, stroked with round caps and a round join. The caps
  -- are why the ends are not cut off square.
  local curve = Path.new():move_to(108, 96):cubic_to(150, 20, 220, 130, 262, 46)
  -- A ring: two circles in one path, with the even-odd rule. The inner one is a hole rather than
  -- a second disc, so the background shows through it - and the host hit tests the rule it fills
  -- by, so the hole is not part of the region either.
  local ring = circle(circle(Path.new({ even_odd = true }), 316, 72, 34), 316, 72, 16)
  -- A dashed line: the dashes belong to the stroke, not to the path, so the path is two points.
  local rule = Path.new():move_to(24, 158):line_to(WIDTH - 24, 158)
  return Canvas.new()
    :fill(face, Brush.solid(0x181828FF))
    :fill(badge, Brush.solid(0xE0B050FF))
    :stroke_styled(curve, Brush.solid(0x8ECBFFFF), { width = 5, cap = "round", join = "round" })
    :fill(ring, Brush.solid(0x70D090FF))
    :stroke_styled(rule, Brush.solid(0xFF8EA0FF), { width = 3, dashes = { 10, 6 } })
    -- An empty family asks the host for its default; custom font bytes are not supported.
    :text(label, point(24, 180), 15, 0xC0C0D0FF)
    -- Declaring a region lets the host report which part was pressed; the default hit area is
    -- the whole window rectangle.
    :hit(CANVAS, face)
end

local args = support.arguments("Draw freeform paths: curves, an even-odd hole, and dashes.")
local deadline = args.duration and vivid.monotonic() + args.duration
local session = overlay.connect()
support.finally(function()
  local window = session:create_window({ bounds = rect(40, 40, WIDTH, HEIGHT) })
  support.finally(function()
    window:present(scene("star, curve, ring, dashes - click or press Escape"))
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
          if event.kind == "pointer" and event.application_id == CANVAS and event.down then
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
