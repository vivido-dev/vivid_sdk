-- The producer core against the SDK's offline contract. No presenter or terminal is needed.

local vivid = require("vivid_sdk")
local t = require("support")

local tests = {}
local function test(name, fn)
  tests[#tests + 1] = { name, fn }
end

local function dry_session(options)
  options = options or {}
  options.dry_run = true
  return vivid.connect(options)
end

local function pixels_surface(session)
  return session:create_surface({
    logical_width = 2,
    logical_height = 2,
    role = vivid.ROLE_FIGURE,
    title = "pixels",
  })
end

test("session profiles and a redacted tostring", function()
  local secret = string.rep("11", 32)
  local session = dry_session({ root_secret = secret })
  local info = session:info()
  t.eq(info.session_id, 1)
  t.eq(info.root_context_id, 1)
  t.eq(info.target_profile, vivid.PROFILE_TERMINAL_SURFACE)
  t.ok(session:supports(vivid.PROFILE_CORE))
  t.ok(session:supports(vivid.PROFILE_LIVE_MEDIA))
  t.ok(not tostring(session):find(secret, 1, true), "the secret is not in tostring")
  t.ok(not tostring(session):find("SECRET", 1, true))
  session:close()
  t.ok(session.closed)
  t.raises("closed", "session is closed", session.close, session)
  t.raises("closed", "session is closed", session.info, session)
end)

test("surface, track, channel, and ordered EOS", function()
  local session = dry_session()
  local surface = pixels_surface(session)
  session:place_terminal_surface(surface, { width = 2, height = 1 })
  local track = session:create_track(surface, { kind = "raster", width = 2, height = 2 })
  local channel = session:open_track_channel(track)
  t.eq(surface.context_id, track.context_id)
  t.eq(surface.id, track.surface_id)
  t.eq(channel.track_id, track.id)
  t.eq(channel.generation, 1)
  t.eq(track.channel_generation, 1)
  t.eq(track.kind, "raster")
  local waited = session:wait_track(track, vivid.WAIT_CHANNEL_ACCEPTED, nil, 1)
  t.eq(waited.track_id, track.id)
  t.eq(waited.channel_generation, channel.generation)
  local media = channel:send_raster(string.rep("\255\0\0\255", 4))
  local eos = channel:eos()
  t.ok(eos > media, "EOS follows the last media record")
  t.raises("invalid", "CHANNEL_EOS", channel.send_raster, channel, string.rep("\0\0\0\255", 4))
  channel:close()
  t.ok(channel.closed)
  t.raises("closed", "track channel is closed", channel.close, channel)
  session:close()
end)

test("raster frame IDs continue on their own and after an explicit one", function()
  local session = dry_session()
  local surface = pixels_surface(session)
  local track = session:create_track(surface, { kind = "raster", width = 1, height = 1 })
  local channel = session:open_track_channel(track)
  channel:send_raster("\0\0\0\255")
  channel:send_raster("\0\0\0\255")
  channel:send_raster("\0\0\0\255", { frame_id = 10 })
  channel:send_raster("\0\0\0\255") -- 11: the sequence continues past the explicit ID.
  t.raises("invalid", nil, channel.send_raster, channel, "\0\0\0\255", { frame_id = 11 })
  session:close()
end)

test("track replacement keeps the surface generation", function()
  local session = dry_session()
  local surface = session:create_surface({ logical_width = 2, logical_height = 2 })
  local generation = surface.generation
  local first = session:create_track(surface, { kind = "raster", width = 2, height = 2 })
  local second = session:create_track(surface, { kind = "raster", width = 2, height = 2 })
  session:destroy_track(first)
  t.eq(surface.generation, generation)
  t.eq(second.surface_id, surface.id)
  session:update_surface(surface, { logical_width = 3, logical_height = 2 })
  t.eq(surface.generation, generation + 1, "a coordinate change advances the generation")
  session:destroy_surface(surface)
  session:close()
end)

test("an encoded-image track is immutable and one-shot", function()
  local session = dry_session()
  local surface = session:create_surface({ logical_width = 1, logical_height = 1 })
  local track = session:create_track(surface, { kind = "image", encoded = t.PNG })
  local channel = session:open_track_channel(track)
  channel:send_image(t.PNG)
  t.raises("invalid", "exactly one", channel.send_image, channel, t.PNG)
  session:close()
end)

test("an image track refuses a malformed container", function()
  local session = dry_session()
  local surface = session:create_surface({ logical_width = 1, logical_height = 1 })
  t.fails(session.create_track, session, surface, { kind = "image", encoded = "\137PNG" })
  t.raises("invalid", "sha256", session.create_track, session, surface,
    { kind = "image", encoded = t.PNG, sha256 = "short" })
  session:close()
end)

test("display_image returns live handles", function()
  local directory = t.tempdir()
  local path = directory .. "/pixel.png"
  t.write_file(path, t.PNG)
  local presentation = vivid.display_image(path, { dry_run = true })
  t.ok(not presentation.session.closed)
  t.ok(not presentation.channel.closed)
  t.eq(presentation.track.surface_id, presentation.surface.id)
  t.eq(presentation.track.context_id, presentation.surface.context_id)
  t.eq(presentation.track.kind, "image")
  presentation:close()
  t.ok(presentation.session.closed)
  t.ok(presentation.channel.closed)
  presentation:close() -- idempotent
  t.raises("vivid", "cannot read", vivid.display_image, directory .. "/missing.png", { dry_run = true })
  t.raises("invalid", "unknown connect option", vivid.display_image, path, { colums = 3 })
  t.remove_tree(directory)
end)

test("a trace holds metadata without record bodies", function()
  local directory = t.tempdir()
  local session = vivid.connect({ trace_dir = directory })
  local surface = session:create_surface({ logical_width = 2, logical_height = 2 })
  local track = session:create_track(surface, { kind = "raster", width = 2, height = 2 })
  local channel = session:open_track_channel(track)
  channel:send_raster(string.rep("\0\0\0\255", 4))
  local marker = session:anchor_marker(nil, 7)
  session:close()
  channel:close()
  local control = t.read_file(directory .. "/control.ndjson")
  t.ok(control:find('"record_type":1[,}]'), "the preface record is traced")
  t.ok(not control:find('"body"', 1, true), "no record bodies")
  t.ok(marker:find("VIVID;3;A;", 1, true))
  t.ok(marker:find(";0000000000000001;0000000000000007;", 1, true))
  t.remove_tree(directory)
end)

test("connect normalizes profile lists", function()
  -- Required profiles drop out of the optional set, order and duplicates do not matter.
  local session = dry_session({
    required_profiles = {
      vivid.PROFILE_TERMINAL_SURFACE,
      vivid.PROFILE_LIVE_MEDIA,
      vivid.PROFILE_CORE,
      vivid.PROFILE_CORE,
    },
  })
  t.ok(session:supports(vivid.PROFILE_LIVE_MEDIA))
  session:close()
  t.raises("invalid", "required profiles must contain core", vivid.connect,
    { dry_run = true, required_profiles = { vivid.PROFILE_TERMINAL_SURFACE } })
end)

test("a pane session replaces media and clears idempotently", function()
  local png = "\137PNG\r\n\26\n\0\0\0\rIHDR\0\0\0\2\0\0\0\1\8\6\0\0\0"
  local pane = vivid.PaneSession.connect({ dry_run = true })
  pane:show_encoded_image(png, { title = "encoded" })
  pane:show_rgba(1, 1, "\1\2\3\4", { title = "raster" })
  pane:clear()
  pane:clear()
  t.eq(tostring(pane), "vivid_sdk.PaneSession(has_presentation=false)")
  pane:close()
  t.ok(pane.closed)
end)

test("a pane session's tostring does not expose authentication", function()
  local secret = string.rep("42", 32)
  local pane = vivid.PaneSession.connect({ dry_run = true, root_secret = secret })
  local shown = tostring(pane)
  t.ok(not shown:find(secret, 1, true))
  t.ok(not shown:lower():find("secret", 1, true))
  t.ok(not shown:lower():find("endpoint", 1, true))
  pane:close()
end)

test("a pane session refuses an invalid replacement without clearing", function()
  local pane = vivid.PaneSession.connect({ dry_run = true })
  pane:show_rgba(1, 1, "\0\0\0\255")
  t.raises("invalid", "dimensions", pane.show_rgba, pane, 0, 1, "")
  t.ok(pane.has_presentation, "the previous presentation stands")
  pane:clear()
  t.ok(not pane.has_presentation)
  pane:close()
end)

test("a pane session adopts a session, which is then closed", function()
  local session = dry_session()
  local pane = vivid.PaneSession.from_session(session)
  t.ok(session.closed, "the pane owns the session now")
  pane:show_rgba(1, 1, "\0\0\0\255")
  pane:close()
  t.raises("closed", "pane session is closed", pane.clear, pane)
end)

test("a lease secret is returned once and is never part of the lease", function()
  local session = dry_session()
  local ready, secret = session:create_session_lease({
    context_id = 1,
    lease_id = 2,
    permitted_profiles = { vivid.PROFILE_TERMINAL_SURFACE, vivid.PROFILE_CORE },
  })
  t.eq(#secret, 64)
  t.ok(secret:match("^%x+$"))
  for key, value in pairs(ready) do
    t.ok(value ~= secret, "the lease table holds no secret")
    t.ok(not tostring(key):lower():find("secret", 1, true))
  end
  t.eq(ready.lease_id, 2)
  t.eq(#ready.permitted_profiles, 2)
  session:close()
end)

test("a root session is not resumable", function()
  local session = dry_session()
  t.raises("vivid", "non-resumable", session.prepare_resume, session)
  session:close()
end)

test("track claims come from the Rust builder", function()
  -- A 2x2 raster frame is 72 bytes of packet header plus 16 bytes of pixels; the in-flight and
  -- retained-pixel claims follow from that rather than from a number written in Lua.
  local session = dry_session()
  local surface = pixels_surface(session)
  local config = session:build_track_config(surface, { kind = "raster", width = 2, height = 2 })
  t.eq(config.maximum_record_body, 72 + 2 * 2 * 4)
  t.eq(config.maximum_inflight_body_bytes, 2 * (72 + 2 * 2 * 4))
  t.eq(config.retained_pixel_charge, 2 * 2)
  t.eq(config.slot, vivid.SLOT_RASTER)
  t.eq(config.lane, vivid.LANE_BULK)
  local audio = session:build_track_config(surface, { kind = "audio", sample_rate = 48000, channels = 2 })
  t.eq(audio.lane, vivid.LANE_REALTIME, "audio defaults to the realtime lane")
  t.eq(audio.codec, "opus")
  -- A video track's claims are sized for real streams, which the offline contract refuses; the
  -- refusal is the builder's own check against the contract, before anything is sent.
  t.raises("invalid", "contract", session.build_track_config, session, surface,
    { kind = "video", codec = "h264", width = 64, height = 32 })
  session:close()
end)

test("a surface configuration keeps scale and descriptor", function()
  local session = dry_session()
  local config = {
    logical_width = 20,
    logical_height = 10,
    scale_numerator = 3,
    scale_denominator = 2,
    rotation = 90,
    semantic_content_revision = 7,
    semantic_availability = 1,
    locator_hint = "figure-7",
  }
  local built = session:build_surface_config(config)
  for name, value in pairs(config) do
    t.eq(built[name], value, name)
  end
  t.eq(built.context_id, session:info().root_context_id)
  t.raises("invalid", nil, session.build_surface_config, session,
    { logical_width = 2, logical_height = 2, scale_denominator = 0 })
  session:close()
end)

test("audio channels do not wrap", function()
  local session = dry_session()
  local surface = pixels_surface(session)
  t.raises("invalid", "channels is out of range", session.create_track, session, surface,
    { kind = "audio", sample_rate = 48000, channels = 257 })
  session:close()
end)

test("configuration tables refuse unknown fields", function()
  local session = dry_session()
  t.raises("invalid", 'unknown surface configuration field "logical_widht"',
    session.create_surface, session, { logical_width = 2, logical_widht = 2, logical_height = 2 })
  local surface = pixels_surface(session)
  t.raises("invalid", 'unknown raster track field "codec"', session.create_track, session, surface,
    { kind = "raster", width = 2, height = 2, codec = "h264" })
  t.raises("invalid", "track kind must be", session.create_track, session, surface, { kind = "vector" })
  t.raises("invalid", "missing configuration field height", session.create_track, session, surface,
    { kind = "raster", width = 2 })
  t.raises("invalid", "unknown placement field", session.place_terminal_surface, session, surface,
    { width = 1, height = 1, colour = 1 })
  t.raises("invalid", "unknown connect option", vivid.connect, { dryrun = true })
  session:close()
end)

test("integers are integral, in range, and exact", function()
  local session = dry_session()
  t.raises("invalid", "must be a non-negative integer", session.create_surface, session,
    { logical_width = 1.5, logical_height = 2 })
  t.raises("invalid", "must be a non-negative integer", session.create_surface, session,
    { logical_width = -1, logical_height = 2 })
  t.raises("invalid", "must be a non-negative integer", session.create_surface, session,
    { logical_width = "2", logical_height = 2 })
  t.raises("invalid", "must be a non-negative integer", session.create_surface, session,
    { logical_width = 0 / 0, logical_height = 2 })
  -- A double cannot say 2^53 + 1, so no float at or beyond 2^53 is exact, on any Lua.
  t.raises("invalid", "exceeds the integers this Lua carries exactly", session.create_surface,
    session, { logical_width = 2 ^ 60, logical_height = 2 })
  t.raises("invalid", "exceeds the integers this Lua carries exactly", session.anchor_marker,
    session, nil, 2 ^ 53)
  if math.tointeger then
    -- A true 64-bit integer is exact, and reaches the wire whole.
    local marker = session:anchor_marker(nil, math.tointeger(2 ^ 60) + 1)
    t.ok(marker:find(";1000000000000001;", 1, true), "the full 64-bit anchor ID")
  end
  local surface = pixels_surface(session)
  t.raises("invalid", "width is out of range", session.create_track, session, surface,
    { kind = "raster", width = 2 ^ 40, height = 2 })
  session:close()
end)

test("booleans are booleans, not truthiness", function()
  local session = dry_session()
  local surface = pixels_surface(session)
  local track = session:create_track(surface, { kind = "raster", width = 1, height = 1 })
  local channel = session:open_track_channel(track)
  t.raises("invalid", "compress must be a boolean", channel.send_raster, channel, "\0\0\0\255",
    { compress = 1 })
  session:close()
end)

test("error_info reads SDK errors and nothing else", function()
  t.eq(vivid.error_info("plain string"), nil)
  t.eq(vivid.error_info(nil), nil)
  local _, err = pcall(error, { custom = true })
  t.eq(vivid.error_info(err), nil)
  local session = dry_session()
  session:close()
  local ok, closed = pcall(session.allocate_id, session)
  t.ok(not ok)
  local info = vivid.error_info(closed)
  t.eq(info.kind, "closed")
  t.eq(info.message, "session is closed")
  t.ok(tostring(closed):find("session is closed", 1, true), "tostring carries the message")
  -- A value mlua refused to convert is the same class of mistake as an out-of-range one.
  local fresh = dry_session()
  local _, wrong = pcall(fresh.create_track, fresh, "not a surface", { kind = "raster" })
  t.eq(vivid.error_info(wrong).kind, "invalid")
  fresh:close()
end)

test("events: an empty queue, a quiet iteration, and a closed session", function()
  local session = dry_session()
  while session:take_event() do
  end
  t.eq(session:take_event(), nil)
  local count = 0
  for _ in session:events(0) do
    count = count + 1
  end
  t.eq(count, 0, "a quiet session ends the iteration at its timeout")
  local iterator = session:events(0)
  session:close()
  t.eq(iterator(), nil, "a closed session has no events left")
  t.raises("invalid", "timeout must be", session.wait_event, session, -1)
end)

test("reverse-channel waits and send pressure", function()
  local session = dry_session()
  local surface = pixels_surface(session)
  local track = session:create_track(surface, { kind = "raster", width = 1, height = 1 })
  local channel = session:open_track_channel(track)
  t.eq(channel:take_event(), nil)
  t.eq(channel:wait_event(0), nil)
  channel:send_raster("\0\0\0\255")
  local pressure = channel:take_send_pressure()
  t.ok(pressure.records >= 1)
  t.ok(pressure.rate_limited_us >= 0 and pressure.flow_limited_us >= 0 and pressure.transport_us >= 0)
  t.ok(channel:media_credit_available(8))
  local control = vivid.VideoRateControl.new(2000000)
  control:observe_send(1000, pressure)
  control:observe_audio_backlog(0)
  local snapshot = control:snapshot()
  t.eq(snapshot.configured_bits_per_second, 2000000)
  t.raises("invalid", "unknown send pressure field", control.observe_send, control, 1,
    { records = 1, rate_limited_us = 0, flow_limited_us = 0, transport_us = 0, extra = 1 })
  session:close()
end)

test("raster deltas against an accepted base frame", function()
  local session = dry_session()
  local surface = pixels_surface(session)
  local track = session:create_track(surface, {
    kind = "raster",
    width = 8,
    height = 8,
    delta_enabled = true,
    maximum_delta_operations = 4,
  })
  local channel = session:open_track_channel(track)
  channel:send_raster(string.rep(t.PIXELS, 16), { frame_id = 1 })
  channel:send_raster_delta({
    { op = "overwrite", x = 0, y = 0, width = 1, height = 1, rgba = "\9\9\9\255" },
    { op = "copy", destination_x = 1, destination_y = 1, width = 1, height = 1, source_x = 0, source_y = 0 },
  }, { base_frame_id = 1 })
  t.raises("invalid", "delta operation needs op", channel.send_raster_delta, channel,
    { { x = 0 } }, { base_frame_id = 2 })
  t.raises("invalid", "missing configuration field base_frame_id", channel.send_raster_delta,
    channel, {}, {})
  t.raises("invalid", "compress is not an option", channel.send_raster_delta_adaptive, channel,
    {}, { base_frame_id = 2, compress = true })
  session:close()
end)

test("slot activation needs at least one binding", function()
  local session = dry_session()
  local surface = pixels_surface(session)
  t.raises("invalid", "at least one slot binding", session.activate_tracks, session, surface, {})
  session:close()
end)

test("scene nodes and anchors", function()
  local session = dry_session()
  local surface = pixels_surface(session)
  local commit = session:create_node(surface, {
    geometry = { [0] = 1, [1] = 0, [2] = 0, [3] = 2 * 4294967296, [4] = 4294967296 },
  })
  t.ok(commit.scene_revision >= 1)
  t.raises("invalid", "geometry key", session.create_node, session, surface, { geometry = { x = 1 } })
  local anchor = session:query_anchor(session:info().root_context_id, 99)
  t.eq(anchor.state, 0, "an anchor the presenter never saw is unknown")
  t.raises("invalid", "nonzero", session.query_anchor, session, 0, 1)
  session:close()
end)

test("probe_encoded_image, constants, and the Lua helpers", function()
  local info = vivid.probe_encoded_image(t.PNG)
  t.eq(info.encoding, vivid.IMAGE_PNG)
  t.eq(info.width, 1)
  t.eq(info.height, 1)
  t.eq(info.encoded_length, #t.PNG)
  t.raises("invalid", nil, vivid.probe_encoded_image, "not an image")
  t.eq(vivid.MIC_PACKET_BYTES, 1920)
  t.eq(vivid.MIC_PACKET_US, 20000)
  local before = vivid.monotonic()
  vivid.sleep(0.01)
  t.ok(vivid.monotonic() - before >= 0.009, "sleep blocks and the clock advances")
  t.raises("invalid", nil, vivid.sleep, -1)
end)

if _VERSION >= "Lua 5.4" and not jit then
  test("to-be-closed handles close at scope exit", function()
    local chunk = assert(load([[
      local vivid = ...
      local escaped
      do
        local session <close> = vivid.connect({ dry_run = true })
        escaped = session
      end
      return escaped
    ]]))
    local session = chunk(vivid)
    t.ok(session.closed)
  end)
end

return tests
