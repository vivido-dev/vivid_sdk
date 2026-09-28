#!/bin/sh
# Build the native module and stage it as lua/vivid_sdk.so (vivid_sdk.dll on Windows), the file
# `require("vivid_sdk")` loads. Run from anywhere; then, from the SDK directory:
#
#   LUA_CPATH="lua/?.so;;" luajit -e 'print(require("vivid_sdk").VERSION)'
#
# Usage: lua/build.sh [--release] [--lua luajit|lua51|lua52|lua53|lua54|lua55]
#
# A module is built for exactly one Lua. LuaJIT is the default; pass --lua for the interpreter
# that will load it. LuaRocks users do not need this script: the rockspec builds and installs the
# module for the Lua LuaRocks targets.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
root=$(dirname "$here")
profile=debug
lua=luajit
while [ $# -gt 0 ]; do
  case "$1" in
    --release) profile=release ;;
    --lua) shift; lua=${1:?--lua needs a value} ;;
    *) echo "usage: $0 [--release] [--lua luajit|lua51|lua52|lua53|lua54|lua55]" >&2; exit 2 ;;
  esac
  shift
done
case "$lua" in
  luajit|lua51|lua52|lua53|lua54|lua55) ;;
  *) echo "unknown Lua feature: $lua" >&2; exit 2 ;;
esac

set -- --manifest-path "$root/lua-bindings/Cargo.toml" --no-default-features --features "$lua"
if [ "$profile" = release ]; then set -- "$@" --release; fi
cargo build "$@"

target=${CARGO_TARGET_DIR:-$root/lua-bindings/target}/$profile
case "$(uname -s)" in
  Darwin) built=$target/libvivid_sdk_lua.dylib staged=$here/vivid_sdk.so ;;
  MINGW*|MSYS*|CYGWIN*) built=$target/vivid_sdk_lua.dll staged=$here/vivid_sdk.dll ;;
  *) built=$target/libvivid_sdk_lua.so staged=$here/vivid_sdk.so ;;
esac
# A copy rather than a link: the next build for another Lua must not change a staged module
# underneath a running interpreter.
rm -f "$staged"
cp "$built" "$staged"
echo "staged $staged for $lua"
