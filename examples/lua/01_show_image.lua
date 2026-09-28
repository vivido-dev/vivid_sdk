-- Display a PNG/JPEG through PaneSession.
package.path = (arg[0]:match("^(.*)[/\\]") or ".") .. "/?.lua;" .. package.path
local vivid = require("vivid_sdk")
local support = require("support")

local args = support.arguments("Display a PNG/JPEG through PaneSession.", { image = true })
local encoded = support.read(args.image) -- Fail before connecting for a missing file.
local pane = vivid.PaneSession.connect()
support.finally(function()
  pane:show_encoded_image(encoded)
  support.hold(args.duration)
end, function()
  pane:close() -- Clears the presentation and closes the session.
end)
