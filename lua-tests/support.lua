-- Assertions and fixtures shared by the Lua tests. Plain Lua, so the suite runs on every Lua the
-- module builds for without a test framework to install.

local vivid = require("vivid_sdk")

local support = {}

-- Red, green, blue, white: four distinguishable pixels, so a capture that returned the wrong
-- buffer cannot pass by being uniformly coloured.
support.PIXELS = string.char(255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255)

-- A complete 1x1 RGBA PNG: signature, IHDR, one zlib IDAT, IEND, all with valid CRCs.
support.PNG = "\137\80\78\71\13\10\26\10\0\0\0\13\73\72\68\82\0\0\0\1\0\0\0\1\8\6\0\0\0\31\21\196"
  .. "\137\0\0\0\13\73\68\65\84\120\156\99\248\207\192\240\31\0\5\0\1\255\137\153\61\29\0\0\0"
  .. "\0\73\69\78\68\174\66\96\130"

-- Seconds; every wait in the suite is bounded.
support.TIMEOUT = 5

local SKIP = {}

function support.skip(reason)
  error(setmetatable({ reason = reason }, SKIP), 0)
end

function support.is_skip(err)
  return getmetatable(err) == SKIP
end

local function show(value)
  if type(value) == "string" then
    return string.format("%q", value)
  end
  return tostring(value)
end

function support.eq(actual, expected, message)
  if actual ~= expected then
    error(string.format("%sexpected %s, got %s", message and (message .. ": ") or "",
      show(expected), show(actual)), 2)
  end
end

function support.ok(value, message)
  if not value then
    error(message or "expected a true value", 2)
  end
  return value
end

local function deep_eq(a, b)
  if type(a) ~= "table" or type(b) ~= "table" then
    return a == b
  end
  for key, value in pairs(a) do
    if not deep_eq(value, b[key]) then
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
support.deep_eq = deep_eq

function support.same(actual, expected, message)
  if not deep_eq(actual, expected) then
    error((message or "tables differ"), 2)
  end
end

-- Call `fn(...)` and require an SDK error of `kind` whose message contains `pattern` (plain
-- text). Returns the decoded error information.
function support.raises(kind, pattern, fn, ...)
  local ok, err = pcall(fn, ...)
  if ok then
    error(string.format("expected a %s error, but the call succeeded", kind), 2)
  end
  local info = vivid.error_info(err)
  if not info then
    error("expected an SDK error, got " .. tostring(err), 2)
  end
  if info.kind ~= kind then
    error(string.format("expected a %s error, got %s: %s", kind, info.kind, info.message), 2)
  end
  if pattern and not info.message:find(pattern, 1, true) then
    error(string.format("expected %q in %q", pattern, info.message), 2)
  end
  return info
end

-- Any error at all, SDK or not; returns it.
function support.fails(fn, ...)
  local ok, err = pcall(fn, ...)
  if ok then
    error("expected the call to fail", 2)
  end
  return err
end

local function execute(command)
  -- Lua 5.1 and LuaJIT return a status code; later Luas return a success flag first.
  local result = os.execute(command)
  return result == true or result == 0
end
support.execute = execute

function support.shell_quote(text)
  return "'" .. text:gsub("'", "'\\''") .. "'"
end

-- A fresh, empty, owner-only directory under a short root: AF_UNIX paths are capped near 104
-- bytes, and a registry socket name alone carries a 32-hex digest.
function support.tempdir()
  local path = os.tmpname()
  os.remove(path)
  path = "/tmp/vvsdk-lua-" .. path:match("([^/]+)$")
  assert(execute("mkdir -m 700 " .. support.shell_quote(path)), "mkdir failed")
  return path
end

function support.remove_tree(path)
  execute("rm -rf " .. support.shell_quote(path))
end

function support.read_file(path)
  local file = assert(io.open(path, "rb"))
  local text = file:read("*a")
  file:close()
  return text
end

function support.write_file(path, text)
  local file = assert(io.open(path, "wb"))
  file:write(text)
  file:close()
end

-- Enough JSON to describe a fake server's script. Arrays are tables with a positive length.
local function encode(value)
  local kind = type(value)
  if kind == "nil" then
    return "null"
  elseif kind == "boolean" or kind == "number" then
    return tostring(value)
  elseif kind == "string" then
    return '"' .. value:gsub('[%c"\\]', function(c)
      return string.format("\\u%04x", c:byte())
    end) .. '"'
  elseif #value > 0 then
    local parts = {}
    for index = 1, #value do
      parts[index] = encode(value[index])
    end
    return "[" .. table.concat(parts, ",") .. "]"
  end
  local parts = {}
  for key, item in pairs(value) do
    parts[#parts + 1] = encode(tostring(key)) .. ":" .. encode(item)
  end
  return "{" .. table.concat(parts, ",") .. "}"
end
support.json = encode

-- The Python used for fake automation servers, or nil when there is none to use.
function support.python()
  local candidate = os.getenv("VIVID_TEST_PYTHON") or "python3"
  if execute(support.shell_quote(candidate) .. " -c '' >/dev/null 2>&1") then
    return candidate
  end
  return nil
end

return support
