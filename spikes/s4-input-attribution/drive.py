#!/usr/bin/env python3
"""Spike S4 driver: nested niri (placed on $GTK_MCP_TEST_NESTED_OUTPUT), the spike app inside it,
real input injected by wayland-mcp (virtual pointer/keyboard) against the nested compositor.
Usage: GTK_MCP_TEST_NESTED_OUTPUT=eDP-1 ./drive.py <spike-binary> [wayland-mcp-binary]"""
import glob, json, os, subprocess, sys, tempfile, time

APP = sys.argv[1]
WMCP = sys.argv[2] if len(sys.argv) > 2 else "/home/alex/projects/wayland-mcp/target/release/wayland-mcp"
OUT = os.environ["GTK_MCP_TEST_NESTED_OUTPUT"]

def host(*a):
    r = subprocess.run(["niri", "msg", "--json", *a], capture_output=True, text=True)
    return json.loads(r.stdout) if r.returncode == 0 and r.stdout.strip() else None

before = host("focused-window")
root = tempfile.mkdtemp(prefix="s4-niri-")
rt = os.path.join(root, "rt"); os.makedirs(rt, mode=0o700)
cfg = os.path.join(root, "config.kdl")
open(cfg, "w").write('output "winit" {\n    mode "1280x800@60"\n    scale 1\n}\n')
env = dict(os.environ, XDG_RUNTIME_DIR=rt, NIRI_CONFIG=cfg)
host_sock = os.environ["WAYLAND_DISPLAY"]
if not host_sock.startswith("/"): host_sock = os.environ["XDG_RUNTIME_DIR"] + "/" + host_sock
env["WAYLAND_DISPLAY"] = host_sock
env.pop("NIRI_SOCKET", None)
niri = subprocess.Popen(["niri"], env=env, stdout=open(root + "/niri.log", "w"), stderr=subprocess.STDOUT)
procs = [niri]
def cleanup():
    for p in procs:
        try: p.kill()
        except Exception: pass
import atexit; atexit.register(cleanup)

disp = None
for _ in range(200):
    c = [os.path.basename(p) for p in glob.glob(rt + "/wayland-*") if not p.endswith(".lock")]
    if c: disp = c[0]; break
    time.sleep(0.1)
assert disp, "nested niri did not start"
for _ in range(50):
    wins = host("windows") or []
    w = next((w for w in wins if w.get("pid") == niri.pid), None)
    if w:
        subprocess.run(["niri", "msg", "action", "move-window-to-monitor", "--id", str(w["id"]), OUT])
        if before: subprocess.run(["niri", "msg", "action", "focus-window", "--id", str(before["id"])])
        break
    time.sleep(0.1)
time.sleep(1)

nenv = dict(os.environ, XDG_RUNTIME_DIR=rt, WAYLAND_DISPLAY=disp, GDK_BACKEND="wayland")
nsock = glob.glob(rt + "/niri.*.sock")
nenv["NIRI_SOCKET"] = nsock[0] if nsock else ""
nenv.pop("DISPLAY", None)
app = subprocess.Popen([APP], env=nenv, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, bufsize=1)
procs.append(app)
lines = []
import threading
def pump():
    for l in app.stdout:
        lines.append((time.monotonic(), l.rstrip()))
threading.Thread(target=pump, daemon=True).start()

def wait_for(prefix, n=1, timeout=15):
    end = time.time() + timeout
    while time.time() < end:
        got = [l for _, l in lines if l.startswith(prefix)]
        if len(got) >= n: return got
        time.sleep(0.1)
    raise SystemExit(f"timeout waiting for {prefix}; lines={[l for _, l in lines]}")
wait_for("READY"); bounds = {l.split()[1]: list(map(float, l.split()[2:])) for l in wait_for("B ", 4)}

wenv = dict(nenv); wenv.pop("NIRI_SOCKET", None)
mcp = None
n = [1]
def start_mcp():
    global mcp
    if mcp:
        try: mcp.kill()
        except Exception: pass
    mcp = subprocess.Popen([WMCP], env=wenv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    procs.append(mcp)
    send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "s4", "version": "0"}}}); recv()
    send({"jsonrpc": "2.0", "method": "notifications/initialized"})
def send(o): mcp.stdin.write(json.dumps(o) + "\n"); mcp.stdin.flush()
def recv():
    l = mcp.stdout.readline()
    if not l: raise SystemExit("wayland-mcp EOF: " + mcp.stderr.read()[:2000])
    return json.loads(l)
def call(name, args={}):
    n[0] += 1; send({"jsonrpc": "2.0", "id": n[0], "method": "tools/call", "params": {"name": name, "arguments": args}}); r = recv()
    out = r.get("result", r)
    t = [c.get("text", "") for c in out.get("content", []) if c.get("type") == "text"]
    return t[0][:300] if t else str(r)[:300]
start_mcp()

wins = None
for _ in range(50):
    wins = subprocess.run(["niri", "msg", "--json", "windows"], env=nenv, capture_output=True, text=True)
    try:
        wl = json.loads(wins.stdout)
        if wl: break
    except Exception: pass
    time.sleep(0.2)
w = wl[0]
# The nested niri does not report the tile position yet; with its default 16 px gaps the first column starts at (16, 16).
ox = (w["layout"].get("tile_pos_in_workspace_view") or [16, 16])[0] + (w["layout"].get("window_offset_in_tile") or [0, 0])[0]
oy = (w["layout"].get("tile_pos_in_workspace_view") or [16, 16])[1] + (w["layout"].get("window_offset_in_tile") or [0, 0])[1]
def center(name):
    x, y, ww, hh = bounds[name]
    return ox + x + ww / 2, oy + y + hh / 2
def mark(s): lines.append((time.monotonic(), "M " + s))
def click(name):
    x, y = center(name)
    start_mcp()
    return call("click", {"x": x, "y": y, "space": "logical"})

mark("click entry"); click("entry")
time.sleep(0.5); mark("type abc"); print(call("type_text", {"text": "abc"}), file=sys.stderr)
time.sleep(0.5); mark("click button"); click("button")
time.sleep(0.5); mark("click switch"); click("switch")
time.sleep(0.5); mark("open menu"); click("menu")
time.sleep(0.8); mark("press Enter in popover (activates first item)")
print(call("key", {"keys": ["Down"]}), file=sys.stderr); print(call("key", {"keys": ["Return"]}), file=sys.stderr)
time.sleep(5)  # the app's own emissions fire at t=4 s
print("\n".join(l for _, l in lines))
