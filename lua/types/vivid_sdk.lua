---@meta vivid_sdk

-- Type definitions for the `vivid_sdk` native Lua module, for lua-language-server.
--
-- This file is never loaded at runtime: `require("vivid_sdk")` loads the native library, and
-- every value below comes from the Rust SDK when it does. Point lua-language-server at this
-- directory (`workspace.library`) to get completion and checking. The Lua analogue of
-- `_native.pyi` and `index.d.ts`; `lua-tests/test_exports.lua` keeps it in step with the module.

---@alias vivid_sdk.Bytes string A Lua byte string: pixels, encoded media, a digest.
---@alias vivid_sdk.Payload table<integer, any> A decoded control payload, keyed by the wire's integer keys.

-------------------------------------------------------------------------------------------------
-- Module
-------------------------------------------------------------------------------------------------

---The Vivid Protocol 1.5 SDK: a producer (`connect`) and a terminating presenter
---(`presenter.start`) in one module.
---
---Protocol constants are fields of the module, read at load time from the Rust table that owns
---them.
---@class vivid_sdk
---@field VERSION string The module's version.
---@field PROFILE_CORE string
---@field PROFILE_TERMINAL_SURFACE string
---@field PROFILE_DESKTOP_SURFACE string
---@field PROFILE_CANVAS_SURFACE string
---@field PROFILE_LIVE_MEDIA string
---@field PROFILE_TIMED_MEDIA string
---@field PROFILE_TIMED_MEDIA_SYNC string
---@field PROFILE_AUDIO_GAIN string
---@field PROFILE_AUDIO_INPUT string
---@field PROFILE_DESKTOP_INPUT string
---@field PROFILE_FILE_DROP string
---@field PROFILE_FILE_DROP_PATH string
---@field PROFILE_OBSERVABILITY string
---@field PROFILE_WEB_CARRIER string
---@field PROFILE_TERMINAL_OVERLAY string
---@field PROFILE_VECTOR_SCENE string
---@field PROFILE_OVERLAY_INPUT string
---@field PROFILE_OVERLAY_TEXT string
---@field PROFILE_OVERLAY_TEXT_LAYOUT string
---@field PROFILE_OVERLAY_TYPOGRAPHY string
---@field PROFILE_OVERLAY_PAINT string
---@field PROFILE_OVERLAY_POINTER string
---@field PROFILE_OVERLAY_CLIPBOARD string
---@field PROFILE_OVERLAY_ENV string
---@field SURFACE_GENERIC string
---@field SURFACE_TERMINAL string
---@field SURFACE_DESKTOP string
---@field SURFACE_CANVAS string
---@field COORDINATE_DESKTOP_LOGICAL_PIXELS integer
---@field COORDINATE_NORMALIZED integer
---@field COORDINATE_CANVAS_LOGICAL_UNITS integer
---@field COORDINATE_TERMINAL_CONTENT_CELLS integer
---@field ROLE_UNSPECIFIED integer
---@field ROLE_DOCUMENT integer
---@field ROLE_DESKTOP integer
---@field ROLE_TIMED_MEDIA integer
---@field ROLE_FIGURE integer
---@field ROLE_TERMINAL integer
---@field ROLE_CANVAS integer
---@field POLICY_DENY_CAPTURE integer
---@field POLICY_DENY_DESCRIPTOR_EXPORT integer
---@field POLICY_DENY_POSTER_RETENTION integer
---@field POLICY_DENY_IMAGE_CACHE integer
---@field POLICY_REDUCED_DIAGNOSTICS integer
---@field POLICY_KNOWN_MASK integer
---@field TRACK_MODE_LIVE integer
---@field TRACK_MODE_TIMED integer
---@field TRACK_DIRECTION_DOWNLINK integer
---@field TRACK_DIRECTION_UPLINK integer
---@field TRACK_KIND_VIDEO integer
---@field TRACK_KIND_AUDIO integer
---@field TRACK_KIND_RASTER integer
---@field TRACK_KIND_IMAGE integer
---@field TRACK_KIND_VECTOR integer
---@field LANE_CONTROL integer
---@field LANE_INTERACTIVE integer
---@field LANE_REALTIME integer
---@field LANE_BULK integer
---@field SLOT_NONE integer
---@field SLOT_PRIMARY_VIDEO integer
---@field SLOT_AUDIO integer
---@field SLOT_RASTER integer
---@field SLOT_POSTER integer
---@field SLOT_VECTOR integer
---@field FIT_FILL integer
---@field FIT_CONTAIN integer
---@field FIT_COVER integer
---@field FIT_NONE integer
---@field IMAGE_PNG integer
---@field IMAGE_JPEG integer
---@field MILESTONE_CHANNEL_ACCEPTED integer
---@field MILESTONE_FIRST_MEDIA integer
---@field MILESTONE_DECODER_INITIALIZED integer
---@field MILESTONE_RANDOM_ACCESS integer
---@field MILESTONE_OUTPUT_READY integer
---@field MILESTONE_PRESENTED integer
---@field MILESTONE_CLOCK_STARTED integer
---@field MILESTONE_EOS_ACCEPTED integer
---@field MILESTONE_BUFFERED_ENDED integer
---@field MILESTONE_CHANNEL_DETACHED integer
---@field MILESTONE_TRACK_LOST integer
---@field MILESTONE_KNOWN_MASK integer
---@field WAIT_REVISION_GREATER integer
---@field WAIT_MILESTONE_SET integer
---@field WAIT_RASTER_FRAME_PRESENTED integer
---@field WAIT_VIDEO_PTS_PRESENTED integer
---@field WAIT_PLAYBACK_STARTED integer
---@field WAIT_PLAYBACK_ENDED integer
---@field WAIT_CHANNEL_ACCEPTED integer
---@field WAIT_CHANNEL_CLOSED integer
---@field WAIT_TRACK_LOST integer
---@field MAX_TRACK_WAIT_TIMEOUT_US integer
---@field OP_OBSERVE integer
---@field OP_SURFACE_TRACK_MEDIA integer
---@field OP_SCENE integer
---@field OP_TERMINAL_ANCHOR integer
---@field OP_DESKTOP_INPUT integer
---@field OP_DELEGATE integer
---@field OP_RECEIVE_FILE_DROP integer
---@field OP_KNOWN_MASK integer
---@field INPUT_CLASS_KEYBOARD integer
---@field INPUT_CLASS_POINTER_MOTION integer
---@field INPUT_CLASS_POINTER_BUTTON integer
---@field INPUT_CLASS_POINTER_AXIS integer
---@field INPUT_CLASS_KNOWN_MASK integer
---@field MIN_WATCHDOG_US integer
---@field MAX_WATCHDOG_US integer
---@field CLEANUP_IMMEDIATE integer
---@field CLEANUP_SUSPEND_ON_UNCLEAN_LOSS integer
---@field DESTINATION_SHELL_CWD integer
---@field DESTINATION_DESKTOP_FOLDER integer
---@field DROP_OFFERED integer
---@field DROP_ACCEPTED integer
---@field DROP_TRANSFERRING integer
---@field DROP_COMMITTED integer
---@field DROP_CANCELLED integer
---@field DROP_FAILED integer
---@field MIC_PACKET_US integer
---@field MIC_PACKET_BYTES integer
---@field COORDINATE_SPACE_GRID_CELL integer
---@field TEXT_LAYER_BETWEEN_BACKGROUND_AND_GLYPH integer
---@field MINIMUM_TARGET_BITS_PER_SECOND integer
---@field DEFAULT_ACTIVATION_TIMEOUT_US integer
---@field MAX_ACTIVATION_TIMEOUT_US integer
---@field PaneSession vivid_sdk.PaneSessionModule
---@field VideoRateControl vivid_sdk.VideoRateControlModule
---@field presenter vivid_sdk.presenter
---@field overlay vivid_sdk.overlay
---@field automation vivid_sdk.automation
local vivid = {}

---One entry of the protocol constant table.
---@class vivid_sdk.ConstantEntry
---@field name string
---@field text string? The value of a profile constant.
---@field number integer? The value of a numeric constant.

---Every protocol constant the SDK exposes, in the table's own order.
---@return vivid_sdk.ConstantEntry[]
function vivid.constant_table() end

---@class vivid_sdk.EncodedImageInfo
---@field encoding integer `IMAGE_PNG` or `IMAGE_JPEG`.
---@field width integer
---@field height integer
---@field encoded_length integer

---Inspect PNG or JPEG header metadata. Reads the container, not the pixels.
---@param data vivid_sdk.Bytes
---@return vivid_sdk.EncodedImageInfo
function vivid.probe_encoded_image(data) end

---What a caught SDK error was. Errors are raised as ordinary Lua errors; pass the caught value
---here to act on its kind rather than its message.
---@class vivid_sdk.ErrorInfo
---@field kind "closed"|"invalid"|"vivid"|"automation" `closed`: the handle was closed. `invalid`: refused before sending. `vivid`: a presenter refused, or the transport or protocol failed. `automation`: an automation endpoint refused or could not be resolved.
---@field message string The message without the traceback.
---@field code integer|string? A presenter's registered error code, or an automation runtime's code.
---@field fatal boolean? Whether a presenter rejection ended the session.
---@field data any? An automation error's structured data.

---The structured form of a caught SDK error, or `nil` for anything else.
---@param err any The value `pcall` returned.
---@return vivid_sdk.ErrorInfo?
function vivid.error_info(err) end

---Block this Lua state for `seconds`. Lua has no portable sleep, and frame pacing needs one.
---@param seconds number
function vivid.sleep(seconds) end

---Seconds on a monotonic clock, for deadlines.
---@return number
function vivid.monotonic() end

---How to reach a presenter and who to be. Every field is optional.
---
---Discovery reads `VIVID_ENDPOINT_CONTROL`, the optional lane endpoints, and `VIVID_ROOT_SECRET`
---when the corresponding field is absent. Prefer the environment for the secret: a value passed
---here has crossed Lua.
---@class vivid_sdk.ConnectOptions
---@field dry_run boolean? Connect in-process against the offline contract.
---@field desktop boolean? The desktop producer profile set.
---@field trace_dir string? Record metadata-only NDJSON traces in this new directory.
---@field endpoint_control string?
---@field endpoint_interactive string?
---@field endpoint_realtime string?
---@field endpoint_bulk string?
---@field root_secret string? Root secret as hex.
---@field producer_name string?
---@field producer_version string?
---@field target_profile string?
---@field required_profiles string[]? Sorted and deduplicated for you.
---@field optional_profiles string[]? Sorted, deduplicated, and stripped of required profiles.

---Connect to a presenter.
---@param options vivid_sdk.ConnectOptions?
---@return vivid_sdk.Session
function vivid.connect(options) end

---@class vivid_sdk.DisplayOptions: vivid_sdk.ConnectOptions
---@field columns integer? Terminal cells the image spans; defaults to its width up to 80.
---@field rows integer? Terminal rows the image spans; defaults to its height up to 24.

---Create, activate, and retain one PNG or JPEG, anchored at the cursor when the terminal has a
---text plane and placed against the grid otherwise.
---@param path string
---@param options vivid_sdk.DisplayOptions?
---@return vivid_sdk.ImagePresentation
function vivid.display_image(path, options) end

---Establish a desktop presentation, taking ownership of `session`.
---@param session vivid_sdk.Session
---@param surface vivid_sdk.SurfaceConfig
---@param video vivid_sdk.TrackConfig
---@param audio vivid_sdk.TrackConfig?
---@return vivid_sdk.DesktopSession
function vivid.establish_desktop(session, surface, video, audio) end

-------------------------------------------------------------------------------------------------
-- Configuration
-------------------------------------------------------------------------------------------------

---One output in a desktop surface topology.
---@class vivid_sdk.OutputConfig
---@field output_id integer
---@field origin_x integer
---@field origin_y integer
---@field width integer
---@field height integer
---@field scale_numerator integer? Default 1.
---@field scale_denominator integer? Default 1.
---@field rotation integer? Protocol rotation code: 0 none, 1 90, 2 180, 3 270.
---@field primary boolean?

---Typed parameters that make a surface a desktop surface.
---@class vivid_sdk.DesktopParameters
---@field captured_origin_x integer
---@field captured_origin_y integer
---@field topology vivid_sdk.OutputConfig[]
---@field semantic_generation integer
---@field input_capabilities integer?

---A surface's semantic, scene, and policy identity. Identity and geometry defaults come from the
---SDK's surface builder.
---@class vivid_sdk.SurfaceConfig
---@field logical_width integer
---@field logical_height integer
---@field semantic_profile string? Default `SURFACE_GENERIC`.
---@field coordinate_model integer? Default `COORDINATE_DESKTOP_LOGICAL_PIXELS`.
---@field role integer? Default `ROLE_UNSPECIFIED`.
---@field title string?
---@field semantic_content_revision integer?
---@field semantic_availability integer?
---@field locator_hint string?
---@field policy integer?
---@field scale_numerator integer?
---@field scale_denominator integer?
---@field rotation integer?
---@field context_id integer? Default: the session's root context.
---@field surface_id integer? Default: a freshly allocated ID.
---@field desktop_parameters vivid_sdk.DesktopParameters?

---Claims every track kind may state. Unset claims are the SDK builder's; state them only to
---narrow them.
---@class vivid_sdk.TrackCommon
---@field slot integer?
---@field mode integer? Default `TRACK_MODE_LIVE`.
---@field lane integer? Default `LANE_BULK`, or `LANE_REALTIME` for audio.
---@field track_id integer?
---@field maximum_rate_millihertz integer?
---@field maximum_encoded_bits_per_second integer?

---A retained raster track of tightly packed sRGB RGBA8 frames.
---@class vivid_sdk.RasterTrackConfig: vivid_sdk.TrackCommon
---@field kind "raster"
---@field width integer
---@field height integer
---@field alpha_mode integer?
---@field delta_enabled boolean?
---@field maximum_delta_operations integer?
---@field zstd_enabled boolean?

---A one-shot encoded-image track; the container is inspected by the SDK.
---@class vivid_sdk.ImageTrackConfig: vivid_sdk.TrackCommon
---@field kind "image"
---@field encoded vivid_sdk.Bytes
---@field sha256 vivid_sdk.Bytes? 32 bytes, letting a presenter cache the image.
---@field cache_lookup boolean?

---A live or timed video track. Packetization defaults to `<codec>-annexb-au-v1`.
---@class vivid_sdk.VideoTrackConfig: vivid_sdk.TrackCommon
---@field kind "video"
---@field codec string
---@field width integer
---@field height integer
---@field packetization string?
---@field maximum_access_unit_bytes integer?
---@field extradata vivid_sdk.Bytes?
---@field profile integer?
---@field level integer?
---@field maximum_reorder_depth integer?
---@field color_primaries integer?
---@field transfer integer?
---@field matrix integer?
---@field signal_range integer?
---@field aspect_numerator integer?
---@field aspect_denominator integer?
---@field codec_string string?
---@field decoder_configuration vivid_sdk.Bytes?

---An audio track. Defaults to Opus on the realtime lane.
---@class vivid_sdk.AudioTrackConfig: vivid_sdk.TrackCommon
---@field kind "audio"
---@field sample_rate integer
---@field channels integer
---@field codec string?
---@field packetization string?
---@field maximum_access_unit_bytes integer?
---@field extradata vivid_sdk.Bytes?
---@field channel_mask integer?
---@field codec_string string?
---@field uplink boolean? Microphone audio flowing toward the producer.

---@alias vivid_sdk.TrackConfig vivid_sdk.RasterTrackConfig|vivid_sdk.ImageTrackConfig|vivid_sdk.VideoTrackConfig|vivid_sdk.AudioTrackConfig

---The resolved configuration the SDK builders produce, as `build_track_config` returns it.
---@class vivid_sdk.TrackConfiguration
---@field kind string
---@field context_id integer
---@field surface_id integer
---@field track_id integer
---@field slot integer
---@field mode integer
---@field lane integer
---@field direction integer
---@field maximum_record_body integer
---@field maximum_rate_millihertz integer
---@field maximum_encoded_bits_per_second integer
---@field maximum_records_per_second integer
---@field maximum_inflight_body_bytes integer
---@field target_latency_us integer
---@field maximum_latency_us integer
---@field retained_pixel_charge integer
---@field [string] any The kind's own fields.

---Where a surface sits in the terminal grid, in cells. Fractions are allowed.
---@class vivid_sdk.Placement
---@field node_id integer? Default: a freshly allocated ID.
---@field x number?
---@field y number?
---@field width number
---@field height number
---@field text_layer integer? Default 1.

---One node in a surface's retained scene.
---@class vivid_sdk.SceneNodeConfig
---@field node_id integer? Default: a freshly allocated ID.
---@field geometry table<integer, integer|string|boolean>? The coordinate model's integer-keyed map.
---@field fit integer? Default `FIT_CONTAIN`.
---@field linear_sampling boolean?
---@field z_index integer?
---@field visible boolean?
---@field opacity integer? 0 to 65535; default opaque.

---One slot's activation binding, naming the channel generation it expects.
---@class vivid_sdk.SlotBinding
---@field slot integer
---@field track_id integer
---@field expected_channel_generation integer
---@field required_milestone integer? Default `MILESTONE_OUTPUT_READY`.

---@class vivid_sdk.PlayOptions
---@field start_pts_us integer?
---@field minimum_buffer_us integer?
---@field maximum_latency_us integer?
---@field synchronized boolean?
---@field hold_serial integer?

-------------------------------------------------------------------------------------------------
-- Results
-------------------------------------------------------------------------------------------------

---@class vivid_sdk.SessionInfo
---@field session_id integer
---@field session_tag string Opaque, hex.
---@field root_context_id integer
---@field target_generation integer
---@field target_profile string
---@field accepted_profiles string[]
---@field session_revision integer
---@field scene_revision integer
---@field establishment_state integer
---@field resume_generation integer

---@class vivid_sdk.HeldPosition
---@field track_id integer
---@field channel_generation integer
---@field epoch integer
---@field pts_us integer
---@field estimated boolean

---@class vivid_sdk.PlaybackHold
---@field context_id integer
---@field surface_id integer
---@field serial integer
---@field held boolean
---@field reasons integer
---@field playing_intent boolean
---@field recovery_required boolean
---@field position vivid_sdk.HeldPosition?

---@class vivid_sdk.FileDropTuple
---@field producer_epoch integer
---@field grant_generation integer
---@field context_id integer
---@field surface_id integer
---@field surface_generation integer
---@field drop_id integer

---One session event, named by `kind`: `target_changed`, `anchor_ready`, `anchor_gone`,
---`track_lost`, `context_changed`, `playback_hold`, `file_drop_offered`, `file_drop_cancelled`,
---`other`, or `connection_closed` (the last event a session produces).
---@class vivid_sdk.SessionEvent
---@field kind string
---@field context_id integer?
---@field anchor_id integer?
---@field object_id integer?
---@field record_type integer?
---@field diagnostic string?
---@field payload vivid_sdk.Payload?
---@field hold vivid_sdk.PlaybackHold?
---@field binding vivid_sdk.FileDropTuple?
---@field suggested_name string?
---@field declared_length integer?
---@field drop_id integer?
---@field reason integer?

---One reverse-channel event: `need_keyframe`, `need_full_frame`, or `error`.
---@class vivid_sdk.ChannelEvent
---@field kind string
---@field payload vivid_sdk.Payload?
---@field code integer?
---@field message string?

---@class vivid_sdk.WaitSatisfied
---@field context_id integer
---@field surface_id integer
---@field track_id integer
---@field revision integer
---@field channel_generation integer
---@field condition integer
---@field observed_value integer?

---@class vivid_sdk.SceneCommit
---@field scene_revision integer
---@field target_generation integer

---@class vivid_sdk.SurfaceDefinition
---@field context_id integer
---@field surface_id integer
---@field semantic_profile string
---@field coordinate_model integer
---@field logical_width integer
---@field logical_height integer
---@field scale_numerator integer
---@field scale_denominator integer
---@field rotation integer
---@field role integer
---@field title string
---@field semantic_content_revision integer
---@field semantic_availability integer
---@field locator_hint string
---@field policy integer

---@class vivid_sdk.SurfaceStatus: vivid_sdk.SurfaceDefinition
---@field revision integer
---@field generation integer
---@field effective_policy integer
---@field active_slots vivid_sdk.Payload
---@field lifecycle integer
---@field profile_status vivid_sdk.Payload

---@class vivid_sdk.TrackStatus
---@field context_id integer
---@field surface_id integer
---@field track_id integer
---@field kind string
---@field mode integer
---@field revision integer
---@field channel_generation integer
---@field lifecycle integer
---@field attachment_state integer
---@field milestones integer
---@field media_epoch integer
---@field last_media_id integer
---@field last_media_record_sequence integer
---@field last_decoded_pts_us integer
---@field last_presented_pts_us integer
---@field last_presentation_id integer
---@field cumulative_body_bytes integer
---@field cumulative_media_records integer
---@field maximum_body_bytes integer
---@field maximum_media_records integer
---@field ingress_depth_bucket integer
---@field playback_state vivid_sdk.Payload?
---@field playback_hold vivid_sdk.PlaybackHold?
---@field terminal_loss_code integer?
---@field audio_gain { raw: integer }?

---@class vivid_sdk.TrackSupport
---@field supported boolean
---@field selected_decoder string
---@field capability_generation integer
---@field effective_claims vivid_sdk.Payload

---@class vivid_sdk.AnchorStatus
---@field context_id integer
---@field anchor_id integer
---@field state integer Unknown (0), ready (1), or gone (2).
---@field target_generation integer?
---@field payload vivid_sdk.Payload

---How long the last sends waited, split by cause. The causes have opposite remedies.
---@class vivid_sdk.SendPressure
---@field rate_limited_us integer
---@field flow_limited_us integer
---@field transport_us integer
---@field records integer

---One microphone packet: 20 ms of 48 kHz mono s16LE.
---@class vivid_sdk.MicPacket
---@field epoch integer
---@field packet_id integer
---@field pts_us integer
---@field pcm vivid_sdk.Bytes

-------------------------------------------------------------------------------------------------
-- Session
-------------------------------------------------------------------------------------------------

---An established logical session. Dropping it is an unclean loss; `close` sends `GOODBYE`.
---@class vivid_sdk.Session
---@field closed boolean
local Session = {}

---End the session with a `GOODBYE` round trip.
function Session:close() end

---Close the lifecycle without a `GOODBYE`, releasing blocked senders.
function Session:abort() end

---@return vivid_sdk.SessionInfo
function Session:info() end

---Whether the presenter accepted this profile.
---@param profile string
---@return boolean
function Session:supports(profile) end

---@return integer
function Session:allocate_id() end

---The next session event, or `nil` when the queue is empty.
---@return vivid_sdk.SessionEvent?
function Session:take_event() end

---The next session event, waiting up to `timeout` seconds (default 30).
---@param timeout number?
---@return vivid_sdk.SessionEvent?
function Session:wait_event(timeout) end

---Iterate events. Ends at `connection_closed`, at a quiet `timeout` (default 30 s), or when the
---session is closed.
---@param timeout number?
---@return fun(): vivid_sdk.SessionEvent?
function Session:events(timeout) end

---The definition `create_surface` would send, from the SDK's surface builder.
---@param config vivid_sdk.SurfaceConfig
---@return vivid_sdk.SurfaceDefinition
function Session:build_surface_config(config) end

---@param config vivid_sdk.SurfaceConfig
---@return vivid_sdk.Surface
function Session:create_surface(config) end

---Replace a surface's definition. Only a real coordinate change advances its generation.
---@param surface vivid_sdk.Surface
---@param config vivid_sdk.SurfaceConfig
function Session:update_surface(surface, config) end

---Destroy a surface and retire its tracks.
---@param surface vivid_sdk.Surface
function Session:destroy_surface(surface) end

---@param surface vivid_sdk.Surface
---@return vivid_sdk.SurfaceStatus
function Session:query_surface(surface) end

---The configuration `create_track` would send, with the SDK builder's claims.
---@param surface vivid_sdk.Surface
---@param config vivid_sdk.TrackConfig
---@return vivid_sdk.TrackConfiguration
function Session:build_track_config(surface, config) end

---@param surface vivid_sdk.Surface
---@param config vivid_sdk.TrackConfig
---@return vivid_sdk.Track
function Session:create_track(surface, config) end

---Whether the presenter would admit this track, without creating it.
---@param surface vivid_sdk.Surface
---@param config vivid_sdk.TrackConfig
---@return vivid_sdk.TrackSupport
function Session:probe_track(surface, config) end

---@param track vivid_sdk.Track
function Session:destroy_track(track) end

---@param track vivid_sdk.Track
---@return vivid_sdk.TrackStatus
function Session:query_track(track) end

---Wait for a track condition (`WAIT_*`), up to `timeout` seconds (default and maximum 30).
---@param track vivid_sdk.Track
---@param condition integer
---@param value integer?
---@param timeout number?
---@return vivid_sdk.WaitSatisfied
function Session:wait_track(track, condition, value, timeout) end

---Activate this track into its configured slot at a compositor boundary.
---@param surface vivid_sdk.Surface
---@param track vivid_sdk.Track
---@param required_milestone integer? Default `MILESTONE_OUTPUT_READY`.
---@return integer presentation
function Session:activate_track(surface, track, required_milestone) end

---Activate a slot set atomically.
---@param surface vivid_sdk.Surface
---@param bindings vivid_sdk.SlotBinding[]
---@return integer presentation
function Session:activate_tracks(surface, bindings) end

---@param track vivid_sdk.Track
---@return vivid_sdk.TrackChannel
function Session:open_track_channel(track) end

---Start a fresh authenticated channel generation and return its channel.
---@param track vivid_sdk.Track
---@param reason integer
---@return vivid_sdk.TrackChannel
function Session:advance_channel(track, reason) end

---Recover a lost channel: advance, reopen, and send the key unit. The sender continues the
---track's packet IDs and epoch.
---@param track vivid_sdk.Track
---@param key_unit vivid_sdk.Bytes
---@return vivid_sdk.TrackSender
function Session:recover_channel(track, key_unit) end

---Place a surface in the terminal grid.
---@param surface vivid_sdk.Surface
---@param placement vivid_sdk.Placement
---@return vivid_sdk.SceneCommit
function Session:place_terminal_surface(surface, placement) end

---@param surface vivid_sdk.Surface
---@param node vivid_sdk.SceneNodeConfig
---@return vivid_sdk.SceneCommit
function Session:create_node(surface, node) end

---@param surface vivid_sdk.Surface
---@param node vivid_sdk.SceneNodeConfig
---@return vivid_sdk.SceneCommit
function Session:update_node(surface, node) end

---@param context_id integer
---@param node_id integer
---@return vivid_sdk.SceneCommit
function Session:delete_node(context_id, node_id) end

---The anchor marker a terminal host prints. Defaults: the root context and a fresh ID.
---@param context_id integer?
---@param anchor_id integer?
---@return string
function Session:anchor_marker(context_id, anchor_id) end

---The marker spelling a ConPTY host prints.
---@param context_id integer
---@param anchor_id integer
---@return string
function Session:conpty_anchor_marker(context_id, anchor_id) end

---@param context_id integer
---@param anchor_id integer
---@return vivid_sdk.AnchorStatus?
function Session:query_anchor(context_id, anchor_id) end

---@return vivid_sdk.Payload
function Session:query_session() end

---Start a timed track. Requires `timed-media-v1`.
---@param track vivid_sdk.Track
---@param options vivid_sdk.PlayOptions?
function Session:play(track, options) end

---@param track vivid_sdk.Track
function Session:pause(track) end

---Set gain as a micropercent: `2^32` is unity, `2^33` the protocol maximum.
---@param track vivid_sdk.Track
---@param raw integer
function Session:set_audio_gain(track, raw) end

---Discard media below a new epoch and keep the channel open.
---@param track vivid_sdk.Track
---@param new_epoch integer
function Session:flush(track, new_epoch) end

---Wait until the presenter has consumed everything sent so far.
---@param track vivid_sdk.Track
function Session:drain(track) end

---Open a desktop-input lane. Requires `desktop-input-v1`.
---@param lane_generation integer? Default 1.
---@return vivid_sdk.InputLane
function Session:open_input_lane(lane_generation) end

---@class vivid_sdk.FileDropBinding
---@field producer_epoch integer
---@field context_id integer
---@field surface_id integer
---@field surface_generation integer
---@field destination integer? `DESTINATION_*`; absent disables the binding.
---@field maximum_file_bytes integer
---@field maximum_record_body integer
---@field maximum_pending_offers integer?
---@field maximum_active_transfers integer?
---@field acceptance_timeout_us integer?
---@field idle_timeout_us integer?

---@class vivid_sdk.FileDropGrant
---@field producer_epoch integer
---@field grant_generation integer
---@field context_id integer
---@field surface_id integer
---@field surface_generation integer
---@field state integer
---@field destination integer?
---@field maximum_file_bytes integer
---@field maximum_pending_offers integer
---@field maximum_active_transfers integer
---@field maximum_record_body integer
---@field acceptance_timeout_us integer
---@field idle_timeout_us integer
---@field reason integer

---Enable, replace, or disable a surface's file-drop binding.
---@param binding vivid_sdk.FileDropBinding
---@return vivid_sdk.FileDropGrant
function Session:set_file_drop_binding(binding) end

---@class vivid_sdk.FileDropAcceptance
---@field drop vivid_sdk.FileDropTuple The complete identity the offer carried.
---@field transfer_id integer
---@field transfer_generation integer
---@field maximum_record_body integer
---@field initial_maximum_body_bytes integer
---@field initial_maximum_records integer

---@class vivid_sdk.FileDropAccepted
---@field drop_id integer
---@field transfer_id integer
---@field transfer_generation integer
---@field open_timeout_us integer

---Accept an offer, naming the transfer this receiver is about to read.
---@param acceptance vivid_sdk.FileDropAcceptance
---@return vivid_sdk.FileDropAccepted
function Session:accept_file_drop(acceptance) end

---Decline an offer or give up on an accepted one.
---@param drop vivid_sdk.FileDropTuple
---@param reason integer
function Session:cancel_file_drop(drop, reason) end

---@class vivid_sdk.FileTransferAdvance
---@field context_id integer
---@field surface_id integer
---@field drop_id integer
---@field transfer_id integer
---@field expected_generation integer
---@field new_generation integer
---@field committed_offset integer
---@field maximum_body_bytes integer
---@field maximum_records integer

---@class vivid_sdk.FileTransferAdvanced
---@field transfer_id integer
---@field generation integer
---@field committed_offset integer
---@field open_timeout_us integer

---Move a transfer onto a fresh generation after a resume, at a committed offset.
---@param advance vivid_sdk.FileTransferAdvance
---@return vivid_sdk.FileTransferAdvanced
function Session:advance_file_transfer(advance) end

---@class vivid_sdk.FileDropStatus
---@field drop_id integer
---@field state integer `DROP_*`.
---@field transfer_id integer
---@field generation integer
---@field committed_offset integer
---@field result integer?
---@field final_name string

---@param drop_id integer
---@return vivid_sdk.FileDropStatus
function Session:query_file_drop(drop_id) end

---@class vivid_sdk.IncomingTransferRequest
---@field context_id integer
---@field surface_id integer
---@field producer_epoch integer
---@field grant_generation integer
---@field surface_generation integer
---@field drop_id integer
---@field transfer_id integer
---@field transfer_generation integer
---@field declared_length integer
---@field maximum_record_body integer
---@field maximum_body_bytes integer
---@field maximum_records integer
---@field resume_offset integer?

---Take over the transfer connection the presenter opened for an acceptance.
---@param request vivid_sdk.IncomingTransferRequest
---@return vivid_sdk.IncomingFileTransfer
function Session:open_incoming_file_transfer(request) end

---@class vivid_sdk.ContextDefinition
---@field context_id integer
---@field parent_context_id integer
---@field operation_classes integer `OP_*` bits.
---@field label string?
---@field lifetime_us integer?
---@field contract integer[]? Every resource; default: the session's own contract.

---@class vivid_sdk.ContextReady
---@field context_id integer
---@field operation_classes integer
---@field contract integer[]
---@field lifetime_us integer
---@field revision integer

---Create a child context scoped to the operation classes and contract given.
---@param definition vivid_sdk.ContextDefinition
---@return vivid_sdk.ContextReady
function Session:create_context(definition) end

---@class vivid_sdk.SessionLeaseDefinition
---@field context_id integer
---@field lease_id integer
---@field permitted_profiles string[] Must include the session's target profile.
---@field activation_timeout_us integer?
---@field disconnect_grace_us integer?
---@field cleanup_policy integer? Default `CLEANUP_SUSPEND_ON_UNCLEAN_LOSS`.
---@field contract integer[]?

---@class vivid_sdk.SessionLeaseReady
---@field context_id integer
---@field lease_id integer
---@field state integer
---@field activation_timeout_us integer
---@field disconnect_grace_us integer
---@field cleanup_policy integer
---@field permitted_profiles string[]
---@field contract integer[]
---@field revision integer

---Mint a bounded session lease. The activation secret is the second return value and is never
---part of the lease table: hand it over through an authenticated channel, never a command line.
---@param definition vivid_sdk.SessionLeaseDefinition
---@return vivid_sdk.SessionLeaseReady lease
---@return string? activation_secret_hex
function Session:create_session_lease(definition) end

---@param context_id integer
---@param lease_id integer
function Session:revoke_session_lease(context_id, lease_id) end

---Choose which observation classes the presenter should report.
---@param mask integer
function Session:set_observation(mask) end

---@class vivid_sdk.ResumeIdentity
---@field context_id integer
---@field lease_id integer
---@field session_id integer
---@field resume_generation integer

---The identity a resuming producer needs. Root sessions are not resumable and raise.
---@return vivid_sdk.ResumeIdentity
function Session:prepare_resume() end

-------------------------------------------------------------------------------------------------
-- Surface, Track, TrackChannel
-------------------------------------------------------------------------------------------------

---Stable semantic, scene, policy, and input identity.
---@class vivid_sdk.Surface
---@field context_id integer
---@field id integer
---@field revision integer
---@field generation integer

---One immutable media configuration owned by a surface.
---@class vivid_sdk.Track
---@field context_id integer
---@field surface_id integer
---@field id integer
---@field kind "video"|"audio"|"raster"|"image"|"vector"
---@field revision integer
---@field channel_generation integer

---@class vivid_sdk.RasterOptions
---@field epoch integer?
---@field frame_id integer? Default: the channel's next frame ID. An explicit one continues the sequence.
---@field compress boolean?

---@class vivid_sdk.DeltaOverwrite
---@field op "overwrite"
---@field x integer
---@field y integer
---@field width integer
---@field height integer
---@field rgba vivid_sdk.Bytes

---@class vivid_sdk.DeltaCopy
---@field op "copy"
---@field destination_x integer
---@field destination_y integer
---@field width integer
---@field height integer
---@field source_x integer
---@field source_y integer

---@class vivid_sdk.DeltaOptions
---@field base_frame_id integer
---@field epoch integer?
---@field frame_id integer?
---@field pts_us integer?
---@field duration_us integer?
---@field compress boolean? Not for the adaptive form, which chooses itself.

---@class vivid_sdk.VideoPacket
---@field packet_id integer
---@field pts_us integer
---@field dts_us integer? Default `pts_us`.
---@field duration_us integer?
---@field key boolean?
---@field epoch integer?

---@class vivid_sdk.AudioPacket
---@field packet_id integer
---@field pts_us integer
---@field duration_us integer
---@field dts_us integer?
---@field epoch integer?
---@field trim_start_samples integer?
---@field trim_end_samples integer?

---One authenticated transport generation for a track.
---@class vivid_sdk.TrackChannel
---@field context_id integer
---@field surface_id integer
---@field track_id integer
---@field kind string
---@field generation integer
---@field closed boolean
local TrackChannel = {}

---Send one tightly packed sRGB RGBA8 frame; returns the record sequence.
---@param rgba vivid_sdk.Bytes
---@param options vivid_sdk.RasterOptions?
---@return integer
function TrackChannel:send_raster(rgba, options) end

---Send a frame, compressing only when the result is smaller than raw.
---@param rgba vivid_sdk.Bytes
---@param options { epoch: integer?, frame_id: integer? }?
---@return integer
function TrackChannel:send_raster_adaptive(rgba, options) end

---Send a delta against `base_frame_id`.
---@param operations (vivid_sdk.DeltaOverwrite|vivid_sdk.DeltaCopy)[]
---@param options vivid_sdk.DeltaOptions
---@return integer
function TrackChannel:send_raster_delta(operations, options) end

---@param operations (vivid_sdk.DeltaOverwrite|vivid_sdk.DeltaCopy)[]
---@param options vivid_sdk.DeltaOptions
---@return integer
function TrackChannel:send_raster_delta_adaptive(operations, options) end

---Send the one encoded image an image track carries.
---@param encoded vivid_sdk.Bytes
---@return integer
function TrackChannel:send_image(encoded) end

---@param data vivid_sdk.Bytes
---@param packet vivid_sdk.VideoPacket
---@return integer
function TrackChannel:send_video(data, packet) end

---@param data vivid_sdk.Bytes
---@param packet vivid_sdk.AudioPacket
---@return integer
function TrackChannel:send_audio(data, packet) end

---Ordered end-of-stream after the last media record.
---@return integer
function TrackChannel:eos() end

function TrackChannel:close() end

---@return vivid_sdk.ChannelEvent?
function TrackChannel:take_event() end

---The next reverse-channel event, waiting up to `timeout` seconds (default 30).
---@param timeout number?
---@return vivid_sdk.ChannelEvent?
function TrackChannel:wait_event(timeout) end

---Iterate reverse events until a quiet `timeout` or the channel closes.
---@param timeout number?
---@return fun(): vivid_sdk.ChannelEvent?
function TrackChannel:events(timeout) end

---@return vivid_sdk.SendPressure
function TrackChannel:take_send_pressure() end

---Whether a record of `body_length` bytes fits the flow window now.
---@param body_length integer
---@return boolean
function TrackChannel:media_credit_available(body_length) end

---Grant the presenter's microphone flow on an uplink track's channel.
function TrackChannel:grant_audio_input() end

---@return vivid_sdk.MicPacket?
function TrackChannel:take_audio_input() end

-------------------------------------------------------------------------------------------------
-- Pipeline
-------------------------------------------------------------------------------------------------

---@class vivid_sdk.SenderVideoPacket
---@field pts_us integer
---@field packet_id integer? Default: the sender's next.
---@field dts_us integer?
---@field duration_us integer?
---@field key boolean?
---@field epoch integer? Default: the sender's current.

---@class vivid_sdk.SenderAudioPacket
---@field pts_us integer
---@field duration_us integer
---@field packet_id integer?
---@field epoch integer?

---A sender that keeps packet IDs and the media epoch continuous across channel recovery.
---@class vivid_sdk.TrackSender
---@field generation integer
---@field detached boolean
local TrackSender = {}

---@return integer
function TrackSender:next_packet_id() end

---@return integer
function TrackSender:current_epoch() end

---@return integer
function TrackSender:bump_epoch() end

function TrackSender:detach() end

---@param data vivid_sdk.Bytes
---@param packet vivid_sdk.SenderVideoPacket
---@return integer
function TrackSender:send_video(data, packet) end

---@param data vivid_sdk.Bytes
---@param packet vivid_sdk.SenderAudioPacket
---@return integer
function TrackSender:send_audio(data, packet) end

---@class vivid_sdk.RateSnapshot
---@field configured_bits_per_second integer
---@field target_bits_per_second integer
---@field adjustments integer
---@field rate_limited_us integer
---@field flow_limited_us integer
---@field transport_us integer

---Encoder pacing fed by `take_send_pressure` observations.
---@class vivid_sdk.VideoRateControl
local VideoRateControl = {}

---@param bytes integer
---@param pressure vivid_sdk.SendPressure
function VideoRateControl:observe_send(bytes, pressure) end

---Tell the controller how far audio has fallen behind, so video yields to it.
---@param backlog_us integer
function VideoRateControl:observe_audio_backlog(backlog_us) end

---The encoder target, if it changed since the last poll.
---@return integer?
function VideoRateControl:poll() end

---@return vivid_sdk.RateSnapshot
function VideoRateControl:snapshot() end

---@class vivid_sdk.VideoRateControlModule
local VideoRateControlModule = {}

---@param configured_bits_per_second integer
---@return vivid_sdk.VideoRateControl
function VideoRateControlModule.new(configured_bits_per_second) end

-------------------------------------------------------------------------------------------------
-- Input lanes and file transfers
-------------------------------------------------------------------------------------------------

---@class vivid_sdk.InputBinding
---@field producer_epoch integer
---@field context_id integer Zero with a zero surface disables injection.
---@field surface_id integer
---@field surface_generation integer
---@field requested_classes integer `INPUT_CLASS_*` bits.
---@field reason integer?
---@field requested_watchdog_us integer?

---@class vivid_sdk.InputBindingStatus
---@field producer_epoch integer
---@field grant_generation integer
---@field context_id integer
---@field surface_id integer
---@field surface_generation integer
---@field effective_classes integer The classes actually granted; drive the injection gate from these.
---@field state integer
---@field reason integer
---@field watchdog_timeout_us integer

---One lane event: `input` (the presenter's exact payload, still to pass the host's gate),
---`renew`, `revoked`, `reset`, `lane_closed`, or `error`.
---@class vivid_sdk.InputLaneEvent
---@field kind string
---@field record_type integer?
---@field surface_id integer?
---@field payload vivid_sdk.Payload?
---@field producer_epoch integer?
---@field grant_generation integer?
---@field context_id integer?
---@field surface_generation integer?
---@field renewal_sequence integer?
---@field watchdog_timeout_us integer?
---@field reason integer?
---@field diagnostic string?
---@field code integer?
---@field message string?

---A desktop-input lane. Nothing here injects input.
---@class vivid_sdk.InputLane
---@field generation integer
---@field closed boolean
local InputLane = {}

---@param binding vivid_sdk.InputBinding
---@return vivid_sdk.InputBindingStatus
function InputLane:set_binding(binding) end

---@return vivid_sdk.InputLaneEvent?
function InputLane:take_event() end

---The next lane event, waiting up to `timeout` seconds (default 1).
---@param timeout number?
---@return vivid_sdk.InputLaneEvent?
function InputLane:wait_event(timeout) end

---Iterate lane events until `lane_closed` or a quiet `timeout`.
---@param timeout number?
---@return fun(): vivid_sdk.InputLaneEvent?
function InputLane:events(timeout) end

---Close the lane. The session stays usable.
function InputLane:close() end

---One event on an incoming transfer: `data` (write `bytes` at `offset`, then grant), `finished`,
---or `aborted`.
---@class vivid_sdk.TransferEvent
---@field kind string
---@field offset integer?
---@field bytes vivid_sdk.Bytes?
---@field final_length integer?
---@field reason integer?
---@field final_offset integer?

---@class vivid_sdk.TransferResult
---@field transfer_id integer
---@field transfer_generation integer
---@field result integer
---@field committed_length integer?
---@field final_name string?
---@field committed_path string? Requires `file-drop-path-v1`.

---The transfer connection for one accepted drop.
---@class vivid_sdk.IncomingFileTransfer
---@field closed boolean
local IncomingFileTransfer = {}

---Read the next event, blocking until one arrives or the read deadline passes.
---@return vivid_sdk.TransferEvent
function IncomingFileTransfer:read_event() end

---Bound every later read to `timeout` seconds; `nil` restores unbounded reads.
---@param timeout number?
function IncomingFileTransfer:set_read_deadline(timeout) end

---Return flow capacity to the sender after committing the bytes read so far.
---@param maximum_body_bytes integer
---@param maximum_records integer
function IncomingFileTransfer:grant(maximum_body_bytes, maximum_records) end

---@param result vivid_sdk.TransferResult
function IncomingFileTransfer:send_result(result) end

---@param reason integer
function IncomingFileTransfer:abort(reason) end

function IncomingFileTransfer:close() end

-------------------------------------------------------------------------------------------------
-- Orchestrators
-------------------------------------------------------------------------------------------------

---@class vivid_sdk.PaneImageOptions
---@field title string? Default "image".
---@field columns integer?
---@field rows integer?
---@field text_layer integer?

---One image in one terminal pane, over the SDK's own pane state machine.
---@class vivid_sdk.PaneSession
---@field closed boolean
---@field has_presentation boolean
local PaneSession = {}

---Present one complete PNG or JPEG, replacing any current presentation.
---@param encoded vivid_sdk.Bytes
---@param options vivid_sdk.PaneImageOptions?
function PaneSession:show_encoded_image(encoded, options) end

---Present one tightly packed sRGB RGBA8 frame, replacing any current presentation.
---@param width integer
---@param height integer
---@param rgba vivid_sdk.Bytes
---@param options vivid_sdk.PaneImageOptions?
function PaneSession:show_rgba(width, height, rgba, options) end

---Remove the current presentation; idempotent.
function PaneSession:clear() end

---Clear the presentation and close the session.
function PaneSession:close() end

---@class vivid_sdk.PaneSessionModule
local PaneSessionModule = {}

---@param options vivid_sdk.ConnectOptions?
---@return vivid_sdk.PaneSession
function PaneSessionModule.connect(options) end

---Adopt an established session, which the pane then owns.
---@param session vivid_sdk.Session
---@return vivid_sdk.PaneSession
function PaneSessionModule.from_session(session) end

---Live handles for a retained image. `close` removes it; `presentation.session:close()` ends the
---session cleanly so the presenter may keep the anchored image as a poster.
---@class vivid_sdk.ImagePresentation
---@field session vivid_sdk.Session
---@field surface vivid_sdk.Surface
---@field track vivid_sdk.Track
---@field channel vivid_sdk.TrackChannel
local ImagePresentation = {}

function ImagePresentation:close() end

---A desktop presentation: surface, node, video and optional audio tracks with their senders.
---@class vivid_sdk.DesktopSession
---@field closed boolean
local DesktopSession = {}

---@return vivid_sdk.Track
function DesktopSession:video_track() end

---@return vivid_sdk.Track?
function DesktopSession:audio_track() end

---@param data vivid_sdk.Bytes
---@param packet vivid_sdk.SenderVideoPacket
---@return integer
function DesktopSession:send_video(data, packet) end

---@param data vivid_sdk.Bytes
---@param packet vivid_sdk.SenderAudioPacket
---@return integer
function DesktopSession:send_audio(data, packet) end

---Wait for decoded-output readiness and activate the video and audio slots atomically.
function DesktopSession:activate_slots() end

function DesktopSession:close() end

-------------------------------------------------------------------------------------------------
-- Presenter
-------------------------------------------------------------------------------------------------

---@class vivid_sdk.presenter
---@field SKIP_UNDECODED_VIDEO string
---@field SKIP_NO_RETAINED_PIXELS string
---@field SKIP_NODE_HIDDEN string
local presenter = {}

---@class vivid_sdk.PresenterOptions
---@field desktop { width: integer, height: integer }? Serve a desktop target instead of a terminal.
---@field retained_bytes integer?

---Bind `unix:/absolute/path` or `tcp:127.0.0.1:PORT` (loopback only; port 0 is ephemeral).
---@param endpoint string
---@param options vivid_sdk.PresenterOptions?
---@return vivid_sdk.Presenter
function presenter.start(endpoint, options) end

---The complete owner tuple of one track.
---@class vivid_sdk.SourceKey
---@field producer integer
---@field context integer
---@field surface integer
---@field track integer

---@class vivid_sdk.CaptureContent
---@field kind "raster"|"encoded_image"
---@field epoch integer?
---@field frame_id integer?
---@field width integer?
---@field height integer?
---@field rgba vivid_sdk.Bytes? A copy of the retained pixels.
---@field data vivid_sdk.Bytes? The encoded still.

---@class vivid_sdk.CaptureLayer
---@field source vivid_sdk.SourceKey
---@field node_id integer
---@field z_index integer
---@field x integer
---@field y integer
---@field width integer
---@field height integer
---@field clip { x: integer, y: integer, width: integer, height: integer }?
---@field content vivid_sdk.CaptureContent

---@class vivid_sdk.SkippedSource
---@field source vivid_sdk.SourceKey
---@field node_id integer
---@field reason string `SKIP_*`.

---@class vivid_sdk.PaneCapture
---@field layers vivid_sdk.CaptureLayer[]
---@field skipped vivid_sdk.SkippedSource[]

---@class vivid_sdk.PaneMediaSummary
---@field surfaces string[]
---@field tracks { source: vivid_sdk.SourceKey, kind: string, capturable: boolean }[]

---@class vivid_sdk.PanePosition
---@field decoder_reset_serial integer
---@field playing boolean
---@field start_pts_us integer
---@field state integer
---@field clock_pts_us integer?
---@field decoded_pts_us integer
---@field presented_pts_us integer
---@field presentation_id integer

---@class vivid_sdk.MediaResource
---@field pinned boolean
---@field producer integer
---@field context integer
---@field surface integer
---@field surface_revision integer
---@field surface_generation integer
---@field track { track_id: integer, revision: integer, channel_generation: integer, media_epoch: integer, capturable: boolean }?

---A running terminating presenter. Reading a pane is pull-based.
---@class vivid_sdk.Presenter
---@field endpoint string The served endpoint, with an ephemeral port resolved.
---@field closed boolean
local Presenter = {}

---Stop serving. Idempotent.
function Presenter:close() end

---Mint the capability a producer authenticates to this pane with. Hand it to exactly one
---producer, never through a command line or a log.
---@param pane integer
---@return string
function Presenter:issue_pane_capability(pane) end

---@param pane integer
function Presenter:revoke_pane(pane) end

---@param pane integer
---@param metrics { columns: integer, rows: integer, cell_width: integer?, cell_height: integer? }
function Presenter:update_metrics(pane, metrics) end

---Block until the pane holds something a capture could compose, or `timeout` seconds pass.
---@param pane integer
---@param timeout number
---@return boolean
function Presenter:wait_for_media(pane, timeout) end

---Compose the producer's retained surfaces. Not a screenshot.
---@param pane integer
---@param viewport_offset integer?
---@return vivid_sdk.PaneCapture
function Presenter:capture_pane(pane, viewport_offset) end

---@param pane integer
---@return vivid_sdk.PaneMediaSummary
function Presenter:pane_media_summary(pane) end

---@param source vivid_sdk.SourceKey
---@param generation integer
---@param pcm vivid_sdk.Bytes
---@return boolean
function Presenter:queue_microphone(source, generation, pcm) end

function Presenter:revoke_microphones() end

---@param reason_mask integer
---@return integer
function Presenter:notify_capabilities_changed(reason_mask) end

---@param pane integer
---@param width integer
---@param height integer
---@param reason_mask integer
---@return integer
function Presenter:update_desktop_target(pane, width, height, reason_mask) end

---@param pane integer
---@param value string
---@param row integer
---@param column integer
---@param alternate boolean
function Presenter:observe_marker(pane, value, row, column, alternate) end

---@param pane integer
---@param lines integer
---@param alternate boolean
function Presenter:scroll_anchors(pane, lines, alternate) end

---@param pane integer
---@param alternate boolean
function Presenter:clear_anchors(pane, alternate) end

---@param pane integer
---@param alternate boolean
function Presenter:set_alternate_screen(pane, alternate) end

---@param source vivid_sdk.SourceKey
---@return integer?
function Presenter:pane_for_source(source) end

---@return integer
function Presenter:projection_revision() end

---@param source vivid_sdk.SourceKey
---@param minimum_epoch integer?
---@param reason integer
---@return "forwarded"|"damped"|"ignored"
function Presenter:request_keyframe(source, minimum_epoch, reason) end

---@param sources vivid_sdk.SourceKey[]
---@param reason integer
function Presenter:request_full_frames(sources, reason) end

---@param source vivid_sdk.SourceKey
---@param position vivid_sdk.PanePosition
function Presenter:apply_outer_position(source, position) end

---@param source vivid_sdk.SourceKey
---@param decoder_reset_serial integer
---@param state integer
---@param eos_state integer
function Presenter:apply_outer_playback(source, decoder_reset_serial, state, eos_state) end

---Mint a media resource ID: `pinned` freezes the content, otherwise it follows the surface.
---@param source vivid_sdk.SourceKey
---@param pinned boolean
---@return string
function Presenter:announce_media_resource(source, pinned) end

---@param id string
---@return vivid_sdk.MediaResource
function Presenter:describe_media_resource(id) end

---@param id string
---@return boolean
function Presenter:release_media_resource(id) end

-------------------------------------------------------------------------------------------------
-- Overlay
-------------------------------------------------------------------------------------------------

---@class vivid_sdk.Point
---@field x number
---@field y number

---@class vivid_sdk.Rect
---@field x number
---@field y number
---@field width number
---@field height number

---@class vivid_sdk.Viewport
---@field width number
---@field height number
---@field scale_numerator integer
---@field scale_denominator integer

---Pane overlays: frameless vector windows the host draws and hit tests.
---@class vivid_sdk.overlay
---@field Path vivid_sdk.overlay.PathModule
---@field Brush vivid_sdk.overlay.BrushModule
---@field Canvas vivid_sdk.overlay.CanvasModule
---@field Modifiers { SHIFT: integer, CONTROL: integer, ALT: integer, SUPER: integer, CAPS_LOCK: integer, NUM_LOCK: integer, KNOWN_MASK: integer }
---@field MouseButton { PRIMARY: integer, AUXILIARY: integer, SECONDARY: integer, BACK: integer, FORWARD: integer, MAXIMUM: integer }
---@field Key { UNMAPPED: integer, FIRST_USAGE: integer, LAST_USAGE: integer }
local overlay = {}

---A session offering the complete overlay profile bundle.
---@param options vivid_sdk.ConnectOptions?
---@return vivid_sdk.overlay.OverlaySession
function overlay.connect(options) end

---Adopt an established session that negotiated the overlay profiles.
---@param session vivid_sdk.Session
---@return vivid_sdk.overlay.OverlaySession
function overlay.from_session(session) end

---@param x number
---@param y number
---@param width number
---@param height number
---@return vivid_sdk.Rect
function overlay.rect(x, y, width, height) end

---@param x number
---@param y number
---@return vivid_sdk.Point
function overlay.point(x, y) end

---A path builder. Canvas commands snapshot it; a coordinate the wire cannot carry is reported
---when the path is used.
---@class vivid_sdk.overlay.Path
---@field length integer Segments added so far.
local Path = {}

---@param x number
---@param y number
---@return vivid_sdk.overlay.Path
function Path:move_to(x, y) end

---@param x number
---@param y number
---@return vivid_sdk.overlay.Path
function Path:line_to(x, y) end

---@param cx number
---@param cy number
---@param x number
---@param y number
---@return vivid_sdk.overlay.Path
function Path:quad_to(cx, cy, x, y) end

---@param ax number
---@param ay number
---@param bx number
---@param by number
---@param x number
---@param y number
---@return vivid_sdk.overlay.Path
function Path:cubic_to(ax, ay, bx, by, x, y) end

---@return vivid_sdk.overlay.Path
function Path:close() end

---@class vivid_sdk.overlay.PathModule
local PathModule = {}

---@param options { even_odd: boolean? }?
---@return vivid_sdk.overlay.Path
function PathModule.new(options) end

---@param bounds vivid_sdk.Rect
---@return vivid_sdk.overlay.Path
function PathModule.rectangle(bounds) end

---@param bounds vivid_sdk.Rect
---@param radius number
---@return vivid_sdk.overlay.Path
function PathModule.rounded_rectangle(bounds, radius) end

---@param bounds vivid_sdk.Rect
---@param radii number[] Four radii, clockwise from the top left.
---@return vivid_sdk.overlay.Path
function PathModule.rounded_rectangle_corners(bounds, radii) end

---@param bounds vivid_sdk.Rect
---@return vivid_sdk.overlay.Path
function PathModule.ellipse(bounds) end

---A validated paint. Colors are straight-alpha sRGB `0xRRGGBBAA`.
---@class vivid_sdk.overlay.Brush

---@class vivid_sdk.overlay.GradientStop
---@field offset number 0 to 1.
---@field color integer

---@alias vivid_sdk.overlay.GradientSpace "srgb"|"oklab"

---@class vivid_sdk.overlay.BrushModule
local BrushModule = {}

---@param color integer
---@return vivid_sdk.overlay.Brush
function BrushModule.solid(color) end

---@param start vivid_sdk.Point
---@param finish vivid_sdk.Point
---@param stops vivid_sdk.overlay.GradientStop[]
---@param space vivid_sdk.overlay.GradientSpace? Anything but srgb needs overlay-paint-v1.
---@return vivid_sdk.overlay.Brush
function BrushModule.linear(start, finish, stops, space) end

---@param center vivid_sdk.Point
---@param radius number
---@param stops vivid_sdk.overlay.GradientStop[]
---@param space vivid_sdk.overlay.GradientSpace?
---@return vivid_sdk.overlay.Brush
function BrushModule.radial(center, radius, stops, space) end

---Fill with an uploaded image (overlay-paint-v1).
---@param image vivid_sdk.overlay.RetainedImage
---@param transform number[]? Six affine terms.
---@param extend "pad"|"repeat"|"reflect"?
---@return vivid_sdk.overlay.Brush
function BrushModule.image(image, transform, extend) end

---@class vivid_sdk.overlay.Shadow
---@field rect vivid_sdk.Rect
---@field radii number[]? Four, clockwise from the top left.
---@field color integer? Default opaque black.
---@field offset vivid_sdk.Point?
---@field blur number?
---@field spread number?
---@field inset boolean?

---@class vivid_sdk.overlay.StrokeStyle
---@field width number
---@field cap "butt"|"round"|"square"?
---@field join "miter"|"bevel"|"round"?
---@field miter_limit number?
---@field dashes number[]? Alternating on and off lengths.
---@field dash_offset number?

---@class vivid_sdk.overlay.TextOptions
---@field family string? Empty asks the host for its default.
---@field weight integer?
---@field italic boolean?
---@field max_width number?

---@alias vivid_sdk.overlay.HitRole "input"|"drag"|"resize"|"transparent"

---A display list. Every drawing method returns the canvas.
---@class vivid_sdk.overlay.Canvas
---@field length integer Commands so far.
local Canvas = {}

---@return vivid_sdk.overlay.Canvas
function Canvas:snapshot() end

function Canvas:validate() end

---@param path vivid_sdk.overlay.Path
---@param brush vivid_sdk.overlay.Brush
---@return vivid_sdk.overlay.Canvas
function Canvas:fill(path, brush) end

---A plain stroke, which needs no paint profile.
---@param path vivid_sdk.overlay.Path
---@param brush vivid_sdk.overlay.Brush
---@param width number
---@return vivid_sdk.overlay.Canvas
function Canvas:stroke(path, brush, width) end

---A stroke with caps, joins, and dashes (overlay-paint-v1).
---@param path vivid_sdk.overlay.Path
---@param brush vivid_sdk.overlay.Brush
---@param style vivid_sdk.overlay.StrokeStyle
---@return vivid_sdk.overlay.Canvas
function Canvas:stroke_styled(path, brush, style) end

---One blurred rounded rectangle (overlay-paint-v1).
---@param shadow vivid_sdk.overlay.Shadow
---@return vivid_sdk.overlay.Canvas
function Canvas:shadow(shadow) end

---@return vivid_sdk.overlay.Canvas
function Canvas:save() end

---@return vivid_sdk.overlay.Canvas
function Canvas:restore() end

---@param value number 0 to 1.
---@return vivid_sdk.overlay.Canvas
function Canvas:opacity(value) end

---@param a number
---@param b number
---@param c number
---@param d number
---@param e number
---@param f number
---@return vivid_sdk.overlay.Canvas
function Canvas:transform(a, b, c, d, e, f) end

---@param path vivid_sdk.overlay.Path
---@return vivid_sdk.overlay.Canvas
function Canvas:clip(path) end

---Host-shaped text.
---@param text string
---@param origin vivid_sdk.Point
---@param size number
---@param color integer
---@param options vivid_sdk.overlay.TextOptions?
---@return vivid_sdk.overlay.Canvas
function Canvas:text(text, origin, size, color, options) end

---An application hit region. Resize edges: left 1, top 2, right 4, bottom 8.
---@param application_id integer Nonzero, unique within the display list.
---@param path vivid_sdk.overlay.Path
---@param role vivid_sdk.overlay.HitRole?
---@param options { edges: integer?, cursor: string? }?
---@return vivid_sdk.overlay.Canvas
function Canvas:hit(application_id, path, role, options) end

---@class vivid_sdk.overlay.CanvasModule
local CanvasModule = {}

---@return vivid_sdk.overlay.Canvas
function CanvasModule.new() end

---@class vivid_sdk.overlay.WindowOptions
---@field bounds vivid_sdk.Rect
---@field mode "floating"|"popup"|"modal"?
---@field title string?
---@field visible boolean?
---@field min_width number?
---@field min_height number?

---One overlay lane event, named by `kind`: `pointer`, `hover`, `wheel`, `key`, `text`, `ime`,
---`focus`, `geometry`, `dismissed`, `cancel`, `accessibility`, `environment`, `viewport`,
---`submission-outcome`, or `connection-lost`. Text offsets are UTF-8 byte offsets.
---@class vivid_sdk.overlay.Event
---@field kind string
---@field scene_revision integer
---@field position vivid_sdk.Point?
---@field application_id integer?
---@field modifiers integer?
---@field button integer?
---@field down boolean?
---@field clicks integer?
---@field pressure number?
---@field entered boolean?
---@field dx number?
---@field dy number?
---@field precise boolean?
---@field phase string?
---@field physical integer?
---@field ["repeat"] boolean?
---@field text string?
---@field preedit string?
---@field selection { start: integer, ["end"]: integer }?
---@field focused boolean?
---@field bounds vivid_sdk.Rect?
---@field settled boolean?
---@field reason string?
---@field action string?
---@field environment { font_family: string, font_size: number, appearance: "light"|"dark", reduced_motion: boolean?, refresh_interval_us: number? }?
---@field revision integer?
---@field viewport vivid_sdk.Viewport?
---@field outcome "presented"|"superseded"?
---@field diagnostic string?
local Event = {}

---Whether this event belongs to `window`. One session may own many windows.
---@param window vivid_sdk.overlay.OverlayWindow
---@return boolean
function Event:targets(window) end

---Owner of overlay windows and their interactive lane.
---@class vivid_sdk.overlay.OverlaySession
---@field closed boolean
local OverlaySession = {}

---@param options vivid_sdk.overlay.WindowOptions
---@param parent vivid_sdk.overlay.OverlayWindow?
---@return vivid_sdk.overlay.OverlayWindow
function OverlaySession:create_window(options, parent) end

---@param window vivid_sdk.overlay.OverlayWindow
---@param capture boolean? Default true.
function OverlaySession:capture_pointer(window, capture) end

---Wait at most `timeout` seconds (0 to 60, default 0.25).
---@param timeout number?
---@return vivid_sdk.overlay.Event?
function OverlaySession:wait_event(timeout) end

---Iterate events until the session closes or its connection is lost.
---@param timeout number?
---@return fun(): vivid_sdk.overlay.Event?
function OverlaySession:events(timeout) end

function OverlaySession:close() end

---@class vivid_sdk.overlay.RetainedImage
---@field id integer The channel-qualified asset identity.

---A specific submission. A timeout leaves the receipt usable; lane loss raises.
---@class vivid_sdk.overlay.Submission
---@field revision integer
local Submission = {}

---@param timeout number? 0 to 60 seconds, default 0.25.
---@return ("presented"|"superseded")?
function Submission:wait(timeout) end

---@class vivid_sdk.overlay.TextGeometry
---@field start integer UTF-8 byte offset, inclusive.
---@field ["end"] integer UTF-8 byte offset, exclusive.
---@field bounds vivid_sdk.Rect
---@field baseline number
---@field rtl boolean

---@class vivid_sdk.overlay.TextMeasurement
---@field width number
---@field height number
---@field lines vivid_sdk.overlay.TextGeometry[]
---@field clusters vivid_sdk.overlay.TextGeometry[]
---@field truncated_at integer? UTF-8 byte offset of an ellipsized layout's cutoff.

---@class vivid_sdk.overlay.TextStyle
---@field size number?
---@field family string?
---@field weight integer?
---@field italic boolean?
---@field color integer?
---@field underline boolean?
---@field strikethrough boolean?

---@class vivid_sdk.overlay.StyledText
---@field runs { text: string, style: vivid_sdk.overlay.TextStyle? }[]
---@field max_width number?
---@field alignment "start"|"center"|"end"|"justify"?
---@field wrap boolean?
---@field max_lines integer?
---@field overflow "clip"|"ellipsis"?
---@field letter_spacing number?
---@field word_spacing number?
---@field line_height number?
---@field ligatures boolean?
---@field kerning boolean?

---An opaque, immutable host layout. Release it through its window.
---@class vivid_sdk.overlay.TextLayout
---@field measurement vivid_sdk.overlay.TextMeasurement

---@class vivid_sdk.overlay.SemanticNode
---@field id integer The application's own ID; actions arrive naming it.
---@field role string
---@field bounds vivid_sdk.Rect
---@field label string?
---@field numeric number[]? Value, minimum, maximum.
---@field level integer?
---@field set integer[]? Position and size.
---@field toggled "off"|"on"|"mixed"?
---@field disabled boolean?
---@field actions string[]?
---@field children integer[]?

---@class vivid_sdk.overlay.WindowStatus
---@field bounds vivid_sdk.Rect
---@field viewport vivid_sdk.Viewport
---@field viewport_revision integer
---@field window_revision integer
---@field presented_revision integer
---@field accepted_revision integer
---@field active_revision integer?
---@field focused boolean

---A frameless window with one surface, vector track, and bulk channel.
---@class vivid_sdk.overlay.OverlayWindow
---@field closed boolean
local OverlayWindow = {}

---Submit an atomic snapshot; success does not acknowledge GPU presentation.
---@param canvas vivid_sdk.overlay.Canvas
function OverlayWindow:present(canvas) end

---@param canvas vivid_sdk.overlay.Canvas
---@return vivid_sdk.overlay.Submission
function OverlayWindow:submit(canvas) end

---Prime and activate a fresh track; old retained images cannot appear in the canvas.
---@param canvas vivid_sdk.overlay.Canvas
---@return vivid_sdk.overlay.Submission
function OverlayWindow:replace_track(canvas) end

---@return vivid_sdk.overlay.WindowStatus
function OverlayWindow:reconcile() end

---@param bounds vivid_sdk.Rect
function OverlayWindow:set_bounds(bounds) end

---@param visible boolean
function OverlayWindow:set_visible(visible) end

function OverlayWindow:center() end

function OverlayWindow:request_focus() end

function OverlayWindow:raise() end

function OverlayWindow:lower() end

function OverlayWindow:close() end

---@return vivid_sdk.Rect
function OverlayWindow:bounds() end

---@return vivid_sdk.Viewport
function OverlayWindow:viewport() end

---@param width integer
---@param height integer
---@param rgba vivid_sdk.Bytes
---@return vivid_sdk.overlay.RetainedImage
function OverlayWindow:upload_rgba(width, height, rgba) end

---@param image vivid_sdk.overlay.RetainedImage
function OverlayWindow:release_image(image) end

---@param canvas vivid_sdk.overlay.Canvas
---@param image vivid_sdk.overlay.RetainedImage
---@param bounds vivid_sdk.Rect
---@param opacity number? 0 to 1.
function OverlayWindow:draw_image(canvas, image, bounds, opacity) end

---@param text string
---@param size number
---@param options vivid_sdk.overlay.TextOptions?
---@return vivid_sdk.overlay.TextMeasurement
function OverlayWindow:measure_text(text, size, options) end

---@param texts vivid_sdk.overlay.StyledText[]
---@return vivid_sdk.overlay.TextMeasurement[]
function OverlayWindow:measure_text_batch(texts) end

---@param texts vivid_sdk.overlay.StyledText[]
---@return vivid_sdk.overlay.TextLayout[]
function OverlayWindow:layout_text_batch(texts) end

---@param text vivid_sdk.overlay.StyledText
---@return vivid_sdk.overlay.TextLayout
function OverlayWindow:layout_text(text) end

---@param canvas vivid_sdk.overlay.Canvas
---@param layout vivid_sdk.overlay.TextLayout
---@param origin vivid_sdk.Point
function OverlayWindow:draw_text_layout(canvas, layout, origin) end

---@param layout vivid_sdk.overlay.TextLayout
function OverlayWindow:release_text_layout(layout) end

---Publish the accessibility tree for the scene revision it describes.
---@param semantics { scene_revision: integer, nodes: vivid_sdk.overlay.SemanticNode[] }
function OverlayWindow:set_semantics(semantics) end

---Place text on the clipboard; honored only just after a press delivered to this window.
---@param text string
function OverlayWindow:set_clipboard(text) end

---@param scene_revision integer
---@param caret vivid_sdk.Rect?
function OverlayWindow:set_editor_geometry(scene_revision, caret) end

-------------------------------------------------------------------------------------------------
-- Automation
-------------------------------------------------------------------------------------------------

---Drive vivido, vivida, and vvmux over their local automation endpoints. Unix only.
---@class vivid_sdk.automation
---@field PROTOCOL_VERSION integer
---@field VVMX_VERSION integer
---@field null lightuserdata An explicit JSON null in a request.
local automation = {}

---@class vivid_sdk.automation.VividoOptions
---@field socket string? An explicit socket path, which wins.
---@field target string? A named instance; one that is gone is an error, never a fall-through.
---@field timeout number? Seconds for each read and write.

---Connect to a vivido or vivida instance, resolving the endpoint as the CLI does.
---@param options vivid_sdk.automation.VividoOptions?
---@return vivid_sdk.automation.VividoSession
function automation.vivido_connect(options) end

---Every live Vivido instance this user can reach.
---@return table[]
function automation.vivido_instances() end

---Connect to a vvmux session server by name.
---@param target string? Default "default".
---@param options { timeout: number? }?
---@return vivid_sdk.automation.VvmuxSession
function automation.vvmux_connect(target, options) end

---Mark a table as a JSON array, so an empty one is sent as `[]` rather than `{}`.
---@param list table?
---@return table
function automation.array(list) end

---Check a session name against the runtimes' rule; returns it or raises `invalid_session_name`.
---@param name string
---@return string
function automation.validate_session_name(name) end

---One connection to a vivido or vivida instance.
---@class vivid_sdk.automation.VividoSession
---@field capabilities table The hello document: methods, event kinds, error codes, limits.
---@field closed boolean
local VividoSession = {}

---Issue one request. Params mirror the serde shape of the runtime's request struct.
---@param method string
---@param params table?
---@return any
function VividoSession:request(method, params) end

---End this connection. The runtime keeps running.
function VividoSession:close() end

---@class vivid_sdk.automation.Envelope
---@field pane_id integer?
---@field agent string?
---@field pane_name string?
---@field lease string?
---@field allow_focused boolean?
---@field expect table?
---@field idempotency_key string?

---One connection to a vvmux session server.
---@class vivid_sdk.automation.VvmuxSession
---@field closed boolean
local VvmuxSession = {}

---Issue one automation record, `{ method = "verb", ... }`, with the request's envelope beside it.
---@param method table
---@param envelope vivid_sdk.automation.Envelope?
---@return any
function VvmuxSession:request(method, envelope) end

function VvmuxSession:close() end

return vivid
