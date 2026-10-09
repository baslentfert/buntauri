#!/bin/bash
# Generate Implib.so stub archives (lib<name>-stub.a) for the GTK/WebKitGTK libraries,
# plus libbuntauri-stub-dlopen.a (the dlopen policy all stubs share).
#
#   scripts/linux-stubs/gen-stubs.sh <implib-dir> <out-dir>
#
# <implib-dir> is a checkout of https://github.com/yugr/Implib.so (implib-gen.py);
# needs the -dev packages of the libraries below (libgtk-3-dev libwebkit2gtk-4.1-dev).
# Point Bun's build at <out-dir> with BUNTAURI_LINUX_STUBS (see bun-glue/scripts/build/buntauri-libs.ts).
set -euo pipefail
implib=${1:?implib dir}
out=${2:?out dir}
mkdir -p "$out"
out=$(cd "$out" && pwd)
cd "$out"
libdir=/usr/lib/$(gcc -dumpmachine)
for name in webkit2gtk-4.1 gtk-3 gdk-3 pango-1.0 pangocairo-1.0 atk-1.0 cairo-gobject cairo gdk_pixbuf-2.0 soup-3.0 gio-2.0 javascriptcoregtk-4.1 gobject-2.0 glib-2.0 gmodule-2.0 harfbuzz; do
  so=$(readlink -f "$libdir/lib$name.so")
  soname=$(readelf -d "$so" | sed -n 's/.*SONAME.*\[\(.*\)\]/\1/p')
  python3 "$implib/implib-gen.py" -q --target "$(uname -m)" --dlopen-callback buntauri_stub_dlopen \
    --library-load-name "$soname" "$so" >/dev/null 2>"gen-$name.err" || { echo "implib FAILED for $name"; cat "gen-$name.err"; exit 1; }
  gcc -c -fPIC -O2 "$(basename "$so").tramp.S" -o "$name.tramp.o"
  gcc -c -fPIC -O2 "$(basename "$so").init.c" -o "$name.init.o"
  ar rcs "lib$name-stub.a" "$name.tramp.o" "$name.init.o"
done
# One dlopen policy for all stubs: load by soname; a missing library is reported, not fatal at startup.
cat > dlopen.c <<'C'
#include <dlfcn.h>
#include <stdio.h>
void *buntauri_stub_dlopen(const char *lib) {
  void *h = dlopen(lib, RTLD_LAZY | RTLD_GLOBAL);
  if (!h) fprintf(stderr, "buntauri: cannot load %s: %s\n", lib, dlerror());
  return h;
}
C
gcc -c -fPIC -O2 dlopen.c -o dlopen.o && ar rcs libbuntauri-stub-dlopen.a dlopen.o
echo "$(ls "$out"/*.a | wc -l) stub archives in $out"
