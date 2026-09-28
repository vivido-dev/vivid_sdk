-- Producer side of Vivido's bounded native binding integration test (not an example).
--
-- The Lua counterpart of overlay_python.py and overlay_node.mjs: the same scene, the same
-- assertions, and Lua's own offsets. Text offsets are UTF-8 byte offsets, so "A😀日" ends at 8
-- where Python counts 3 characters and JavaScript 4 UTF-16 units.

local vivid = require("vivid_sdk")
local overlay = vivid.overlay
local Canvas, Brush, Path, rect, point = overlay.Canvas, overlay.Brush, overlay.Path, overlay.rect, overlay.point

local OPTIONS = { bounds = rect(10, 20, 100, 80) }
local PIXELS = string.char(255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255)
local LABEL = "A\240\159\152\128\230\151\165" -- "A😀日"
local LONG = LABEL .. "e\204\129 long label to truncate" -- ... "é" as e + U+0301
-- The widest application ID this Lua carries exactly: above 2^53 where integers allow it.
local REGION = math.maxinteger and (math.maxinteger - 17) or (2 ^ 53 - 17)

local TEXT = {
  runs = {
    { text = "A\240\159\152\128", style = { size = 18, color = 0xFF0000FF, underline = true } },
    { text = "\230\151\165", style = { size = 20, color = 0x0000FFFF, strikethrough = true } },
  },
  max_width = 90,
  alignment = "center",
}
local ELLIPSIS = {
  runs = { { text = LONG, style = { size = 12, color = 0x00FF00FF } } },
  max_width = 85,
  max_lines = 1,
  overflow = "ellipsis",
  letter_spacing = 0.5,
  word_spacing = 2,
  line_height = 16,
  ligatures = false,
  kerning = false,
}

local function same(a, b)
  if type(a) ~= "table" or type(b) ~= "table" then
    return a == b
  end
  for key, value in pairs(a) do
    if not same(value, b[key]) then
      return false
    end
  end
  for key in pairs(b) do
    if a[key] == nil then
      return false
    end
  end
  return true
end

local function max_end(measured)
  local result = 0
  for _, cluster in ipairs(measured.clusters) do
    result = math.max(result, cluster["end"])
  end
  return result
end

local function check_ellipsis(measured)
  local cut = measured.truncated_at
  assert(cut and cut > 0 and cut < #LONG)
  local byte = LONG:byte(cut + 1)
  -- The cutoff is a character boundary, and not the one before a combining mark.
  assert(byte < 0x80 or byte >= 0xC0, "cut inside a UTF-8 sequence")
  assert(LONG:sub(cut + 1, cut + 2) ~= "\204\129", "cut before a combining mark")
  local marker = false
  for _, cluster in ipairs(measured.clusters) do
    assert(cluster["end"] <= cut)
    marker = marker or (cluster.start == cut and cluster["end"] == cut and cluster.bounds.width > 0)
  end
  assert(marker, "the ellipsis marker is an empty range at the cutoff")
end

local function scene()
  local result = Canvas.new()
  for _, square in ipairs({ { 0, 0, 0xFF0000FF }, { 10, 0, 0x00FF00FF }, { 0, 10, 0x0000FFFF }, { 10, 10, 0xFFFF00FF } }) do
    result:fill(Path.rectangle(rect(square[1], square[2], 10, 10)), Brush.solid(square[3]))
  end
  return result:hit(REGION, Path.rectangle(rect(0, 0, 100, 80)))
end

local function accept(event, window, seen)
  if event.kind == "viewport" and event.viewport.width == 500 then
    assert(event.revision >= 2 and event.scene_revision == 0)
    seen.viewport = true
  end
  if event.kind == "pointer" or event.kind == "ime" then
    assert(event:targets(window) and event.scene_revision == 1)
  end
  if event.kind == "pointer" then
    assert(event.application_id == REGION)
    seen.pointer = true
  end
  if event.kind == "ime" then
    assert(event.preedit == LABEL)
    assert(event.selection.start == 1 and event.selection["end"] == 5, "UTF-8 byte offsets")
    seen.ime = true
  end
  return seen.viewport and seen.pointer and seen.ime
end

local function fails(fn, ...)
  assert(not pcall(fn, ...), "expected a refusal")
end

local session = overlay.connect()
local window = session:create_window(OPTIONS)
window:center()
window:set_bounds(OPTIONS.bounds)
window:set_visible(false)
window:set_visible(true)
window:raise()
window:lower()
window:request_focus()
assert(same(window:bounds(), OPTIONS.bounds))
assert(window:viewport().width == 400)
local measured = window:measure_text(LABEL, 18)
local batch = window:measure_text_batch({ TEXT, TEXT })
local layout = window:layout_text_batch({ TEXT })[1]
local ellipsis = window:layout_text(ELLIPSIS)
check_ellipsis(ellipsis.measurement)
assert(same(window:measure_text_batch({ ELLIPSIS })[1], ellipsis.measurement))
assert(same(batch[1], batch[2]) and same(batch[1], layout.measurement))
assert(max_end(layout.measurement) == #LABEL)
assert(measured.width > 0 and measured.height > 0 and #measured.lines > 0)
assert(max_end(measured) == #LABEL)

local canvas = scene()
window:draw_text_layout(canvas, layout, point(0, 40))
window:draw_text_layout(canvas, ellipsis, point(0, 22))
local image = window:upload_rgba(2, 2, PIXELS)
window:draw_image(canvas, image, rect(50, 0, 20, 20))
local receipt = window:submit(canvas)
assert(receipt.revision == 1 and receipt:wait(10) == "presented")
window:release_image(image)
fails(window.submit, window, canvas)

local seen, deadline = {}, vivid.monotonic() + 10
for event in session:events() do
  if accept(event, window, seen) then
    break
  end
  assert(vivid.monotonic() < deadline, "input events did not arrive")
end
session:capture_pointer(window)
session:capture_pointer(window, false)
window:set_editor_geometry(receipt.revision, rect(5, 6, 1, 18))
window:set_editor_geometry(receipt.revision, nil)

assert(io.read("*l") == "release")
local pending = window:submit(Canvas.new())
local replacement_scene = scene()
window:draw_text_layout(replacement_scene, layout, point(0, 40))
window:draw_text_layout(replacement_scene, ellipsis, point(0, 22))
local replacement = window:replace_track(replacement_scene)
assert(pending:wait(5) == "superseded" and replacement.revision == 3)
local status = window:reconcile()
assert(status.active_revision == 3 and status.accepted_revision == 3)
assert(replacement:wait(10) == "presented")
assert(window:reconcile().presented_revision == 3)
window:release_text_layout(layout)
window:release_text_layout(ellipsis)
fails(window.submit, window, replacement_scene)
local popup = session:create_window({ bounds = rect(40, 40, 20, 20), mode = "popup" }, window)
popup:present(Canvas.new())
popup:close()
window:present(scene())
window:close()
session:close()
