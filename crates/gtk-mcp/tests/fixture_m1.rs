//! M1 acceptance against the real fixture: AC-3 (detached/stale), AC-4, AC-5,
//! AC-6, AC-8, AC-9, plus properties/signals/text/model/wait_for/diffs.
//! Every call is bounded (broker deadlines + the harness watchdog).

mod fixture_env;

use fixture_env::*;
use gtk_mcp::{Broker, Config, Policy};
use gtk_mcp_protocol::ErrorCode;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant};

const SECRET: &str = "hunter2-SECRET-0xC0FFEE";

fn by_id(id: &str) -> Value {
    json!({"automation_id": id})
}

fn rw(fx: &Fixture) -> Client {
    Client { broker: fx.broker(true) }
}

async fn counter(c: &Client) -> String {
    let q = c.json("inspect", json!({"target": by_id("click-counter")})).await;
    q["text"].as_str().unwrap().to_string()
}

fn decode_png(b: &[u8]) -> image::RgbaImage {
    image::load_from_memory_with_format(b, image::ImageFormat::Png).expect("valid PNG").to_rgba8()
}

// ---- AC-4 ------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn interact_set_expanded_reads_back_and_is_idempotent() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);
    for _ in 0..2 {
        let r = c.json("interact", json!({"target": by_id("advanced-expander"), "operation": "set_expanded", "expanded": true})).await;
        assert_eq!(r["observed"], json!({"expanded": true}));
    }
    // the child keeps its ref and is now attached and mapped
    let s = c.snapshot_json(json!({})).await;
    let exp = node_by_name(&s, "advanced-expander");
    let child = node_by_name(exp, "server-address");
    assert!(child.get("attached").is_none());
    assert_eq!(child["state"]["mapped"], true);
    let r = c.json("interact", json!({"target": by_id("advanced-expander"), "operation": "set_expanded", "expanded": false})).await;
    assert_eq!(r["observed"]["expanded"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn interact_operations_and_errors() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);
    let r = c.json("interact", json!({"target": by_id("save-primary"), "operation": "activate"})).await;
    assert_eq!(r["observed"]["activated"], true);
    assert_eq!(counter(&c).await, "clicks: 1");
    let r = c.json("interact", json!({"target": by_id("feature-switch"), "operation": "set_active", "active": true})).await;
    assert_eq!(r["observed"]["active"], true);
    let r = c.json("interact", json!({"target": by_id("remember"), "operation": "set_checked", "checked": true})).await;
    assert_eq!(r["observed"]["checked"], true);
    let r = c.json("interact", json!({"target": by_id("volume"), "operation": "set_value", "value": 70.0})).await;
    assert_eq!(r["observed"]["value"], 70.0);
    let e = c.err("interact", json!({"target": by_id("volume"), "operation": "set_value", "value": 1000.0})).await;
    assert_eq!(e.code, ErrorCode::InvalidValue);
    let r = c.json("interact", json!({"target": by_id("choices"), "operation": "select", "index": 2})).await;
    assert_eq!(r["observed"]["selected"], 2);
    let e = c.err("interact", json!({"target": by_id("choices"), "operation": "select", "index": 9})).await;
    assert_eq!(e.code, ErrorCode::InvalidValue);
    let r = c.json("interact", json!({"target": by_id("stack"), "operation": "select_page", "name": "page-b"})).await;
    assert_eq!(r["observed"]["visible_page"], "page-b");
    let r = c.json("interact", json!({"target": by_id("name-entry"), "operation": "fill", "text": "Ada"})).await;
    assert_eq!(r["observed"]["text"], "Ada");
    // submit became sensitive through the app's own handler
    let q = c.json("inspect", json!({"target": by_id("submit")})).await;
    assert_eq!(q["flags"]["sensitive"], true);
    // unsupported op on a widget, insensitive widget
    let e = c.err("interact", json!({"target": by_id("feature-switch"), "operation": "set_expanded", "expanded": true})).await;
    assert_eq!(e.code, ErrorCode::UnsupportedOperation);
    let e = c.err("interact", json!({"target": by_id("disabled-button"), "operation": "activate"})).await;
    assert_eq!(e.code, ErrorCode::UnsupportedOperation);
    let r = c.json("interact", json!({"target": by_id("menu-button"), "operation": "popup"})).await;
    assert!(r["observed"].is_object());
    let r = c.json("interact", json!({"target": by_id("menu-button"), "operation": "popdown"})).await;
    assert!(r["observed"].is_object());
}

#[tokio::test(flavor = "multi_thread")]
async fn interact_mutations_disabled_in_broker_or_agent() {
    let display = require_display!();
    // agent side off, broker on
    let fx = Fixture::start(&display, false, &[]);
    let c = Client { broker: fx.broker(true) };
    let e = c.err("interact", json!({"target": by_id("save-primary"), "operation": "activate"})).await;
    assert_eq!(e.code, ErrorCode::MutationsDisabled);
    assert_eq!(counter(&c).await, "clicks: 0");
    // broker side off, agent on
    let fx2 = Fixture::start(&display, true, &[]);
    let c2 = Client { broker: fx2.broker(false) };
    let e = c2.err("interact", json!({"target": by_id("save-primary"), "operation": "activate"})).await;
    assert_eq!(e.code, ErrorCode::MutationsDisabled);
    assert_eq!(counter(&c2).await, "clicks: 0");
}

// ---- AC-3 --------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn targets_ambiguous_interact_detached_then_stale() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);
    let e = c.err("interact", json!({"target": {"text": "Save", "type_name": "GtkButton"}, "operation": "activate"})).await;
    assert_eq!(e.code, ErrorCode::AmbiguousTarget);
    assert_eq!(e.data.as_ref().unwrap()["candidates"].as_array().unwrap().len(), 2);
    assert_eq!(counter(&c).await, "clicks: 0", "no widget was activated");

    let victim = c.json("query", json!({"selector": by_id("victim")})).await["matches"][0]["ref"].as_str().unwrap().to_string();
    c.json("interact", json!({"target": by_id("remove-widget"), "operation": "activate"})).await;
    // removed from its parent but alive
    let e = c.err("interact", json!({"target": victim, "operation": "focus"})).await;
    assert_eq!(e.code, ErrorCode::Detached);
    let i = c.json("inspect", json!({"target": victim})).await;
    assert_eq!(i["state"], "detached");
    let p = c.json("get_property", json!({"target": victim, "names": ["label"]})).await;
    assert_eq!(p["values"]["label"], "victim");
    // finalized
    c.json("interact", json!({"target": by_id("finalize-widget"), "operation": "activate"})).await;
    let e = c.err("inspect", json!({"target": victim})).await;
    assert_eq!(e.code, ErrorCode::StaleReference);
    let e = c.err("interact", json!({"target": victim, "operation": "focus"})).await;
    assert_eq!(e.code, ErrorCode::StaleReference);
}

// ---- AC-5 --------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn capture_settled_after_expand_and_unmapped_target() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);
    let before = c.call("capture", json!({"target": by_id("advanced-expander")})).await.unwrap();
    let before_meta = before.json_content().unwrap();
    let png_before = decode_png(before.images()[0]);
    assert_eq!(before_meta["stale"], false);
    assert_eq!(png_before.width(), before_meta["width"]);
    assert_eq!(png_before.height(), before_meta["height"]);

    c.json("interact", json!({"target": by_id("advanced-expander"), "operation": "set_expanded", "expanded": true})).await;
    let after = c.call("capture", json!({"target": by_id("advanced-expander")})).await.unwrap();
    let meta = after.json_content().unwrap();
    let png = decode_png(after.images()[0]);
    assert_eq!(meta["stale"], false, "{meta}");
    assert!(png.height() > png_before.height(), "expanded content adds height: {} vs {}", png.height(), png_before.height());
    // the region below the old height shows the entry (not a flat colour)
    let region: Vec<_> = (png_before.height()..png.height()).flat_map(|y| (0..png.width()).map(move |x| (x, y))).map(|(x, y)| *png.get_pixel(x, y)).collect();
    let first = region[0];
    assert!(region.iter().any(|p| *p != first), "expanded region must contain the entry's rendering");
    // the capture is retained by id and diffable
    let a = before_meta["capture_id"].as_str().unwrap();
    let b = meta["capture_id"].as_str().unwrap();
    let d = c.call("diff_captures", json!({"from": a, "to": b})).await.unwrap();
    let dm = d.json_content().unwrap();
    assert!(dm["size_mismatch"].is_object(), "{dm}");

    // unmapped but rooted target: the hidden stack page
    let e = c.err("capture", json!({"target": {"text": "Page B content"}})).await;
    assert_eq!(e.code, ErrorCode::TargetUnmapped);
    // annotate returns refs with bounds
    let ann = c.json("capture", json!({"target": by_id("advanced-expander"), "annotate": true})).await;
    assert!(!ann["annotations"].as_array().unwrap().is_empty());
    // `scale` multiplies the logical size. A content-sized button is used because a
    // compositor may resize the *window* between two captures (tiling reconfigure:
    // seen under sway, where consecutive window captures were 640 and 427 logical px wide),
    // which would make a window-vs-window ratio meaningless.
    let b1 = c.call("capture", json!({"target": by_id("remove-widget")})).await.unwrap();
    let b2 = c.call("capture", json!({"target": by_id("remove-widget"), "scale": 2.0})).await.unwrap();
    let (m1, m2) = (b1.json_content().unwrap(), b2.json_content().unwrap());
    assert_eq!(m2["scale"], 2.0);
    let (i1, i2) = (decode_png(b1.images()[0]), decode_png(b2.images()[0]));
    assert_eq!(i1.width(), m1["width"]);
    assert_eq!(i2.width(), m2["width"]);
    assert!(i2.width().abs_diff(i1.width() * 2) <= 1 && i2.height().abs_diff(i1.height() * 2) <= 1, "{}x{} vs {}x{}", i2.width(), i2.height(), i1.width(), i1.height());
    // a window capture honours scale against the window's own size at capture time
    let win = c.json("list_windows", json!({})).await["windows"][0]["ref"].clone();
    let w2 = c.call("capture", json!({"target": win, "scale": 2.0})).await.unwrap();
    let wm = w2.json_content().unwrap();
    assert_eq!(decode_png(w2.images()[0]).width(), wm["width"]);
    assert_eq!(wm["scale"], 2.0);
}

// ---- AC-6 --------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn secrets_never_leave_the_agent() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);
    let mut seen: Vec<String> = Vec::new();
    macro_rules! rec {
        ($tool:expr, $args:expr) => {{
            let out = c.call($tool, $args).await;
            let s = match &out {
                Ok(o) => format!("{:?}", o.contents),
                Err(e) => format!("{e:?}"),
            };
            seen.push(s);
            out
        }};
    }
    // put the secret in through the agent (readback must not echo it)
    let r = rec!("interact", json!({"target": by_id("password"), "operation": "fill", "text": SECRET})).unwrap();
    assert_eq!(r.json_content().unwrap()["observed"]["text"], json!({"redacted": true}));
    rec!("interact", json!({"target": by_id("password-row"), "operation": "fill", "text": SECRET})).unwrap();

    let snap = rec!("snapshot", json!({"format": "json"})).unwrap().json_content().unwrap();
    rec!("snapshot", json!({"format": "text"})).unwrap();
    let pw_ref = node_by_name(&snap, "password")["ref"].as_str().unwrap().to_string();
    let inner = find_node(node_by_name(&snap, "password"), &|n| n["type"] == "GtkText").unwrap()["ref"].as_str().unwrap().to_string();
    let rel = rec!("inspect", json!({"target": pw_ref})).unwrap().json_content().unwrap();
    let mut targets = vec![pw_ref.clone(), inner];
    // the buffer object is reachable as a related object of the inner GtkText
    let inner_i = rec!("inspect", json!({"target": targets[1].clone()})).unwrap().json_content().unwrap();
    for r in [&rel, &inner_i] {
        if let Some(b) = r["related"]["buffer"]["ref"].as_str() {
            targets.push(b.to_string());
        }
    }
    assert!(targets.len() >= 3, "buffer reachable via related: {inner_i}");
    for t in &targets {
        let out = rec!("get_property", json!({"target": t, "names": ["text", "text-length", "length", "cursor-position"]}));
        if let Ok(o) = out {
            let v = o.json_content().unwrap();
            for (_, val) in v["values"].as_object().unwrap() {
                assert_eq!(*val, json!({"redacted": true}), "{t}: {v}");
            }
        }
        let e = rec!("inspect_text", json!({"target": t})).unwrap_err();
        assert_eq!(e.code, ErrorCode::Redacted);
    }
    // selectors and waits never see secret text
    let q = rec!("query", json!({"selector": {"text": SECRET}})).unwrap().json_content().unwrap();
    assert_eq!(q["total"], 0);
    let q = rec!("query", json!({"selector": {"text_contains": "hunter2"}})).unwrap().json_content().unwrap();
    assert_eq!(q["total"], 0);
    let w = rec!("wait_for", json!({"target": by_id("password"), "condition": "text", "contains": "hunter2", "timeout_ms": 300})).unwrap().json_content().unwrap();
    assert_eq!(w["satisfied"], false);
    // a non-peeking capture succeeds (GTK draws bullets)...
    rec!("capture", json!({"target": by_id("password")})).unwrap();
    // ...a peeking one is refused, for the entry and for its window
    c.json("interact", json!({"target": by_id("peek-toggle"), "operation": "set_active", "active": true})).await;
    let e = rec!("capture", json!({"target": by_id("password")})).unwrap_err();
    assert_eq!(e.code, ErrorCode::Redacted);
    let win = c.json("list_windows", json!({})).await["windows"][0]["ref"].clone();
    let e = rec!("capture", json!({"target": win})).unwrap_err();
    assert_eq!(e.code, ErrorCode::Redacted);
    // still redacted after peeking toggled visibility
    let out = rec!("get_property", json!({"target": pw_ref, "names": ["text"]})).unwrap().json_content().unwrap();
    assert_eq!(out["values"]["text"], json!({"redacted": true}));
    rec!("snapshot", json!({"format": "json"})).unwrap();

    for (i, s) in seen.iter().enumerate() {
        assert!(!s.contains(SECRET), "response #{i} leaked the secret: {s}");
        assert!(!s.contains("hunter2"), "response #{i} leaked part of the secret");
    }
}

// ---- gm-r54 ------------------------------------------------------------------

/// The fixture's "seeded-pass" entry has app-side text and visibility=false,
/// and this test never observes it before mutating: the selector only uses
/// type/automation_id keys, which never evaluate secrecy (plan 4.4). Flipping
/// `visibility` via set_property must not un-mark the entry secret, or the
/// plaintext would leak through every later read.
const SEEDED: &str = "stalwart-ember-7-quill";

#[tokio::test(flavor = "multi_thread")]
async fn set_property_cannot_unseal_an_unobserved_secret() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);
    let target = json!({"type_name": "GtkEntry", "automation_id": "seeded-pass"});
    let mut seen: Vec<String> = Vec::new();

    // Flip visibility before any observation of the entry (gm-r54 trigger).
    let r = c.call("set_property", json!({"target": target, "name": "visibility", "value": true})).await;
    seen.push(format!("{r:?}"));
    let r = r.unwrap().json_content().unwrap();
    assert_eq!(r["observed"], true);

    // The entry is still secret: reads are redacted, not plaintext.
    let out = c.call("get_property", json!({"target": target, "names": ["text", "text-length"]})).await;
    seen.push(format!("{out:?}"));
    let out = out.unwrap().json_content().unwrap();
    assert_eq!(out["values"]["text"], json!({"redacted": true}));
    assert_eq!(out["values"]["text-length"], json!({"redacted": true}));

    // Snapshot withholds the text as well.
    let snap = c.call("snapshot", json!({"format": "json"})).await;
    seen.push(format!("{snap:?}"));
    let snap = snap.unwrap().json_content().unwrap();
    let node = node_by_name(&snap, "seeded-pass");
    assert_eq!(node["secret"], true);
    assert!(node.get("text").is_none(), "{node}");

    // inspect_text refuses like for any other secret.
    let e = c.err("inspect_text", json!({"target": target})).await;
    assert_eq!(e.code, ErrorCode::Redacted);

    for (i, s) in seen.iter().enumerate() {
        assert!(!s.contains(SEEDED), "response #{i} leaked the seeded secret: {s}");
    }
}

/// gm-ef6: the `last_frame` fallback must never serve a frame painted while a
/// secret field was peeking. Sequence: capture the window (its paintable is
/// remembered and keeps following paints), peek (plaintext paints), un-peek
/// (visibility=false again, so `check_target` passes), then capture before
/// the next paint. Both fallback paths (`settle:false` on a dirty widget and
/// `settle:true` timing out with no frame) must fail `capture_unavailable`
/// instead of returning the remembered peeking frame; a later settled
/// capture succeeds with a fresh (bulleted) frame. The captures are issued
/// immediately after un-peeking, well inside one compositor frame interval
/// (spike S2: ~1 s under nested-niri throttling), so the widget is still
/// dirty and the fallback is the path under test.
#[tokio::test(flavor = "multi_thread")]
async fn capture_after_peek_off_never_serves_the_peeking_frame() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);
    c.json("interact", json!({"target": by_id("password"), "operation": "fill", "text": SECRET})).await;
    let win = c.json("list_windows", json!({})).await["windows"][0]["ref"].clone();
    // Remember the window's paintable via a normal settled capture.
    c.call("capture", json!({"target": win})).await.unwrap();

    for args in [
        json!({"target": win, "settle": false}),
        json!({"target": win, "settle": true, "settle_timeout_ms": 0}),
    ] {
        // Peek and give the compositor a frame interval to paint plaintext
        // into the remembered paintable.
        c.json("interact", json!({"target": by_id("peek-toggle"), "operation": "set_active", "active": true})).await;
        tokio::time::sleep(Duration::from_millis(1600)).await;
        let e = c.err("capture", json!({"target": win})).await;
        assert_eq!(e.code, ErrorCode::Redacted, "capture while peeking must stay refused");
        // Un-peek: visibility=false, widget dirty, no fresh paint yet.
        c.json("interact", json!({"target": by_id("peek-toggle"), "operation": "set_active", "active": false})).await;
        let e = c.err("capture", args.clone()).await;
        assert_eq!(e.code, ErrorCode::CaptureUnavailable, "capture {args} after peek-off must not serve the peeking frame: {e:?}");
        // A settled capture still succeeds once a fresh frame paints.
        let ok = c.call("capture", json!({"target": win})).await.unwrap();
        assert_eq!(ok.json_content().unwrap()["stale"], false);
    }
}

// ---- AC-8 --------------------------------------------------------------------

fn short_deadline_broker(fx: &Fixture, ms: u64) -> Arc<Broker> {
    let mut cfg = Config::with_socket_dir(fx.dir.path());
    cfg.default_deadline_ms = ms;
    Broker::new(Policy { allow_mutations: true, ..Policy::default() }, cfg).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn dispatch_blocked_main_loop_is_unresponsive_and_nothing_runs_after_unblocking() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = Client { broker: short_deadline_broker(&fx, 1000) };
    c.json("list_apps", json!({})).await;
    // Block the main loop for 5 s; the blocking call itself outlives its 1 s deadline.
    let blocker = {
        let c2 = Client { broker: c.broker.clone() };
        tokio::spawn(async move { c2.call("interact", json!({"target": by_id("block-5s"), "operation": "activate"})).await })
    };
    tokio::time::sleep(Duration::from_millis(500)).await;
    let t = Instant::now();
    let e = c.err("interact", json!({"target": by_id("save-primary"), "operation": "activate"})).await;
    assert_eq!(e.code, ErrorCode::TargetUnresponsive, "{e:?}");
    assert_eq!(e.data.as_ref().unwrap()["started"], false);
    assert!(t.elapsed() < Duration::from_secs(3), "must return at the deadline, not after the block: {:?}", t.elapsed());
    let blocked = blocker.await.unwrap().unwrap_err();
    assert_eq!(blocked.code, ErrorCode::Timeout, "the blocking call had started: {blocked:?}");
    assert_eq!(blocked.data.as_ref().unwrap()["started"], true);
    // after unblocking: the abandoned mutation must not have executed
    tokio::time::sleep(Duration::from_secs(5)).await;
    let c_ok = rw(&fx);
    assert_eq!(counter(&c_ok).await, "clicks: 0", "a cancelled, not-started request must never run");
    // the app is responsive again
    c_ok.json("interact", json!({"target": by_id("save-primary"), "operation": "activate"})).await;
    assert_eq!(counter(&c_ok).await, "clicks: 1");
}

#[tokio::test(flavor = "multi_thread")]
async fn dispatch_started_mutation_past_deadline_becomes_late_result() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = Client { broker: short_deadline_broker(&fx, 1000) };
    c.json("list_apps", json!({})).await;
    let e = c.err("interact", json!({"target": by_id("slow-op"), "operation": "activate"})).await;
    assert_eq!(e.code, ErrorCode::Timeout);
    assert_eq!(e.data.as_ref().unwrap()["started"], true);
    let session = c.broker.sessions().remove(0);
    let t = Instant::now();
    let late = loop {
        let evs = session.events_since(0);
        if let Some(l) = evs.entries.iter().find(|e| e.value["kind"] == "late_result") {
            break l.value.clone();
        }
        assert!(t.elapsed() < Duration::from_secs(10), "late result never recorded");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(late["result"]["observed"]["activated"], true);
    let c_ok = rw(&fx);
    assert_eq!(counter(&c_ok).await, "clicks: 1", "the mutation ran exactly once and is never retried");
}

// ---- AC-9 --------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn batch_runs_steps_with_refs_and_a_settled_capture() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);
    let r = c
        .json(
            "batch",
            json!({"steps": [
                {"tool": "interact", "arguments": {"target": by_id("advanced-expander"), "operation": "set_expanded", "expanded": true}},
                {"tool": "wait_for", "arguments": {"target": by_id("server-address"), "condition": "mapped", "timeout_ms": 3000}},
                {"tool": "capture", "arguments": {"target": "$1.ref"}}
            ]}),
        )
        .await;
    let steps = r["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 3, "{r}");
    assert!(steps.iter().all(|s| s["ok"] == true), "{r}");
    assert_eq!(steps[1]["result"]["satisfied"], true);
    assert_eq!(steps[2]["result"]["stale"], false);
    assert_eq!(steps[2]["result"]["target"], steps[0]["result"]["target"]);
    // the settled capture's PNG was retained by the broker
    let id = steps[2]["result"]["capture_id"].as_str().unwrap();
    assert!(png_retained(&c, id).await);
}

async fn png_retained(c: &Client, id: &str) -> bool {
    // diffing a capture with itself succeeds only if the broker retained it
    c.call("diff_captures", json!({"from": id, "to": id})).await.is_ok()
}

#[tokio::test(flavor = "multi_thread")]
async fn batch_invalid_step_executes_nothing_and_readonly_runs_without_mutations() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);
    let e = c
        .err(
            "batch",
            json!({"steps": [
                {"tool": "interact", "arguments": {"target": by_id("save-primary"), "operation": "activate"}},
                {"tool": "interact", "arguments": {"target": by_id("save-primary"), "operation": "activate"}},
                {"tool": "interact", "arguments": {"target": by_id("save-primary"), "operation": "bogus"}}
            ]}),
        )
        .await;
    assert_eq!(e.code, ErrorCode::InvalidParams);
    assert_eq!(counter(&c).await, "clicks: 0", "nothing executes when step 3 is invalid");
    let e = c.err("batch", json!({"steps": [{"tool": "batch", "arguments": {"steps": []}}]})).await;
    assert_eq!(e.code, ErrorCode::InvalidParams);

    // read-only batch with mutations off everywhere
    let fx2 = Fixture::start(&display, false, &[]);
    let c2 = Client { broker: fx2.broker(false) };
    let r = c2
        .json(
            "batch",
            json!({"steps": [
                {"tool": "query", "arguments": {"selector": by_id("volume")}},
                {"tool": "get_property", "arguments": {"target": "$1.ref", "names": ["sensitive"]}}
            ]}),
        )
        .await;
    assert!(r["steps"].as_array().unwrap().iter().all(|s| s["ok"] == true), "{r}");
    // a mutating step with mutations off is refused as a whole
    let e = c2.err("batch", json!({"steps": [{"tool": "interact", "arguments": {"target": by_id("save-primary"), "operation": "activate"}}]})).await;
    assert_eq!(e.code, ErrorCode::MutationsDisabled);
    // stop_on_error default and override
    let r = c
        .json(
            "batch",
            json!({"steps": [
                {"tool": "inspect", "arguments": {"target": by_id("nope")}},
                {"tool": "inspect", "arguments": {"target": by_id("volume")}}
            ]}),
        )
        .await;
    assert_eq!(r["steps"].as_array().unwrap().len(), 1);
    assert_eq!(r["stopped_at"], 0);
    let r = c
        .json(
            "batch",
            json!({"stop_on_error": false, "steps": [
                {"tool": "inspect", "arguments": {"target": by_id("nope")}},
                {"tool": "inspect", "arguments": {"target": by_id("volume")}}
            ]}),
        )
        .await;
    assert_eq!(r["steps"].as_array().unwrap().len(), 2);
    assert_eq!(r["steps"][0]["error"]["code"], "not_found");
}

// ---- properties, signals, text, model, wait_for, observe, diffs --------------

#[tokio::test(flavor = "multi_thread")]
async fn properties_and_signals() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);
    let l = c.json("list_properties", json!({"target": by_id("save-primary"), "owner": "GtkButton"})).await;
    assert!(l["properties"].as_array().unwrap().iter().any(|p| p["name"] == "label" && p["writable"] == true));
    let g = c.json("get_property", json!({"target": by_id("save-primary"), "names": ["label", "sensitive", "halign"]})).await;
    assert_eq!(g["values"]["label"], "Save");
    assert_eq!(g["values"]["sensitive"], true);
    assert_eq!(g["values"]["halign"], "fill");
    let s = c.json("set_property", json!({"target": by_id("save-primary"), "name": "label", "value": "Store"})).await;
    assert_eq!(s["observed"], "Store");
    let e = c.err("set_property", json!({"target": by_id("save-primary"), "name": "label", "value": 5})).await;
    assert_eq!(e.code, ErrorCode::InvalidValue);
    let e = c.err("get_property", json!({"target": by_id("save-primary"), "names": ["nonexistent"]})).await;
    assert_eq!(e.code, ErrorCode::InvalidParams);
    let sig = c.json("list_signals", json!({"target": by_id("save-primary"), "name": "clicked"})).await;
    assert_eq!(sig["signals"][0]["name"], "clicked");
    // set_property with observe
    let s = c.json("set_property", json!({"target": by_id("save-primary"), "name": "label", "value": "Save", "observe": {"snapshot_depth": 1}})).await;
    assert!(s["observation"]["snapshot"]["roots"].is_array(), "{s}");
}

#[tokio::test(flavor = "multi_thread")]
async fn text_and_model_tools() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);
    let t = c.json("inspect_text", json!({"target": by_id("notes")})).await;
    assert_eq!(t["text"], "hello notes");
    let e = c.json("edit_text", json!({"target": by_id("notes"), "operation": "insert", "offset": 5, "text": ","})).await;
    assert_eq!(e["observed"]["text"], "hello, notes");
    let e = c.json("edit_text", json!({"target": by_id("notes"), "operation": "replace", "start": 0, "end": 5, "text": "bye"})).await;
    assert_eq!(e["observed"]["text"], "bye, notes");
    let e = c.err("edit_text", json!({"target": by_id("notes"), "operation": "delete", "start": 0, "end": 999})).await;
    assert_eq!(e.code, ErrorCode::InvalidValue);
    let m = c.json("inspect_model", json!({"target": by_id("items-list"), "limit": 5})).await;
    assert_eq!(m["n_items"], 500);
    assert_eq!(m["items"].as_array().unwrap().len(), 5);
    assert_eq!(m["items"][0]["summary"], "Item 0");
    assert!(m["realized_rows"].as_array().unwrap().len() < 500);
    let m2 = c.json("inspect_model", json!({"target": by_id("items-list"), "offset": 495, "limit": 50})).await;
    assert_eq!(m2["items"].as_array().unwrap().len(), 5);
}

#[tokio::test(flavor = "multi_thread")]
async fn wait_for_sensitive_after_fill_and_timeout_is_not_an_error() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = Arc::new(rw(&fx));
    let waiter = {
        let c = c.clone();
        tokio::spawn(async move { c.json("wait_for", json!({"target": by_id("submit"), "condition": "sensitive", "timeout_ms": 8000})).await })
    };
    tokio::time::sleep(Duration::from_millis(600)).await;
    c.json("interact", json!({"target": by_id("name-entry"), "operation": "fill", "text": "x"})).await;
    let r = waiter.await.unwrap();
    assert_eq!(r["satisfied"], true, "{r}");
    assert!(r["elapsed_ms"].as_u64().unwrap() < 7000);
    let r = c.json("wait_for", json!({"target": by_id("save-primary"), "condition": "text", "equals": "Never", "timeout_ms": 400})).await;
    assert_eq!(r["satisfied"], false);
    let r = c.json("wait_for", json!({"target": {"type_name": "GtkButton"}, "condition": "match_count", "count": 12345, "timeout_ms": 300})).await;
    assert_eq!(r["satisfied"], false);
    let r = c.json("wait_for", json!({"target": by_id("save-primary"), "condition": "exists", "timeout_ms": 1000})).await;
    assert_eq!(r["satisfied"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn interact_observe_and_observation_failure_keeps_the_mutation() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);
    let r = c
        .json(
            "interact",
            json!({"target": by_id("advanced-expander"), "operation": "set_expanded", "expanded": true, "observe": {"snapshot_depth": 2, "capture": true}}),
        )
        .await;
    assert!(r["observation"]["snapshot"]["roots"].is_array(), "{r}");
    assert!(r["observation"]["capture"]["capture_id"].is_string(), "{r}");
    assert!(r.get("observation_error").is_none() || r["observation_error"].is_null());
    // closing a window and observing a capture of it: dispatch ok, observation fails
    c.json("interact", json!({"target": by_id("open-second-window"), "operation": "activate"})).await;
    let r = c
        .json("interact", json!({"target": {"window": {"title": "Second window"}, "type_name": "GtkWindow"}, "operation": "close", "observe": {"capture": true}}))
        .await;
    assert!(r["observation_error"]["code"].is_string(), "{r}");
    let w = c.json("list_windows", json!({})).await;
    assert_eq!(w["windows"].as_array().unwrap().len(), 1, "the close happened");
}

#[tokio::test(flavor = "multi_thread")]
async fn diff_snapshots_reports_expander_child_changes() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);
    let a = c.snapshot_json(json!({})).await;
    c.json("interact", json!({"target": by_id("advanced-expander"), "operation": "set_expanded", "expanded": true})).await;
    let b = c.snapshot_json(json!({})).await;
    let child_ref = node_by_name(&a, "server-address")["ref"].as_str().unwrap().to_string();
    let d = c.json("diff_snapshots", json!({"from": a["snapshot_id"], "to": b["snapshot_id"]})).await;
    let changed = d["changed"].as_array().unwrap();
    let ch = changed.iter().find(|x| x["ref"] == child_ref.as_str()).unwrap_or_else(|| panic!("child not in diff: {d}"));
    let fields = ch["changes"].to_string();
    assert!(fields.contains("attached") && fields.contains("mapped"), "{ch}");
    let e = c.err("diff_snapshots", json!({"from": "snap-9999", "to": b["snapshot_id"]})).await;
    assert_eq!(e.code, ErrorCode::SnapshotUnavailable);
}

// gm-lhp: a secret buffer must not become readable by re-homing it onto a
// plain entry via set_property(buffer=...).
#[tokio::test(flavor = "multi_thread")]
async fn secret_buffer_cannot_be_rehomed_to_plain_entry() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);
    let mut seen: Vec<String> = Vec::new();
    macro_rules! rec {
        ($tool:expr, $args:expr) => {{
            let out = c.call($tool, $args).await;
            let s = match &out {
                Ok(o) => format!("{:?}", o.contents),
                Err(e) => format!("{e:?}"),
            };
            seen.push(s);
            out
        }};
    }
    rec!("interact", json!({"target": by_id("password"), "operation": "fill", "text": SECRET})).unwrap();

    let snap = rec!("snapshot", json!({"format": "json"})).unwrap().json_content().unwrap();
    let pw_ref = node_by_name(&snap, "password")["ref"].as_str().unwrap().to_string();
    let inner = find_node(node_by_name(&snap, "password"), &|n| n["type"] == "GtkText").unwrap()["ref"].as_str().unwrap().to_string();
    let plain = node_by_name(&snap, "name-entry")["ref"].as_str().unwrap().to_string();
    // Step 2 of the trigger: the secret buffer is reachable as an object ref.
    let buf_out = rec!("get_property", json!({"target": inner, "names": ["buffer"]})).unwrap().json_content().unwrap();
    let buf_ref = buf_out["values"]["buffer"]["ref"].as_str().expect("buffer ref").to_string();
    let _ = pw_ref;

    // Step 3: assigning the secret buffer onto a plain entry must be refused.
    // If it ever lands (assignment not refused), fix (a) must instead make the
    // plain entry secret, so every read below is redacted.
    let assigned = match rec!("set_property", json!({"target": plain, "name": "buffer", "value": buf_ref})) {
        Ok(_) => true,
        Err(e) => {
            assert_eq!(e.code, ErrorCode::Redacted, "secret buffer assignment must be refused as redacted: {e:?}");
            false
        }
    };

    if assigned {
        // Step 4: the re-homed entry must behave as secret everywhere.
        let out = rec!("get_property", json!({"target": plain.clone(), "names": ["text", "text-length"]})).unwrap().json_content().unwrap();
        for (_, val) in out["values"].as_object().unwrap() {
            assert_eq!(*val, json!({"redacted": true}), "plain entry with secret buffer must redact: {out}");
        }
        let e = rec!("inspect_text", json!({"target": plain.clone()})).unwrap_err();
        assert_eq!(e.code, ErrorCode::Redacted);
        let e = rec!("capture", json!({"target": plain})).unwrap_err();
        assert_eq!(e.code, ErrorCode::Redacted);
    }
    rec!("snapshot", json!({"format": "json"})).unwrap();
    let q = rec!("query", json!({"selector": {"text": SECRET}})).unwrap().json_content().unwrap();
    assert_eq!(q["total"], 0);

    for (i, s) in seen.iter().enumerate() {
        assert!(!s.contains(SECRET), "response #{i} leaked the secret: {s}");
        assert!(!s.contains("hunter2"), "response #{i} leaked part of the secret");
    }
}
