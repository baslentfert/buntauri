#!/bin/bash
# Linker wrapper: link GTK/WebKitGTK through the stubs instead of NEEDED entries.
args=()
for a in "$@"; do
  case "$a" in
    -l*) n="${a#-l}"; if [ -f "/stubs/lib$n-stub.a" ]; then args+=("/stubs/lib$n-stub.a"); continue; fi ;;
  esac
  args+=("$a")
done
exec cc "${args[@]}" /stubs/libbuntauri-stub-dlopen.a -ldl
