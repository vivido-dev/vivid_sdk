-- Overlay canvases and windows, against the SDK's offline contract.

local vivid = require("vivid_sdk")
local t = require("support")

local overlay = vivid.overlay
local Path, Brush, Canvas = overlay.Path, overlay.Brush, overlay.Canvas
local rect, point = overlay.rect, overlay.point

local tests = {}
local function test(name, fn)
  tests[#tests + 1] = { name, fn }
end

-- The widest application ID this Lua carries exactly.
local BIG = math.maxinteger or (2 ^ 53 - 1)

local function drawing()
  local path = Path.new():move_to(0, 0):line_to(50, 0):quad_to(60, 20, 50, 40)
    :cubic_to(30, 60, 10, 60, 0, 40):close()
  local stops = { { offset = 0, color = 0xFF0000FF }, { offset = 1, color = 0x0000FFFF } }
  return Canvas.new()
    :save()
    :clip(Path.rounded_rectangle(rect(0, 0, 100, 80), 8))
    :transform(1, 0, 0, 1, 2, 3)
    :opacity(0.75)
    :fill(path, Brush.linear(point(0, 0), point(60, 40), stops))
    :stroke(Path.ellipse(rect(5, 5, 40, 20)), Brush.radial(point(10, 10), 30, stops), 2)
    :text("A\240\159\152\128\230\151\165\230\156\172\232\170\158", point(4, 60), 12, 0xFFFFFFFF)
    :hit(BIG, Path.rectangle(rect(0, 0, 100, 80)))
    :restore()
end

local function with_session(fn)
  local session = overlay.connect({ dry_run = true })
  local ok, err = pcall(fn, session)
  session:close()
  if not ok then
    error(err, 0)
  end
end

test("canvas ownership, receipts, and session cleanup", function()
  local session = overlay.connect({ dry_run = true })
  local other = overlay.connect({ dry_run = true })
  local options = { bounds = rect(10, 20, 100, 80) }
  local window, neighbor = session:create_window(options), session:create_window(options)
  t.fails(other.create_window, other, options, window)
  local image = window:upload_rgba(1, 1, "\255\0\0\255")
  t.ok(image.id > 0)
  local canvas = drawing()
  canvas:validate()
  window:draw_image(canvas, image, rect(2, 2, 10, 10))
  t.fails(neighbor.draw_image, neighbor, Canvas.new(), image, rect(0, 0, 10, 10))
  window:present(canvas)
  window:present(canvas:snapshot())
  local receipt = window:submit(canvas:snapshot())
  t.eq(receipt.revision, 3)
  t.eq(receipt:wait(0), nil)
  t.raises("invalid", "60 seconds", receipt.wait, receipt, 61)
  window:release_image(image)
  t.fails(window.submit, window, canvas)
  t.fails(window.release_image, window, image)
  t.fails(window.upload_rgba, window, 2, 2, "bad")
  window:close()
  t.ok(window.closed)
  t.raises("closed", "overlay window is closed", window.present, window, canvas)
  t.fails(neighbor.present, neighbor, canvas)
  neighbor:present(drawing())
  session:close()
  t.ok(session.closed)
  t.fails(receipt.wait, receipt, 0)
  t.fails(neighbor.present, neighbor, canvas)
  t.eq(session:wait_event(0), nil, "a closed session has no events")
  other:close()
end)

test("invalid scenes and numeric boundaries", function()
  local canvas = Canvas.new()
  t.fails(Path.rectangle, rect(0, 0, 0 / 0, 1))
  t.raises("invalid", nil, canvas.opacity, canvas, 1.1)
  t.raises("invalid", "must be a non-negative integer", canvas.hit, canvas, -1,
    Path.rectangle(rect(0, 0, 1, 1)))
  local unbalanced = Canvas.new():restore()
  t.raises("invalid", nil, unbalanced.validate, unbalanced)
  local valid = drawing()
  local snapshot = valid:snapshot()
  valid:restore()
  snapshot:validate()
  t.raises("invalid", nil, valid.validate, valid)
  t.raises("invalid", nil, Brush.solid, -1)
  t.raises("invalid", nil, Brush.solid, 2 ^ 32)
  t.raises("invalid", "unknown rectangle field", Path.rectangle, { x = 0, y = 0, w = 1, h = 1 })
  t.raises("invalid", "overlay Path", canvas.fill, canvas, "not a path", Brush.solid(0xFFFFFFFF))
  -- A handle of the wrong type is the same class of mistake.
  t.raises("invalid", nil, canvas.fill, Path.new(), Path.new(), Brush.solid(0xFFFFFFFF))
end)

test("a path reports the first coordinate the wire cannot carry when used", function()
  local broken = Path.new():move_to(0, 0):line_to(1 / 0, 0):line_to(1, 1)
  -- The refused segment is not added; the builder remembers the failure for when it is used.
  t.eq(broken.length, 2)
  local canvas = Canvas.new()
  t.raises("invalid", nil, canvas.fill, canvas, broken, Brush.solid(0xFFFFFFFF))
  t.raises("invalid", nil, canvas.fill, canvas, Path.new(), Brush.solid(0xFFFFFFFF))
  t.eq(canvas.length, 0, "a refused command is not added")
  -- A shape is a path like any other, and can be extended.
  local extended = Path.rectangle(rect(0, 0, 4, 4)):move_to(1, 1):line_to(2, 2):close()
  Canvas.new():fill(extended, Brush.solid(0xFFFFFFFF)):validate()
  -- Two nested subpaths under the even-odd rule make a ring.
  local ring = Path.new({ even_odd = true })
  for _, radius in ipairs({ 4, 2 }) do
    ring:move_to(4 + radius, 4):quad_to(4 + radius, 4 + radius, 4, 4 + radius)
      :quad_to(4 - radius, 4 + radius, 4 - radius, 4):quad_to(4 - radius, 4 - radius, 4, 4 - radius)
      :quad_to(4 + radius, 4 - radius, 4 + radius, 4):close()
  end
  t.eq(ring.length, 12)
  Canvas.new():fill(ring, Brush.solid(0xFFFFFFFF)):validate()
end)

test("paint commands validate and round-trip through a scene", function()
  with_session(function(session)
    local window = session:create_window({ bounds = rect(0, 0, 200, 120) })
    local path = Path.rectangle(rect(0, 0, 200, 120))
    local image = window:upload_rgba(2, 2, string.rep("\0", 16))
    local canvas = Canvas.new()
      :shadow({ rect = rect(10, 10, 100, 60), radii = { 4, 8, 12, 16 }, color = 0x00000055,
        offset = point(0, 6), blur = 18, spread = -2 })
      :fill(path, Brush.image(image, nil, "repeat"))
      :fill(path, Brush.linear(point(0, 0), point(200, 0),
        { { offset = 0, color = 0xFF0000FF }, { offset = 1, color = 0x0000FFFF } }, "oklab"))
      :stroke_styled(path, Brush.solid(0xFFFFFFFF),
        { width = 2.5, cap = "round", join = "bevel", miter_limit = 6, dashes = { 4, 2 }, dash_offset = 1.5 })
      :fill(Path.rounded_rectangle_corners(rect(0, 0, 80, 40), { 2, 6, 10, 14 }), Brush.solid(0x00FF00FF))
    canvas:validate()
    t.ok(window:submit(canvas).revision > 0)
    for _, bad in ipairs({
      { rect = rect(0, 0, 10, 10), radii = { -1, 0, 0, 0 } },
      { rect = rect(0, 0, 10, 10), blur = 4097 },
      { rect = rect(0, 0, 10, 10), blur = -1 },
    }) do
      t.fails(function()
        Canvas.new():shadow(bad):validate()
      end)
    end
    for _, bad in ipairs({
      { width = 0 },
      { width = 1, miter_limit = 0.5 },
      { width = 1, dashes = { 0 } },
      { width = 1, dashes = { 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
        1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1 } },
      { width = 1, dash_offset = -1 },
      { width = 1, cap = "pointed" },
    }) do
      t.fails(function()
        Canvas.new():stroke_styled(path, Brush.solid(0xFFFFFFFF), bad):validate()
      end)
    end
    t.fails(Brush.linear, point(0, 0), point(1, 0), { { offset = 2, color = 0xFFFFFFFF } })
    t.fails(Brush.linear, point(0, 0), point(1, 0), {}, "cmyk")
    t.fails(Brush.image, image, { 1, 0, 0 }, "pad")
  end)
end)

test("region cursors", function()
  local path = Path.rectangle(rect(0, 0, 100, 80))
  Canvas.new():hit(1, path):validate()
  Canvas.new():hit(2, path, "input", { cursor = "text" }):validate()
  Canvas.new():hit(3, path, "drag", { cursor = "grabbing" }):validate()
  Canvas.new():hit(4, path, "resize", { edges = 8, cursor = "resize-up-left" }):validate()
  local canvas = Canvas.new()
  t.raises("invalid", "unknown cursor shape", canvas.hit, canvas, 5, path, "input", { cursor = "wand" })
  t.raises("invalid", "unknown hit role", canvas.hit, canvas, 6, path, "poke")
  t.raises("invalid", "unknown hit option field", canvas.hit, canvas, 7, path, "input", { edge = 1 })
end)

test("a clipboard write is refused without a gesture", function()
  with_session(function(session)
    local window = session:create_window({ bounds = rect(0, 0, 100, 80) })
    t.fails(window.set_clipboard, window, "text")
  end)
end)

test("semantics validate locally", function()
  with_session(function(session)
    local window = session:create_window({ bounds = rect(0, 0, 100, 80) })
    local node = { id = 1, role = "group", bounds = rect(0, 0, 10, 10) }
    t.fails(window.set_semantics, window, { scene_revision = 1, nodes = { node } })
    t.fails(window.set_semantics, window, { scene_revision = 1,
      nodes = { { id = 1, role = "group", bounds = rect(0, 0, 10, 10), children = { 0 } } } })
    t.fails(window.set_semantics, window, { scene_revision = 1,
      nodes = { { id = 0, role = "group", bounds = rect(0, 0, 10, 10) } } })
    t.fails(window.set_semantics, window, { scene_revision = 1,
      nodes = { { id = 1, role = "group", bounds = rect(0, 0, 10, 10), set = { 3, 2 } } } })
    t.raises("invalid", "unknown semantic role", window.set_semantics, window, { scene_revision = 1,
      nodes = { { id = 1, role = "gizmo", bounds = rect(0, 0, 10, 10) } } })
  end)
end)

test("styled text is validated before any request", function()
  with_session(function(session)
    local window = session:create_window({ bounds = rect(0, 0, 100, 80) })
    local function measure(text)
      return window:measure_text_batch({ text })
    end
    for _, invalid in ipairs({
      { runs = {} },
      { runs = { { text = string.rep("x", 4097) } } },
      { runs = { { text = "x" } }, max_lines = 0 },
      { runs = { { text = "x" } }, overflow = "ellipsis" },
      { runs = { { text = "x" } }, letter_spacing = -1 },
      { runs = { { text = "x" } }, line_height = 0 },
      { runs = { { text = "x" } }, word_spacing = 1 / 0 },
      { runs = { { text = "x", style = { size = 0 / 0 } } } },
      { runs = { { text = "x" } }, alignment = "middle" },
      { runs = { { text = "x", style = { colour = 1 } } } },
    }) do
      t.raises("invalid", nil, measure, invalid)
    end
  end)
end)

test("window operations need a host, and local options are checked first", function()
  -- Geometry and text come from the host; the offline contract has neither, so it says so. The
  -- live checks are Vivido's native binding test, which runs bindings/tests/overlay_lua.lua.
  with_session(function(session)
    local window = session:create_window({ bounds = rect(4, 5, 100, 80), title = "panel",
      min_width = 10, min_height = 10 })
    t.ok(vivid.error_info(t.fails(window.bounds, window)).message:find("viewport", 1, true))
    t.ok(vivid.error_info(t.fails(window.measure_text, window, "hello", 12)).message
      :find("text service", 1, true))
    t.raises("invalid", "unknown text option field", window.measure_text, window, "x", 12, { size = 3 })
    t.raises("invalid", "unknown window mode", session.create_window, session,
      { bounds = rect(0, 0, 1, 1), mode = "fullscreen" })
    t.raises("invalid", "unknown window option field", session.create_window, session,
      { bounds = rect(0, 0, 1, 1), modal = true })
    t.raises("invalid", "between zero and 60 seconds", session.wait_event, session, 61)
    window:close()
    window:close() -- idempotent
  end)
end)

test("an SDK presenter refuses overlay negotiation", function()
  local running = vivid.presenter.start("tcp:127.0.0.1:0")
  local secret = running:issue_pane_capability(1)
  local info = vivid.error_info(t.fails(overlay.connect,
    { endpoint_control = running.endpoint, root_secret = secret }))
  t.ok(info and info.message:find("profile", 1, true), "the refusal names the profile")
  running:close()
end)

return tests
