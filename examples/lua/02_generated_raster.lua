-- Display generated RGBA8 pixels without image libraries.
package.path = (arg[0]:match("^(.*)[/\\]") or ".") .. "/?.lua;" .. package.path
local vivid = require("vivid_sdk")
local support = require("support")

local args = support.arguments("Display generated RGBA8 pixels without image libraries.")
local pane = vivid.PaneSession.connect()
support.finally(function()
  pane:show_rgba(2, 2, support.PIXELS)
  support.hold(args.duration)
end, function()
  pane:close() -- Clears the presentation and closes the session.
end)
