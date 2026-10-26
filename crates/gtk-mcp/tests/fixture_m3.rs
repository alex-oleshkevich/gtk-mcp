//! M3 acceptance against the real fixture: AC-10 (live CSS) and AC-12 (layout
//! evidence), plus style, highlight, render-tree and settings tools.
//! Every call is bounded (broker deadlines + the harness watchdog); polling
//! loops have explicit deadlines.

mod fixture_env;

use fixture_env::*;
use gtk_mcp::{Broker, Config, Policy};
use gtk_mcp_protocol::ErrorCode;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

const SECRET: &str = "hunter2-SECRET-0xC0FFEE";

fn by_id(id: &str) -> Value {
    json!({"automation_id": id})
}

fn broker(fx: &Fixture, allow_mutations: bool) -> Arc<Broker> {
    let policy = Policy {
        toolsets: vec!["core".into(), "layout-style".into()],
        allow_mutations,
        ..Policy::default()
    };
    Broker::new(policy, Config::with_socket_dir(fx.dir.path())).unwrap()
}

fn rw(fx: &Fixture) -> Client {
    Client { broker: broker(fx, true) }
}

fn decode_png(b: &[u8]) -> image::RgbaImage {
    image::load_from_memory_with_format(b, image::ImageFormat::Png).expect("valid PNG").to_rgba8()
}

async fn ref_of(c: &Client, id: &str) -> String {
    c.json("query", json!({"selector": by_id(id)})).await["matches"][0]["ref"].as_str().unwrap().to_string()
}

async fn scroll_section(c: &Client) {
    c.json("interact", json!({"target": by_id("m3-section"), "operation": "scroll_to"})).await;
}

async fn shot(c: &Client, id: &str) -> image::RgbaImage {
    let out = c.call("capture", json!({"target": by_id(id)})).await.unwrap_or_else(|e| panic!("capture {id}: {e} {:?}", e.data));
    decode_png(out.images()[0])
}

fn count_pixels(img: &image::RgbaImage, f: impl Fn(&image::Rgba<u8>) -> bool) -> usize {
    img.pixels().filter(|p| f(p)).count()
}

fn near(p: &image::Rgba<u8>, rgb: (u8, u8, u8), tol: i32) -> bool {
    let d = |a: u8, b: u8| (i32::from(a) - i32::from(b)).abs();
    d(p[0], rgb.0) <= tol && d(p[1], rgb.1) <= tol && d(p[2], rgb.2) <= tol
}

/// A second MCP client talking to the agent socket directly, so a test can drop
/// the connection (a broker's session reader keeps its socket open).
struct Raw {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    next: u64,
}

impl Raw {
    fn connect(fx: &Fixture) -> Raw {
        let stream = UnixStream::connect(fx.socket()).expect("connect to the agent socket");
        stream.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        let reader = BufReader::new(stream.try_clone().unwrap());
        Raw { stream, reader, next: 100 }
    }

    /// `Ok(result)` or `Err(error.data)`.
    fn call(&mut self, method: &str, params: Value) -> Result<Value, Value> {
        self.next += 1;
        let id = self.next;
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stream, "{req}").unwrap();
        loop {
            let mut line = String::new();
            let n = self.reader.read_line(&mut line).unwrap_or_else(|e| panic!("{method}: no response within 20 s: {e}"));
            assert!(n > 0, "{method}: the agent closed the connection");
            let msg: Value = serde_json::from_str(&line).unwrap();
            if msg["id"] != json!(id) {
                continue; // progress notifications
            }
            return match msg.get("error") {
                Some(e) => Err(e["data"].clone()),
                None => Ok(msg["result"].clone()),
            };
        }
    }
}

/// Poll `f` until it returns true, at most `secs` seconds.
async fn eventually<F: AsyncFn() -> bool>(what: &str, secs: u64, f: F) {
    let end = Instant::now() + Duration::from_secs(secs);
    loop {
        if f().await {
            return;
        }
        assert!(Instant::now() < end, "timed out after {secs} s waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

fn findings<'a>(v: &'a Value, check: &str) -> Vec<&'a Value> {
    v["findings"].as_array().unwrap().iter().filter(|f| f["check"] == check).collect()
}

fn has_finding_for(v: &Value, check: &str, r: &str) -> bool {
    findings(v, check).iter().any(|f| f["ref"] == r)
}

// ---- AC-12 -----------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn layout_audit_finds_ellipsized_label_and_for_size_measures() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);

    let long = ref_of(&c, "long-label").await;
    let audit = c.json("audit_layout", json!({})).await;
    assert!(has_finding_for(&audit, "ellipsized_label", &long), "long-label must be reported: {audit}");
    let f = findings(&audit, "ellipsized_label").into_iter().find(|f| f["ref"] == long.as_str()).unwrap();
    assert_eq!(f["evidence"]["ellipsize"], "end");
    assert!(f["evidence"]["text_width"].as_i64().unwrap() > f["evidence"]["allocated_width"].as_i64().unwrap(), "{f}");
    assert!(f["evidence"]["text"].as_str().unwrap().starts_with("This label text"));

    // the other fixture cases
    let ell = ref_of(&c, "m3-ellipsized").await;
    let fits = ref_of(&c, "m3-fits").await;
    assert!(has_finding_for(&audit, "ellipsized_label", &ell));
    assert!(!audit["findings"].as_array().unwrap().iter().any(|f| f["ref"] == fits.as_str()), "a label that fits has no finding");

    let zero = ref_of(&c, "m3-zero").await;
    assert!(has_finding_for(&audit, "zero_size", &zero), "{audit}");

    let (base, top) = (ref_of(&c, "m3-overlay-base").await, ref_of(&c, "m3-overlay-top").await);
    let ov = findings(&audit, "overlapping_siblings");
    let ov = ov.iter().find(|f| f["ref"] == top.as_str() && f["evidence"]["other"]["ref"] == base.as_str()).unwrap_or_else(|| panic!("overlay overlap missing: {audit}"));
    assert_eq!(ov["evidence"]["parent_type"], "GtkOverlay");
    assert!(ov["evidence"]["overlap"]["width"].as_f64().unwrap() > 1.0);

    let clip = ref_of(&c, "m3-clip-child").await;
    let out = findings(&audit, "outside_parent");
    let out = out.iter().find(|f| f["ref"] == clip.as_str()).unwrap_or_else(|| panic!("clipped child missing: {audit}"));
    assert_eq!(out["evidence"]["parent_clips"], true);
    assert!(out["evidence"]["overflow_px"]["right"].as_f64().unwrap() > 20.0);

    let (icon, icon_button) = (ref_of(&c, "m3-bad-icon").await, ref_of(&c, "m3-bad-icon-button").await);
    assert!(has_finding_for(&audit, "unresolved_icon", &icon));
    assert!(has_finding_for(&audit, "unresolved_icon", &icon_button));
    let ic = findings(&audit, "unresolved_icon").into_iter().find(|f| f["ref"] == icon.as_str()).unwrap();
    assert_eq!(ic["evidence"]["icon_names"], json!(["gtk-mcp-no-such-icon"]));

    // GtkInscription ellipsis is reported unsupported, not as a finding
    let insc = ref_of(&c, "m3-inscription").await;
    assert!(audit["unsupported"].as_array().unwrap().iter().any(|u| u["ref"] == insc.as_str() && u["type"] == "GtkInscription"), "{audit}");
    assert!(!audit["findings"].as_array().unwrap().iter().any(|f| f["ref"] == insc.as_str() && f["check"] == "ellipsized_label"));

    // scoping, check selection and limits
    let only = c.json("audit_layout", json!({"checks": ["ellipsized_label"]})).await;
    assert!(only["findings"].as_array().unwrap().iter().all(|f| f["check"] == "ellipsized_label"));
    let limited = c.json("audit_layout", json!({"limit": 1})).await;
    assert_eq!(limited["findings"].as_array().unwrap().len(), 1);
    assert_eq!(limited["truncated"], true);
    assert!(limited["total"].as_u64().unwrap() > 1);
    let scoped = c.json("audit_layout", json!({"target": by_id("m3-section")})).await;
    assert!(!has_finding_for(&scoped, "ellipsized_label", &long), "long-label is outside the m3 section");
    assert!(has_finding_for(&scoped, "ellipsized_label", &ell));

    // inspect_layout for_size=360 on the ellipsized label of AC-12
    let l = c.json("inspect_layout", json!({"target": by_id("long-label"), "for_size": 360})).await;
    assert_eq!(l["ref"], long.as_str());
    let (as_w, as_h) = (&l["for_size"]["as_width"], &l["for_size"]["as_height"]);
    assert_eq!(l["for_size"]["size"], 360);
    assert_eq!(as_w["orientation"], "vertical");
    assert_eq!(as_h["orientation"], "horizontal");
    assert!(as_w["minimum"].as_i64().unwrap() > 0 && as_w["natural"].as_i64().unwrap() >= as_w["minimum"].as_i64().unwrap(), "{l}");
    assert!(as_h["natural"].as_i64().unwrap() >= as_h["minimum"].as_i64().unwrap());
    // 12 max chars: the unconstrained natural width is far below the label's full text width
    let unconstrained = &l["measure"]["horizontal"];
    assert!(unconstrained["natural"].as_i64().unwrap() < 200, "{l}");
    assert_eq!(unconstrained["for_size"], -1);
    assert_eq!(l["alignment"]["halign"], "start");
    assert_eq!(l["parent"]["type"], "GtkBox");
    assert!(l["siblings"].as_array().unwrap().iter().any(|s| s["self"] == true && s["ref"] == long.as_str()));
    assert!(l["allocation"]["width"].as_i64().unwrap() > 0);
    assert!(l["bounds_in_root"]["width"].as_f64().unwrap() > 0.0);
}

#[tokio::test(flavor = "multi_thread")]
async fn inspect_layout_for_size_is_consistent_with_the_allocation() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);

    // the wrapping label's height depends on the width: narrower is taller
    let wide = c.json("inspect_layout", json!({"target": by_id("m3-wrapping"), "for_size": 360})).await;
    let narrow = c.json("inspect_layout", json!({"target": by_id("m3-wrapping"), "for_size": 120})).await;
    let (hw, hn) = (wide["for_size"]["as_width"]["natural"].as_i64().unwrap(), narrow["for_size"]["as_width"]["natural"].as_i64().unwrap());
    assert!(hn > hw, "height for 120px ({hn}) must exceed height for 360px ({hw})");

    // the measurement at the allocated width equals the allocated height
    let alloc_w = wide["allocation"]["width"].as_i64().unwrap();
    let at_alloc = c.json("inspect_layout", json!({"target": by_id("m3-wrapping"), "for_size": alloc_w})).await;
    assert_eq!(at_alloc["for_size"]["as_width"]["natural"], wide["allocation"]["height"], "{at_alloc}");

    // a start-aligned label is allocated its unconstrained natural width
    let ell = c.json("inspect_layout", json!({"target": by_id("m3-ellipsized")})).await;
    assert_eq!(ell["allocation"]["width"], ell["measure"]["horizontal"]["natural"], "{ell}");
    assert_eq!(ell["alignment"]["halign"], "start");
    assert!(ell.get("for_size").is_none());

    // errors
    let e = c.err("inspect_layout", json!({"target": by_id("m3-ellipsized"), "for_size": -5})).await;
    assert_eq!(e.code, ErrorCode::InvalidValue);
    // detached widgets have no layout
    let victim = ref_of(&c, "victim").await;
    c.json("interact", json!({"target": by_id("remove-widget"), "operation": "activate"})).await;
    let e = c.err("inspect_layout", json!({"target": victim})).await;
    assert_eq!(e.code, ErrorCode::Detached);
}

// ---- inspect_style --------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn inspect_style_reports_public_style_information() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);
    let s = c.json("inspect_style", json!({"target": by_id("save-primary")})).await;
    assert_eq!(s["css_name"], "button");
    assert!(s["css_classes"].as_array().unwrap().iter().any(|c| c == "text-button"), "{s}");
    assert!(s["state_flags"].is_array());
    assert!(s["css_path"].as_str().unwrap().ends_with("button.text-button#save-primary"), "{s}");
    assert!(s["color"].as_str().unwrap().starts_with("rgb"));
    assert!(s["font"].is_string());
    assert_eq!(s["direction"], "ltr");

    let d = c.json("inspect_style", json!({"target": by_id("disabled-button")})).await;
    assert!(d["state_flags"].as_array().unwrap().iter().any(|f| f == "insensitive"), "{d}");

    let a = c.json("inspect_style", json!({"target": by_id("m3-styled")})).await;
    assert!(a["css_classes"].as_array().unwrap().iter().any(|c| c == "m3-accent"));
    // no computed CSS
    assert!(a.get("computed").is_none() && a.get("computed_css").is_none());
}

// ---- AC-10 -----------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn css_invalid_reports_diagnostics_and_installs_nothing() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);
    let r = c.json("apply_css", json!({"css": "\n\n.m3-accent { colour: red; }\n"})).await;
    assert_eq!(r["installed"], false, "{r}");
    assert_eq!(r["handle"], Value::Null);
    assert_eq!(r["providers"], 0, "no provider may be installed");
    let d = &r["diagnostics"][0];
    assert_eq!(d["severity"], "error", "{r}");
    assert_eq!(d["start"]["line"], 3, "{r}");
    assert!(d["start"]["column"].as_u64().unwrap() >= 1);
    assert!(d["end"]["line"].as_u64().unwrap() >= 3);
    assert!(!d["message"].as_str().unwrap().is_empty());
    // syntax error too
    let r = c.json("apply_css", json!({"css": ".a { color: ; }\n.b { background: #zzz }"})).await;
    assert_eq!(r["installed"], false);
    assert!(r["diagnostics"].as_array().unwrap().len() >= 2, "{r}");
    // the app's look is untouched and no provider is counted
    let s = c.json("inspect_style", json!({"target": by_id("m3-styled")})).await;
    assert_eq!(s["temporary_css_providers"], 0);
    // oversized css is refused
    let e = c.err("apply_css", json!({"css": "a{}".repeat(100_000)})).await;
    assert_eq!(e.code, ErrorCode::InvalidValue);
    // unknown handle
    let e = c.err("remove_css", json!({"handle": "css-999"})).await;
    assert_eq!(e.code, ErrorCode::NotFound);
}

#[tokio::test(flavor = "multi_thread")]
async fn css_apply_changes_pixels_remove_and_disconnect_revert() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);
    scroll_section(&c).await;
    let original = shot(&c, "m3-styled").await;
    assert!(count_pixels(&original, |p| near(p, (0x2a, 0x7f, 0xff), 6)) > original.pixels().len() / 2, "the fixture styles the class blue");

    // apply -> the change is in the capture; no extra wait is needed
    let r = c.json("apply_css", json!({"css": ".m3-accent { background-color: #e01b24; }"})).await;
    assert_eq!(r["installed"], true, "{r}");
    assert_eq!(r["diagnostics"], json!([]));
    assert_eq!(r["providers"], 1);
    assert_eq!(r["settled"], true);
    let handle = r["handle"].as_str().unwrap().to_string();
    let changed = shot(&c, "m3-styled").await;
    assert_eq!(changed.dimensions(), original.dimensions());
    assert!(count_pixels(&changed, |p| near(p, (0xe0, 0x1b, 0x24), 6)) > changed.pixels().len() / 2, "region turned red");
    assert!(changed != original);
    assert_eq!(c.json("inspect_style", json!({"target": by_id("m3-styled")})).await["temporary_css_providers"], 1);

    // remove -> pixel-identical to the original
    let r = c.json("remove_css", json!({"handle": handle})).await;
    assert_eq!(r["removed"], true);
    assert_eq!(r["providers"], 0);
    assert!(shot(&c, "m3-styled").await == original, "removing the provider reverts the look");
    let e = c.err("remove_css", json!({"handle": handle})).await;
    assert_eq!(e.code, ErrorCode::NotFound);

    // client disconnect -> reverted
    let mut raw = Raw::connect(&fx);
    let r = raw.call("apply_css", json!({"css": ".m3-accent { background-color: #e01b24; }"})).unwrap();
    assert_eq!(r["installed"], true, "{r}");
    let raw_handle = r["handle"].as_str().unwrap().to_string();
    assert!(shot(&c, "m3-styled").await != original, "the second client's CSS is live");
    // another client cannot remove it
    let e = c.err("remove_css", json!({"handle": raw_handle})).await;
    assert_eq!(e.code, ErrorCode::NotFound);
    assert!(shot(&c, "m3-styled").await != original);
    drop(raw); // closes the connection

    eventually("the dropped client's CSS to be reverted", 15, async || shot(&c, "m3-styled").await == original).await;
    assert_eq!(c.json("inspect_style", json!({"target": by_id("m3-styled")})).await["temporary_css_providers"], 0);

    // a fresh broker (new connection) sees the original look as well
    let c2 = rw(&fx);
    scroll_section(&c2).await;
    assert!(shot(&c2, "m3-styled").await == original);
}

#[tokio::test(flavor = "multi_thread")]
async fn highlight_outlines_hides_its_class_and_reverts() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);
    scroll_section(&c).await;
    let original = shot(&c, "m3-styled").await;
    let magenta = |p: &image::Rgba<u8>| near(p, (0xff, 0x2b, 0xd6), 12);
    assert_eq!(count_pixels(&original, magenta), 0);

    let h = c.json("highlight", json!({"target": by_id("m3-styled")})).await;
    assert_eq!(h["highlighted"], true, "{h}");
    assert_eq!(h["settled"], true);
    let styled_ref = ref_of(&c, "m3-styled").await;
    assert_eq!(h["target"], styled_ref.as_str());
    let lit = shot(&c, "m3-styled").await;
    assert!(count_pixels(&lit, magenta) > 100, "the outline is visible in the capture");

    // the agent's class is hidden from snapshots, selectors, inspect and inspect_style
    let snap = c.snapshot_json(json!({"root": by_id("m3-section")})).await;
    let node = node_by_name(&snap, "m3-styled");
    assert!(node["identity"]["css_classes"].as_array().unwrap().iter().all(|c| c != "gtk-mcp-highlight"), "{node}");
    assert!(!serde_json::to_string(&snap).unwrap().contains("gtk-mcp-highlight"));
    assert!(c.json("query", json!({"selector": {"css_class": "gtk-mcp-highlight"}})).await["matches"].as_array().unwrap().is_empty());
    let st = c.json("inspect_style", json!({"target": by_id("m3-styled")})).await;
    assert!(!st.to_string().contains("gtk-mcp-highlight"), "{st}");
    assert!(!c.json("inspect", json!({"target": by_id("m3-styled")})).await.to_string().contains("gtk-mcp-highlight"));

    // highlighting another widget clears the previous highlight
    let h2 = c.json("highlight", json!({"target": by_id("m3-fits")})).await;
    assert_eq!(h2["cleared"], json!([h["handle"]]));
    assert!(shot(&c, "m3-styled").await == original, "the previous highlight is gone");
    let e = c.err("remove_css", json!({"handle": h["handle"]})).await;
    assert_eq!(e.code, ErrorCode::NotFound);
    assert!(count_pixels(&shot(&c, "m3-fits").await, magenta) > 20);

    // remove_css removes a highlight
    c.json("remove_css", json!({"handle": h2["handle"]})).await;
    assert_eq!(count_pixels(&shot(&c, "m3-fits").await, magenta), 0);

    // no target: just clear
    let h3 = c.json("highlight", json!({"target": by_id("m3-styled")})).await;
    let cleared = c.json("highlight", json!({})).await;
    assert_eq!(cleared["highlighted"], false);
    assert_eq!(cleared["cleared"], json!([h3["handle"]]));
    assert!(shot(&c, "m3-styled").await == original);

    // disconnect reverts a highlight made by another client
    let mut raw = Raw::connect(&fx);
    raw.call("highlight", json!({"target": by_id("m3-styled")})).unwrap();
    assert!(count_pixels(&shot(&c, "m3-styled").await, magenta) > 100);
    drop(raw);
    eventually("the dropped client's highlight to disappear", 15, async || shot(&c, "m3-styled").await == original).await;
}

/// gm-cse: a highlight that fails to install (the client is at its provider
/// cap) must not strip the class from a widget another client highlights.
#[tokio::test(flavor = "multi_thread")]
async fn failed_highlight_keeps_another_clients_highlight() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);
    scroll_section(&c).await;
    let magenta = |p: &image::Rgba<u8>| near(p, (0xff, 0x2b, 0xd6), 12);
    let mut raw = Raw::connect(&fx);
    raw.call("highlight", json!({"target": by_id("m3-styled")})).unwrap();
    assert!(count_pixels(&shot(&c, "m3-styled").await, magenta) > 100);

    // Fill this client's provider budget, then highlight the same widget.
    let mut handles = Vec::new();
    loop {
        match c.call("apply_css", json!({"css": format!(".gm-cse-{} {{ }}", handles.len())})).await {
            Ok(out) => handles.push(out.json_content().unwrap()["handle"].as_str().unwrap().to_owned()),
            Err(e) => {
                assert_eq!(e.code, ErrorCode::InvalidValue, "{e}");
                break;
            }
        }
        assert!(handles.len() <= 64, "the per-client provider cap is 64");
    }
    let e = c.err("highlight", json!({"target": by_id("m3-styled")})).await;
    assert_eq!(e.code, ErrorCode::InvalidValue, "{e}");
    // remove_css waits until the restyle reached a frame, so a stripped class would show.
    c.json("remove_css", json!({"handle": handles[0]})).await;
    assert!(count_pixels(&shot(&c, "m3-styled").await, magenta) > 100, "the other client's highlight is still drawn");
}

// ---- inspect_render --------------------------------------------------------------

fn collect_types(v: &Value, out: &mut Vec<String>) {
    if let Some(t) = v["type"].as_str() {
        out.push(t.to_string());
    }
    for c in v["children"].as_array().into_iter().flatten() {
        collect_types(c, out);
    }
}

fn find_text_nodes<'a>(v: &'a Value, out: &mut Vec<&'a Value>) {
    if v["type"] == "text" {
        out.push(v);
    }
    for c in v["children"].as_array().into_iter().flatten() {
        find_text_nodes(c, out);
    }
}

fn all_nodes<'a>(v: &'a Value, out: &mut Vec<&'a Value>) {
    out.push(v);
    for c in v["children"].as_array().into_iter().flatten() {
        all_nodes(c, out);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn render_tree_is_limited_and_keeps_secrets_out() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);

    // plain widget: nodes, text with glyph count
    let r = c.json("inspect_render", json!({"target": by_id("click-counter")})).await;
    let mut types = Vec::new();
    collect_types(&r["root"], &mut types);
    assert!(!types.is_empty() && r["node_count"].as_u64().unwrap() as usize == types.len(), "{r}");
    let mut texts = Vec::new();
    find_text_nodes(&r["root"], &mut texts);
    assert!(texts.iter().any(|t| t["glyphs"].as_u64().unwrap_or(0) >= 7), "label text node with its glyphs: {r}");
    assert_eq!(r["redacted_text_nodes"], 0);
    assert!(r["root"]["bounds"]["width"].as_f64().unwrap() > 0.0);

    // limits
    let small = c.json("inspect_render", json!({"target": by_id("save-primary"), "max_nodes": 2})).await;
    assert_eq!(small["truncated"], true);
    assert!(small["node_count"].as_u64().unwrap() <= 2);
    let shallow = c.json("inspect_render", json!({"target": by_id("save-primary"), "max_depth": 1})).await;
    assert_eq!(shallow["truncated"], true, "{shallow}");

    // secret subtree: text nodes are replaced
    c.json("interact", json!({"target": by_id("password"), "operation": "fill", "text": SECRET})).await;
    c.json("interact", json!({"target": by_id("password-row"), "operation": "fill", "text": SECRET})).await;
    for id in ["password", "password-row"] {
        // Widgets scrolled out of the viewport have no render node in the last frame.
        c.json("interact", json!({"target": by_id(id), "operation": "scroll_to"})).await;
        // Focused: the caret (whose position encodes the length) is drawn.
        c.json("interact", json!({"target": by_id(id), "operation": "focus"})).await;
        let r = c.json("inspect_render", json!({"target": by_id(id)})).await;
        let s = r.to_string();
        assert!(!s.contains(SECRET), "{id}: secret leaked");
        let mut texts = Vec::new();
        find_text_nodes(&r["root"], &mut texts);
        assert!(!texts.is_empty(), "{id}: the field renders text (bullets): {r}");
        assert!(texts.iter().all(|t| t["redacted"] == true && t.get("glyphs").is_none() && t.get("bounds").is_none()), "{id}: {r}");
        assert_eq!(r["redacted_text_nodes"].as_u64().unwrap() as usize, texts.len());
        assert!(r["secret_regions"].as_u64().unwrap() >= 1);
        // No node inside the field keeps geometry or attributes (caret x, selection
        // width and scroll offset encode the length).
        let mut all = Vec::new();
        all_nodes(&r["root"], &mut all);
        for n in &all {
            assert_eq!(n["redacted"], true, "{id}: {n}");
            for k in ["bounds", "transform", "clip", "color", "child_bounds"] {
                assert!(n.get(k).is_none(), "{id}: node keeps {k}: {n}");
            }
        }
    }
    // a window containing the secret field redacts it as well
    let win = c.json("list_windows", json!({})).await["windows"][0]["ref"].clone();
    let whole = c.json("inspect_render", json!({"target": win, "max_nodes": 5000})).await;
    assert!(!whole.to_string().contains(SECRET));
    assert!(whole["redacted_text_nodes"].as_u64().unwrap() >= 2, "{}", whole["redacted_text_nodes"]);
    let mut all = Vec::new();
    all_nodes(&whole["root"], &mut all);
    assert!(all.iter().any(|n| n["redacted"] == true && n["type"] != "text"), "nodes inside the fields are withheld");
    assert!(whole["root"]["bounds"].is_object(), "nodes outside secret fields keep their geometry");

    // peeking secret: refused, for the field and for any ancestor
    let pw = c.snapshot_json(json!({"root": by_id("password")})).await;
    let inner = find_node(node_by_name(&pw, "password"), &|n| n["type"] == "GtkText").unwrap()["ref"].as_str().unwrap().to_string();
    c.json("set_property", json!({"target": inner, "name": "visibility", "value": true})).await;
    let e = c.err("inspect_render", json!({"target": by_id("password")})).await;
    assert_eq!(e.code, ErrorCode::Redacted);
    let e = c.err("inspect_render", json!({"target": win})).await;
    assert_eq!(e.code, ErrorCode::Redacted);
    c.json("set_property", json!({"target": inner, "name": "visibility", "value": false})).await;
    c.json("inspect_render", json!({"target": by_id("password")})).await;

    // unmapped target
    let e = c.err("inspect_render", json!({"target": {"text": "Page B content"}})).await;
    assert_eq!(e.code, ErrorCode::TargetUnmapped);
}

// ---- settings --------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn settings_are_inspected_and_overrides_revert_on_disconnect() {
    let display = require_display!();
    let fx = Fixture::start(&display, true, &[]);
    let c = rw(&fx);

    let s = c.json("inspect_settings", json!({})).await;
    assert!(s["gtk"]["gtk-theme-name"].is_string(), "{s}");
    assert!(s["gtk"]["gtk-icon-theme-name"].is_string());
    assert!(s["gtk"]["gtk-font-name"].is_string());
    assert!(s["gtk"]["gtk-enable-animations"].is_boolean());
    assert_eq!(s["libadwaita"]["available"], true);
    let sm = &s["libadwaita"]["style_manager"];
    let dark0 = sm["dark"].as_bool().unwrap();
    assert!(sm["color-scheme"].is_string());
    assert!(sm["high-contrast"].is_boolean());
    assert_eq!(s["default_text_direction"], "ltr");
    let dpi0 = s["gtk"]["gtk-xft-dpi"].as_i64().unwrap();
    let anim0 = s["gtk"]["gtk-enable-animations"].as_bool().unwrap();

    // read-only and unknown settings
    let e = c.err("override_setting", json!({"setting": "high_contrast", "value": true})).await;
    assert_eq!(e.code, ErrorCode::UnsupportedOperation);
    let e = c.err("override_setting", json!({"setting": "accent_color", "value": "red"})).await;
    assert_eq!(e.code, ErrorCode::UnsupportedOperation);
    let e = c.err("override_setting", json!({"setting": "nonsense", "value": 1})).await;
    assert_eq!(e.code, ErrorCode::InvalidParams);
    let e = c.err("override_setting", json!({"setting": "animations"})).await;
    assert_eq!(e.code, ErrorCode::InvalidParams);
    let e = c.err("override_setting", json!({"setting": "animations", "value": "yes"})).await;
    assert_eq!(e.code, ErrorCode::InvalidValue);
    let e = c.err("override_setting", json!({"setting": "text_scale", "value": 99})).await;
    assert_eq!(e.code, ErrorCode::InvalidValue);
    let e = c.err("override_setting", json!({"setting": "color_scheme", "value": "plaid"})).await;
    assert_eq!(e.code, ErrorCode::InvalidValue);
    let e = c.err("override_setting", json!({"setting": "text_direction", "value": "up"})).await;
    assert_eq!(e.code, ErrorCode::InvalidValue);

    // a second client overrides everything; the first one watches
    let mut raw = Raw::connect(&fx);
    let (scheme, want_dark) = if dark0 { ("force-light", false) } else { ("force-dark", true) };
    let r = raw.call("override_setting", json!({"setting": "color_scheme", "value": scheme})).unwrap();
    assert_eq!(r["observed"]["dark"], want_dark, "{r}");
    assert_eq!(r["observed"]["color-scheme"], scheme);
    let r = raw.call("override_setting", json!({"setting": "animations", "value": !anim0})).unwrap();
    assert_eq!(r["previous"], anim0);
    let r = raw.call("override_setting", json!({"setting": "text_scale", "value": 1.5})).unwrap();
    assert_eq!(r["observed"]["scale"], 1.5);
    raw.call("override_setting", json!({"setting": "text_direction", "value": "rtl"})).unwrap();

    let s = c.json("inspect_settings", json!({})).await;
    assert_eq!(s["libadwaita"]["style_manager"]["dark"], want_dark, "the override is visible to other clients");
    assert_eq!(s["gtk"]["gtk-enable-animations"], !anim0);
    let base = if dpi0 > 0 { dpi0 } else { 96 * 1024 };
    assert_eq!(s["gtk"]["gtk-xft-dpi"].as_i64().unwrap(), (base as f64 * 1.5).round() as i64);
    assert_eq!(s["default_text_direction"], "rtl");
    assert_eq!(s["overrides"].as_array().unwrap().len(), 4);
    // the dark state is visible to widgets too
    let style = c.json("inspect_style", json!({"target": by_id("save-primary")})).await;
    assert_eq!(style["direction"], "rtl");

    // an early reset restores one setting
    let r = raw.call("override_setting", json!({"setting": "animations", "reset": true})).unwrap();
    assert_eq!(r["restored"], true);
    assert_eq!(c.json("inspect_settings", json!({})).await["gtk"]["gtk-enable-animations"], anim0);
    let e = raw.call("override_setting", json!({"setting": "animations", "reset": true})).unwrap_err();
    assert_eq!(e["code"], "not_found");

    drop(raw);
    eventually("overrides to be reverted after the client disconnected", 15, async || {
        let s = c.json("inspect_settings", json!({})).await;
        s["libadwaita"]["style_manager"]["dark"] == dark0
            && s["default_text_direction"] == "ltr"
            && s["gtk"]["gtk-xft-dpi"].as_i64() == Some(dpi0)
            && s["overrides"].as_array().unwrap().is_empty()
    })
    .await;
    let s = c.json("inspect_settings", json!({})).await;
    assert_eq!(s["libadwaita"]["style_manager"]["color-scheme"], sm["color-scheme"]);
}

// ---- mutation gate and toolset exposure ---------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn mutating_layout_style_tools_are_gated_and_read_only_ones_are_not() {
    let display = require_display!();
    // agent side off, broker on
    let fx = Fixture::start(&display, false, &[]);
    let c = Client { broker: broker(&fx, true) };
    for (tool, args) in [
        ("apply_css", json!({"css": ".a { color: red; }"})),
        ("remove_css", json!({"handle": "css-1"})),
        ("highlight", json!({"target": by_id("m3-styled")})),
        ("override_setting", json!({"setting": "animations", "value": false})),
    ] {
        let e = c.err(tool, args).await;
        assert_eq!(e.code, ErrorCode::MutationsDisabled, "{tool} (agent gate)");
    }
    // broker side off, agent on
    let fx2 = Fixture::start(&display, true, &[]);
    let c2 = Client { broker: broker(&fx2, false) };
    for (tool, args) in [
        ("apply_css", json!({"css": ".a { color: red; }"})),
        ("highlight", json!({"target": by_id("m3-styled")})),
        ("override_setting", json!({"setting": "animations", "value": false})),
    ] {
        let e = c2.err(tool, args).await;
        assert_eq!(e.code, ErrorCode::MutationsDisabled, "{tool} (broker gate)");
    }
    // read-only tools run without mutations on either side
    for cl in [&c, &c2] {
        cl.json("inspect_layout", json!({"target": by_id("m3-styled")})).await;
        cl.json("audit_layout", json!({})).await;
        cl.json("inspect_style", json!({"target": by_id("m3-styled")})).await;
        cl.json("inspect_render", json!({"target": by_id("click-counter")})).await;
        cl.json("inspect_settings", json!({})).await;
    }
    // nothing changed
    assert_eq!(c2.json("inspect_style", json!({"target": by_id("m3-styled")})).await["temporary_css_providers"], 0);

    // the layout-style tools only exist when the toolset is enabled
    let core_only = Broker::new(Policy { allow_mutations: true, ..Policy::default() }, Config::with_socket_dir(fx2.dir.path())).unwrap();
    let c3 = Client { broker: core_only };
    let e = c3.err("inspect_layout", json!({"target": by_id("m3-styled")})).await;
    assert_eq!(e.code, ErrorCode::InvalidParams);

    // batch: mutating steps follow the same gate, read-only steps run
    let r = c2
        .json("batch", json!({"steps": [
            {"tool": "inspect_layout", "arguments": {"target": by_id("m3-styled")}},
            {"tool": "audit_layout", "arguments": {"checks": ["zero_size"]}},
        ]}))
        .await;
    assert_eq!(r["steps"].as_array().unwrap().len(), 2);
    assert!(r["steps"].as_array().unwrap().iter().all(|s| s["ok"] == true), "{r}");
    let c1 = rw(&fx2);
    let r = c1
        .json("batch", json!({"steps": [
            {"tool": "apply_css", "arguments": {"css": ".m3-accent { background-color: #e01b24; }"}},
            {"tool": "inspect_style", "arguments": {"target": by_id("m3-styled")}},
        ]}))
        .await;
    assert_eq!(r["steps"][0]["result"]["installed"], true, "{r}");
    assert_eq!(r["steps"][1]["result"]["temporary_css_providers"], 1);
}
