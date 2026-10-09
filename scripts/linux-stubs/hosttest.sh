#!/bin/bash
# Run the stub-linked demo under xvfb; compare what parent and child have loaded; then kill the child.
f=/work/target/stub/debug/examples/demo
cd /tmp
BUNTAURI_SELFTEST=1 dbus-run-session -- xvfb-run -a $f > /tmp/demo.log 2>&1 &
sleep 8
procs() { for p in /proc/[0-9]*; do [ "$(readlink $p/exe 2>/dev/null)" = "$f" ] && echo ${p#/proc/}; done; }
host_env() { tr '\0' '\n' < /proc/$1/environ | grep -q '^BUNTAURI_UI_HOST='; }
parent=$(for p in $(procs); do host_env $p || echo $p; done | head -1)
child=$(for p in $(procs); do host_env $p && echo $p; done | head -1)
echo "parent=$parent child=$child"
for who in parent child; do
  pid=${!who}
  echo "$who: gtk=$(grep -c 'libgtk-3' /proc/$pid/maps) webkit=$(grep -c 'libwebkit2gtk' /proc/$pid/maps) jsc-gtk=$(grep -c 'libjavascriptcoregtk' /proc/$pid/maps) glib=$(grep -c 'libglib-2' /proc/$pid/maps)"
done
echo "--- kill child"
kill -9 $child
sleep 3
grep "^\[host\]" /tmp/demo.log | tail -8
[ -z "$(procs)" ] && echo "no demo processes left" || echo "left: $(procs)"
