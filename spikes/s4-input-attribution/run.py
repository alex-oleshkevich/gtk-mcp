#!/usr/bin/env python3
"""Spike S4 driver: private nested niri + the spike app + wayland-mcp (real
virtual pointer / keyboard) against that niri. Prints a measured table.

Display rule: the nested niri's window is moved to $GTK_MCP_TEST_NESTED_OUTPUT
(eDP-1) and the previously focused host window is refocused; nothing is ever
left on the user's working output. Everything started here is killed on exit.

  GTK_MCP_TEST_NESTED_OUTPUT=eDP-1 python3 run.py [spike-bin] [wayland-mcp-bin]
"""
import json, os, signal, subprocess, sys, tempfile, threading, time, shutil

SPIKE = sys.argv[1] if len(sys.argv) > 1 else "/home/alex/gm-work/target-D/s4/debug/s4-input-attribution"
WMCP = sys.argv[2] if len(sys.argv) > 2 else "/home/alex/projects/wayland-mcp/target/release/wayland-mcp"
OUTPUT = os.environ.get("GTK_MCP_TEST_NESTED_OUTPUT")
if not OUTPUT:
    sys.exit("set GTK_MCP_TEST_NESTED_OUTPUT (e.g. eDP-1)")

procs = []


def host(*args):
    r = subprocess.run(["niri", "msg", "--json", *args], capture_output=True, text=True)
    return json.loads(r.stdout) if r.returncode == 0 and r.stdout.strip() else None


def cleanup():
    for p in reversed(procs):
        try:
            os.killpg(p.pid, signal.SIGKILL)
        except Exception:
            pass
    shutil.rmtree(root, ignore_errors=True)


root = tempfile.mkdtemp(prefix="s4-")
rt = os.path.join(root, "runtime")
os.makedirs(rt, mode=0o700)
cfg = os.path.join(root, "config.kdl")
open(cfg, "w").write('output "winit" {\n    mode "1280x800@60"\n    scale 1\n}\n')
focused = host("focused-window")
prev = focused["id"] if focused else None
host_sock = os.environ["XDG_RUNTIME_DIR"] + "/" + os.environ["WAYLAND_DISPLAY"]
env = dict(os.environ, XDG_RUNTIME_DIR=rt, WAYLAND_DISPLAY=host_sock, NIRI_CONFIG=cfg)
env.pop("NIRI_SOCKET", None)
niri = subprocess.Popen(["niri"], env=env, stdout=open(root + "/niri.log", "w"), stderr=subprocess.STDOUT, start_new_session=True)
procs.append(niri)

try:
    t = time.time()
    disp = None
    while time.time() - t < 20 and not disp:
        for n in os.listdir(rt):
            if n.startswith("wayland-") and not n.endswith(".lock"):
                disp = n
        time.sleep(0.1)
    assert disp, "nested niri did not start: " + open(root + "/niri.log").read()[:500]
    # Move its host window off the working output, restore focus.
    t = time.time()
    while time.time() - t < 5:
        wins = host("windows") or []
        w = next((w for w in wins if w.get("pid") == niri.pid), None)
        if w:
            subprocess.run(["niri", "msg", "action", "move-window-to-monitor", "--id", str(w["id"]), OUTPUT])
            if prev is not None:
                subprocess.run(["niri", "msg", "action", "focus-window", "--id", str(prev)])
            break
        time.sleep(0.1)
    nested = dict(os.environ, XDG_RUNTIME_DIR=rt, WAYLAND_DISPLAY=disp, GDK_BACKEND="wayland", GTK_A11Y="none")
    nsock = next(os.path.join(rt, n) for n in os.listdir(rt) if n.startswith("niri.") and n.endswith(".sock"))

    def nested_niri(*a):
        r = subprocess.run(["niri", "msg", "--json", *a], capture_output=True, text=True, env=dict(os.environ, NIRI_SOCKET=nsock))
        return json.loads(r.stdout) if r.returncode == 0 and r.stdout.strip() else None

    events = []
    lock = threading.Lock()
    app = subprocess.Popen([SPIKE], env=nested, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1, start_new_session=True,
                           stderr=open(root + "/app.err", "w"))
    procs.append(app)

    def rd():
        for l in app.stdout:
            try:
                with lock:
                    events.append(json.loads(l))
            except Exception:
                pass

    threading.Thread(target=rd, daemon=True).start()

    def cmd(c):
        app.stdin.write(c + "\n")
        app.stdin.flush()

    def mark():
        with lock:
            return len(events)

    def since(m):
        with lock:
            return list(events[m:])

    def wait_for(pred, timeout=10):
        t = time.time()
        while time.time() - t < timeout:
            with lock:
                for e in events:
                    if pred(e):
                        return e
            time.sleep(0.05)
        raise SystemExit("timeout waiting for app event")

    wait_for(lambda e: e.get("k") == "ready")
    time.sleep(1.5)

    # wayland-mcp against the nested niri (its "host" target = our WAYLAND_DISPLAY)
    mcp = subprocess.Popen([WMCP], env=nested, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=open(root + "/mcp.err", "w"), text=True, bufsize=1, start_new_session=True)
    procs.append(mcp)
    nid = [0]

    def rpc(method, params=None, notify=False):
        msg = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            msg["params"] = params
        if not notify:
            nid[0] += 1
            msg["id"] = nid[0]
        mcp.stdin.write(json.dumps(msg) + "\n")
        mcp.stdin.flush()
        if notify:
            return None
        while True:
            l = mcp.stdout.readline()
            if not l:
                raise SystemExit("wayland-mcp closed: " + open(root + "/mcp.err").read()[:500])
            r = json.loads(l)
            if r.get("id") == msg["id"]:
                return r

    rpc("initialize", {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "s4", "version": "0"}})
    rpc("notifications/initialized", notify=True)

    def tool(name, **args):
        r = rpc("tools/call", {"name": name, "arguments": args})
        res = r.get("result") or {}
        text = " ".join(c.get("text", "") for c in res.get("content", []) if c.get("type") == "text")
        if r.get("error") or res.get("isError"):
            return {"error": r.get("error") or text}
        return {"ok": text[:200]}

    # --- calibration: window origin on the nested output --------------------
    wins = nested_niri("windows")
    print("nested niri window:", json.dumps(wins)[:400])
    wl = wins[0]["layout"]
    ox, oy = wl["tile_pos_in_workspace_view"] or [0, 0]
    cmd("layout")
    lay = wait_for(lambda e: e.get("k") == "layout")
    m = mark()
    print("move ->", tool("move", x=ox + 100, y=oy + 100, space="logical"))
    time.sleep(0.5)
    hv = [e for e in since(m) if e.get("k") == "hover"]
    print("hover after move to", (ox + 100, oy + 100), ":", hv[-1:] if hv else "NONE")
    if hv:
        ox += 100 - hv[-1]["x"]
        oy += 100 - hv[-1]["y"]
    print("calibrated window origin on output:", ox, oy, "layout:", lay)

    def center(name):
        x, y, w, h = lay[name]
        return ox + x + w / 2, oy + y + h / 2

    rows = []
    if os.environ.get("S4_DEBUG"):
        print("outputs:", json.dumps(nested_niri("outputs"))[:600])
        print("desktop_info:", tool("desktop_info"))
        for (x, y) in [(gx, gy) for gx in (50, 400, 800, 1200, 1400) for gy in (50, 400, 900, 1500, 1700)]:
            m = mark()
            tool("move", x=x, y=y, space="logical")
            time.sleep(0.25)
            print("move", (x, y), "->", [(e.get("x"), e.get("y")) for e in since(m)])

        print("NIRI LOG:", open(root + "/niri.log").read()[-1500:])
        print("MCP ERR:", open(root + "/mcp.err").read()[-800:])
        print("APP ERR:", open(root + "/app.err").read()[-500:])
        raise SystemExit

    def scenario(title, fn):
        cmd("noop")
        time.sleep(0.4)
        m = mark()
        res = fn()
        time.sleep(0.9)
        ev = since(m)
        ins = {}
        for e in ev:
            if e.get("k") == "input":
                ins[e["type"]] = ins.get(e["type"], 0) + 1
        hv = [(e["x"], e["y"]) for e in ev if e.get("k") == "hover"]
        if hv: ins["hover"] = f"{len(hv)} last={hv[-1]}"
        ems = [(e["sig"], e["w"], e["attr"], e["since_input_ms"], e["held"]) for e in ev if e.get("k") == "emit"]
        rows.append((title, res, ins, ems))
        print(f"\n## {title}\n  driver: {res}\n  controller saw: {ins}\n  emissions: {ems}")

    def click(name, **kw):
        x, y = center(name)
        return tool("click", x=x, y=y, space="logical", **kw)

    scenario("pointer click on GtkButton", lambda: click("btn"))
    scenario("pointer click on GtkEntry then type_text 'hi'", lambda: (click("entry"), tool("type_text", text="hi"))[1])
    scenario("pointer click on GtkSwitch", lambda: click("sw"))
    scenario("pointer click on GtkCheckButton", lambda: click("check"))
    scenario("pointer click on GtkToggleButton", lambda: click("toggle"))
    scenario("pointer click on GtkExpander title", lambda: tool("click", x=ox + lay["exp"][0] + 20, y=oy + lay["exp"][1] + 10, space="logical"))
    scenario("pointer click on GtkScale (jump to point)", lambda: tool("click", x=ox + lay["scale"][0] + lay["scale"][2] * 0.75, y=oy + lay["scale"][1] + lay["scale"][3] / 2, space="logical"))
    scenario("pointer drag on GtkScale", lambda: tool("drag", **{"from": {"x": ox + lay["scale"][0] + lay["scale"][2] * 0.3, "y": oy + lay["scale"][1] + lay["scale"][3] / 2, "space": "logical"},
                                                                  "to": {"x": ox + lay["scale"][0] + lay["scale"][2] * 0.6, "y": oy + lay["scale"][1] + lay["scale"][3] / 2, "space": "logical"}, "duration_ms": 600}))
    scenario("wheel scroll over GtkScale", lambda: tool("scroll", x=center("scale")[0], y=center("scale")[1], space="logical", dy=3))
    sx, sy, sw_, sh = lay["spin"]
    scenario("pointer click on GtkSpinButton up arrow", lambda: tool("click", x=ox + sx + sw_ - 12, y=oy + sy + sh / 4, space="logical"))
    sp_pt = dict(x=ox + sx + sw_ - 12, y=oy + sy + sh / 4, space="logical")

    def hold_spin():
        tool("move", **sp_pt)
        tool("mouse_button", button="left", pressed=True)
        time.sleep(1.6)
        return tool("mouse_button", button="left", pressed=False)

    scenario("hold button on GtkSpinButton arrow 1.6 s (auto-repeat timer, no further events)", hold_spin)
    scenario("pointer click on GestureClick pad (no signal in list)", lambda: click("pad"))
    scenario("keyboard: Tab, Tab… to button, Return", lambda: (click("entry"), tool("key", keys=["Tab"]), tool("key", keys=["Return"]))[2])
    scenario("keyboard: space on focused GtkToggleButton (activates on key release)", lambda: (click("entry"), tool("key", keys=["Tab"]), tool("key", keys=["Tab"]), tool("key", keys=["Tab"]), tool("key", keys=["Tab"]), tool("key", keys=["space"]))[5])
    scenario("popover menu: click MenuButton", lambda: click("menubtn"))
    scenario("popover menu: Down + Return on popover item (GSimpleAction::activate)", lambda: (tool("key", keys=["Down"]), tool("key", keys=["Return"]))[1])
    scenario("touch tap on GtkButton", lambda: tool("touch_tap", x=center("btn")[0], y=center("btn")[1], space="logical"))
    scenario("programmatic emissions in the app (button/entry/switch/check/toggle/scale/spin/expander/action)", lambda: (cmd("app"), time.sleep(1.6))[1])
    # gap measurement: delay of emission vs. latest input for every human emission
    lat = [e[3] for r in rows for e in r[3] if e[2] == "human" and not e[4]]
    print("\nhuman emission latencies since last input (ms), excl. held:", sorted(lat))
    cmd("quit")
finally:
    cleanup()
