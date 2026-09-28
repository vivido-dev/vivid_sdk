-- A Lua producer and a Lua presenter, over a real loopback socket.
--
-- Every wait is bounded and every presenter is closed, because a leaked presenter thread would
-- keep the interpreter from exiting.

local vivid = require("vivid_sdk")
local t = require("support")

local tests = {}
local function test(name, fn)
  tests[#tests + 1] = { name, fn }
end

local function with_presenter(fn)
  local running = vivid.presenter.start("tcp:127.0.0.1:0")
  local ok, err = pcall(fn, running)
  running:close()
  if not ok then
    error(err, 0)
  end
end

local function producer_for(running, pane)
  running:update_metrics(pane, { columns = 80, rows = 24 })
  local secret = running:issue_pane_capability(pane)
  return vivid.PaneSession.connect({ endpoint_control = running.endpoint, root_secret = secret })
end

test("a producer and a presenter exchange a frame", function()
  with_presenter(function(running)
    local pane = producer_for(running, 1)
    pane:show_rgba(2, 2, t.PIXELS)
    t.ok(running:wait_for_media(1, t.TIMEOUT))
    local capture = running:capture_pane(1)
    t.eq(#capture.skipped, 0)
    t.eq(#capture.layers, 1)
    local layer = capture.layers[1]
    t.eq(layer.content.kind, "raster")
    t.eq(layer.content.width, 2)
    t.eq(layer.content.height, 2)
    t.eq(layer.content.rgba, t.PIXELS, "the exact bytes the producer sent")
    t.ok(layer.source.producer > 0 and layer.source.track > 0, "the complete owner tuple")
    pane:close()
  end)
end)

test("an ephemeral port reports the port it got", function()
  with_presenter(function(running)
    t.ok(running.endpoint:find("^tcp:127%.0%.0%.1:"))
    t.ok(not running.endpoint:find(":0$"), "the resolved port, not the request")
  end)
end)

test("a pane capability never appears in a tostring", function()
  with_presenter(function(running)
    local secret = running:issue_pane_capability(1)
    t.eq(#secret, 64, "a 32-byte root secret, hex encoded")
    t.ok(not tostring(running):find(secret, 1, true))
    t.ok(tostring(running):find(running.endpoint, 1, true), "addressing is not capability material")
  end)
end)

test("a summary reports what a capture would produce", function()
  with_presenter(function(running)
    t.eq(#running:pane_media_summary(1).tracks, 0)
    local pane = producer_for(running, 1)
    pane:show_rgba(2, 2, t.PIXELS)
    t.ok(running:wait_for_media(1, t.TIMEOUT))
    local summary = running:pane_media_summary(1)
    t.eq(#summary.tracks, 1)
    t.eq(summary.tracks[1].kind, "raster")
    t.ok(summary.tracks[1].capturable, "pixels are in hand, not merely promised")
    t.eq(running:pane_for_source(summary.tracks[1].source), 1)
    pane:close()
  end)
end)

test("two owners reusing local IDs capture only their own pixels", function()
  -- Both producers are built the same way, so both allocate the same local surface, track, and
  -- node numbers; identity is the complete owner tuple, and the second owner holds nothing.
  with_presenter(function(running)
    local first = producer_for(running, 1)
    local second = producer_for(running, 2)
    first:show_rgba(2, 2, t.PIXELS)
    t.ok(running:wait_for_media(1, t.TIMEOUT))
    t.ok(not running:wait_for_media(2, 0.2), "the second owner holds nothing")
    t.eq(#running:capture_pane(1).layers, 1)
    t.eq(#running:capture_pane(2).layers, 0)
    second:show_rgba(2, 2, string.rep("\7\7\7\255", 4))
    t.ok(running:wait_for_media(2, t.TIMEOUT))
    t.eq(running:capture_pane(1).layers[1].content.rgba, t.PIXELS, "the first owner is intact")
    t.eq(running:capture_pane(2).layers[1].content.rgba, string.rep("\7\7\7\255", 4))
    -- Revoking one owner leaves the other's retained media alone.
    running:revoke_pane(2)
    t.eq(#running:capture_pane(2).layers, 0)
    t.eq(running:capture_pane(1).layers[1].content.rgba, t.PIXELS)
    first:close()
  end)
end)

test("an explicit producer session drives the same presenter", function()
  with_presenter(function(running)
    running:update_metrics(3, { columns = 80, rows = 24, cell_width = 8, cell_height = 16 })
    local session = vivid.connect({
      endpoint_control = running.endpoint,
      root_secret = running:issue_pane_capability(3),
    })
    local surface = session:create_surface({ logical_width = 2, logical_height = 2 })
    session:place_terminal_surface(surface, { node_id = 1, width = 4, height = 2 })
    local track = session:create_track(surface, { kind = "raster", width = 2, height = 2 })
    local channel = session:open_track_channel(track)
    channel:send_raster(t.PIXELS)
    session:wait_track(track, vivid.WAIT_MILESTONE_SET, vivid.MILESTONE_OUTPUT_READY, t.TIMEOUT)
    session:activate_track(surface, track)
    t.ok(running:wait_for_media(3, t.TIMEOUT))
    t.eq(running:capture_pane(3).layers[1].content.rgba, t.PIXELS)
    local status = session:query_track(track)
    t.eq(status.track_id, track.id)
    t.eq(status.kind, "raster")
    t.ok(session:query_surface(surface).logical_width == 2)
    channel:close()
    session:delete_node(surface.context_id, 1)
    session:destroy_surface(surface)
    session:close()
  end)
end)

test("a closed presenter refuses further work", function()
  local running = vivid.presenter.start("tcp:127.0.0.1:0")
  running:close()
  t.ok(running.closed)
  t.raises("closed", "presenter is closed", running.issue_pane_capability, running, 1)
  t.raises("closed", "presenter is closed", running.capture_pane, running, 1)
  running:close() -- idempotent
end)

test("a negative timeout is refused rather than waiting forever", function()
  with_presenter(function(running)
    t.raises("invalid", "timeout", running.wait_for_media, running, 1, -1)
    t.raises("invalid", "unknown pane metric field", running.update_metrics, running, 1,
      { columns = 80, rows = 24, colums = 1 })
  end)
end)

test("a Unix endpoint is created owner-only", function()
  local directory = t.tempdir()
  local path = directory .. "/presenter.sock"
  local running = vivid.presenter.start("unix:" .. path)
  t.eq(running.endpoint, "unix:" .. path)
  local listing = assert(io.popen("ls -l " .. t.shell_quote(path))):read("*a")
  t.ok(listing:find("^srw%-%-%-%-%-%-%-"), "a socket readable and writable by its owner only")
  t.raises("vivid", nil, vivid.presenter.start, "unix:" .. path)
  running:close()
  t.remove_tree(directory)
end)

test("a non-loopback TCP endpoint is refused", function()
  t.fails(vivid.presenter.start, "tcp:0.0.0.0:0")
end)

test("media resources describe a pane's track", function()
  with_presenter(function(running)
    local pane = producer_for(running, 1)
    pane:show_rgba(2, 2, t.PIXELS)
    t.ok(running:wait_for_media(1, t.TIMEOUT))
    local source = running:pane_media_summary(1).tracks[1].source
    local id = running:announce_media_resource(source, true)
    local described = running:describe_media_resource(id)
    t.ok(described.pinned)
    t.eq(described.producer, source.producer)
    t.eq(described.surface, source.surface)
    t.ok(running:release_media_resource(id))
    t.ok(not running:release_media_resource(id))
    t.ok(running:projection_revision() >= 1)
    pane:close()
  end)
end)

return tests
