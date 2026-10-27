#!/usr/bin/env bash
# Spike S3 driver: private session bus + private a11y bus + nested niri (moved to
# $GTK_MCP_TEST_NESTED_OUTPUT, focus restored), then app + AT-SPI client.
#   GTK_MCP_TEST_NESTED_OUTPUT=eDP-1 S3_BIN=<target>/debug/s3-a11y-join ./run.sh
set -u
OUT=${S3_OUT:-/tmp/s3run}
BIN=${S3_BIN:?set S3_BIN to the built s3-a11y-join binary}
[ -n "${GTK_MCP_TEST_NESTED_OUTPUT:-}" ] || { echo "set GTK_MCP_TEST_NESTED_OUTPUT (never use the working output)"; exit 2; }
rm -rf "$OUT"; mkdir -p "$OUT/runtime"; chmod 700 "$OUT/runtime"
HOST_SOCK="$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY"
PREV=$(niri msg --json focused-window | sed -n 's/.*"id":\([0-9]*\).*/\1/p' | head -1)
PIDS=()
cleanup() {
  for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null; done
  sleep 0.5
  for p in "${PIDS[@]}"; do kill -9 "$p" 2>/dev/null; done
}
trap cleanup EXIT

BUS="unix:path=$OUT/bus"
env -u DISPLAY -u WAYLAND_DISPLAY XDG_RUNTIME_DIR="$OUT/runtime" dbus-daemon --session --nofork --address="$BUS" >"$OUT/dbus.log" 2>&1 &
PIDS+=($!)
for _ in $(seq 50); do [ -S "$OUT/bus" ] && break; sleep 0.1; done
env -u DISPLAY -u WAYLAND_DISPLAY XDG_RUNTIME_DIR="$OUT/runtime" DBUS_SESSION_BUS_ADDRESS="$BUS" /usr/lib/at-spi-bus-launcher --launch-immediately >"$OUT/launcher.log" 2>&1 &
PIDS+=($!)
A11Y=""
for _ in $(seq 50); do
  A11Y=$(gdbus call --address "$BUS" --dest org.a11y.Bus --object-path /org/a11y/bus --method org.a11y.Bus.GetAddress 2>/dev/null | sed "s/^('\(.*\)',)$/\1/")
  [ -n "$A11Y" ] && break; sleep 0.1
done
echo "a11y bus: $A11Y"
env -u DISPLAY -u WAYLAND_DISPLAY XDG_RUNTIME_DIR="$OUT/runtime" DBUS_SESSION_BUS_ADDRESS="$BUS" /usr/lib/at-spi2-registryd >"$OUT/registryd.log" 2>&1 &
PIDS+=($!)
sleep 0.7

printf 'output "winit" {\n    mode "1280x800@60"\n    scale 1\n}\n' >"$OUT/niri.kdl"
env -u NIRI_SOCKET XDG_RUNTIME_DIR="$OUT/runtime" WAYLAND_DISPLAY="$HOST_SOCK" NIRI_CONFIG="$OUT/niri.kdl" DBUS_SESSION_BUS_ADDRESS="$BUS" niri >"$OUT/niri.log" 2>&1 &
NIRI=$!
PIDS+=($NIRI)
WL=""
for _ in $(seq 100); do
  WL=$(ls "$OUT/runtime" | grep -E '^wayland-[0-9]+$' | head -1)
  [ -n "$WL" ] && break; sleep 0.1
done
[ -n "$WL" ] || { echo "nested niri did not start"; cat "$OUT/niri.log" | head -20; exit 1; }
for _ in $(seq 50); do
  ID=$(niri msg --json windows | PID=$NIRI python3 -c 'import json,sys,os; w=[x for x in json.load(sys.stdin) if x["pid"]==int(os.environ["PID"])]; print(w[0]["id"] if w else "")')
  [ -n "$ID" ] && break; sleep 0.1
done
if [ -n "${ID:-}" ]; then
  niri msg action move-window-to-monitor --id "$ID" "$GTK_MCP_TEST_NESTED_OUTPUT"
  [ -n "$PREV" ] && niri msg action focus-window --id "$PREV"
fi

export DBUS_SESSION_BUS_ADDRESS="$BUS" XDG_RUNTIME_DIR="$OUT/runtime" WAYLAND_DISPLAY="$WL" GDK_BACKEND=wayland GSETTINGS_BACKEND=memory GTK_A11Y=atspi
unset DISPLAY NIRI_SOCKET
rm -f "$OUT/stop"
"$BIN" app "$OUT/gtk.json" "$OUT/stop" >"$OUT/app.log" 2>&1 &
APP=$!
PIDS+=($APP)
for _ in $(seq 200); do [ -f "$OUT/gtk.json" ] && break; sleep 0.1; done
[ -f "$OUT/gtk.json" ] || { echo "app did not dump"; cat "$OUT/app.log"; exit 1; }
timeout 90 "$BIN" client "$APP" "$OUT/gtk.json" "$OUT/report.json"
echo "client exit: $?"
touch "$OUT/stop"
sleep 0.5
echo "done; report: $OUT/report.json"
