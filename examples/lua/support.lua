-- Small CLI and fixture helpers; SDK lifecycle stays in each example.

local vivid = require("vivid_sdk")

local support = {}

support.PIXELS = string.char(255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255)

local function usage(description, image)
  io.stderr:write(description, "\n\nusage: ", arg[0], image and " IMAGE" or "",
    " [--duration SECONDS]\n")
  os.exit(2)
end

-- `{ image = path?, duration = seconds? }` from the command line.
function support.arguments(description, options)
  options = options or {}
  local result, index = {}, 1
  while arg[index] do
    local value = arg[index]
    if value == "--duration" then
      local seconds = tonumber(arg[index + 1] or "")
      if not seconds or seconds ~= seconds or seconds < 0 or seconds > 3600 then
        io.stderr:write("duration must be 0..3600 seconds\n")
        os.exit(2)
      end
      result.duration = seconds
      index = index + 2
    elseif options.image and not result.image and value:sub(1, 2) ~= "--" then
      result.image = value
      index = index + 1
    else
      usage(description, options.image)
    end
  end
  if options.image and not result.image then
    usage(description, true)
  end
  return result
end

function support.read(path)
  local file = assert(io.open(path, "rb"))
  local bytes = file:read("*a")
  file:close()
  return bytes
end

function support.hold(duration)
  if duration then
    vivid.sleep(duration)
  else
    io.write("Press Enter to remove the presentation.")
    io.flush()
    io.read("*l")
  end
end

-- Run `body`, then `cleanup` whether or not `body` failed: Lua's try/finally.
function support.finally(body, cleanup)
  local ok, err = pcall(body)
  cleanup()
  if not ok then
    error(err, 0)
  end
end

return support
