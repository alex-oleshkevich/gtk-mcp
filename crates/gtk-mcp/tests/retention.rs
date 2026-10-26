mod common;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::*;
use gtk_mcp::{Broker, Policy};
use gtk_mcp_protocol::*;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

fn snap(n: u64) -> Value {
    let mut children = vec![json!({"ref":"w2","type":"GtkButton","label":"Save","state":{"sensitive": n < 2},"children":[]})];
    let mut width = 100;
    if n >= 2 {
        children.push(json!({"ref":"w3","type":"GtkEntry","text":"a","children":[]}));
        width = 120;
    }
    json!({
        "snapshot_id": format!("snap-{n}"),
        "sequence": n,
        "roots": [{
            "ref":"w1","type":"GtkWindow","title":"Prefs",
            "state":{"visible":true,"mapped":true,"sensitive":true},
            "bounds_in_root":{"x":0,"y":0,"width":width,"height":50},
            "children": children,
        }],
        "node_count": 1 + children.len(),
        "complete": true,
        "truncated": false,
    })
}

fn snapshot_mock(dir: &std::path::Path) -> MockAgent {
    let counter = Arc::new(AtomicU64::new(0));
    MockAgent::builder(dir)
        .script("snapshot", vec![Step::Started, Step::RespondFn(Arc::new(move |_| snap(counter.fetch_add(1, Ordering::SeqCst) + 1)))])
        .start()
}

fn setup(mock: impl FnOnce(&std::path::Path) -> MockAgent, tweak: impl FnOnce(&mut gtk_mcp::Config)) -> (tempfile::TempDir, MockAgent, Arc<Broker>) {
    let dir = tempfile::tempdir().unwrap();
    let m = mock(dir.path());
    let b = broker_with(dir.path(), Policy::default(), tweak);
    (dir, m, b)
}

#[tokio::test]
async fn snapshot_text_default_with_id_line_and_json_format() {
    let (_d, _m, b) = setup(snapshot_mock, |_| {});
    let out = b.call_tool("snapshot", json!({})).await.unwrap();
    let text = out.text_content();
    assert!(text.contains("GtkWindow \"Prefs\" [w1]"), "{text}");
    assert!(text.contains("GtkButton \"Save\" [w2]"), "{text}");
    assert!(text.ends_with("snapshot_id: snap-1"), "{text}");

    let out = b.call_tool("snapshot", json!({"format":"json"})).await.unwrap();
    let v = out.json_content().unwrap();
    assert_eq!(v["snapshot_id"], "snap-2");
    assert_eq!(v["session"], "s1");
    assert_eq!(v["roots"][0]["ref"], "w1");
}

#[tokio::test]
async fn snapshot_retention_evicts_oldest() {
    let (_d, _m, b) = setup(snapshot_mock, |c| c.retain_snapshots = 2);
    for _ in 0..3 {
        b.call_tool("snapshot", json!({})).await.unwrap();
    }
    let e = b.call_tool("diff_snapshots", json!({"from":"snap-1","to":"snap-3"})).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::SnapshotUnavailable);
    let e = b.call_tool("diff_snapshots", json!({"from":"snap-2","to":"snap-99"})).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::SnapshotUnavailable);
    b.call_tool("diff_snapshots", json!({"from":"snap-2","to":"snap-3"})).await.unwrap();
}

#[tokio::test]
async fn snapshot_diff_reports_added_removed_changed() {
    let (_d, mock, b) = setup(snapshot_mock, |_| {});
    for _ in 0..2 {
        b.call_tool("snapshot", json!({})).await.unwrap();
    }
    let n_before = mock.requests().len();
    let d = b.call_tool("diff_snapshots", json!({"from":"snap-1","to":"snap-2"})).await.unwrap().json_content().unwrap();
    assert_eq!(mock.requests().len(), n_before, "diff is broker-only");
    assert_eq!(d["added"], json!([{"ref":"w3","type":"GtkEntry"}]));
    assert_eq!(d["removed"], json!([]));
    let changed = d["changed"].as_array().unwrap();
    let w1 = changed.iter().find(|c| c["ref"] == "w1").unwrap();
    let fields: Vec<&str> = w1["changes"].as_array().unwrap().iter().map(|c| c["field"].as_str().unwrap()).collect();
    assert_eq!(fields, vec!["child_count", "bounds_in_root.size"]);
    let w2 = changed.iter().find(|c| c["ref"] == "w2").unwrap();
    assert_eq!(w2["changes"], json!([{"field":"state.sensitive","from":true,"to":false}]));
    assert_eq!(changed.len(), 2);

    // reversed: w3 removed
    let d = b.call_tool("diff_snapshots", json!({"from":"snap-2","to":"snap-1"})).await.unwrap().json_content().unwrap();
    assert_eq!(d["removed"], json!([{"ref":"w3","type":"GtkEntry"}]));
    // identical
    let d = b.call_tool("diff_snapshots", json!({"from":"snap-2","to":"snap-2"})).await.unwrap().json_content().unwrap();
    assert_eq!(d["changed"], json!([]));
}

#[tokio::test]
async fn snapshot_without_id_is_an_error() {
    let (_d, _m, b) = setup(|d| MockAgent::builder(d).respond("snapshot", json!({"roots":[]})).start(), |_| {});
    let e = b.call_tool("snapshot", json!({})).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Unavailable);
}

// -- captures -----------------------------------------------------------------

fn capture_mock(dir: &std::path::Path, pngs: Vec<Vec<u8>>) -> MockAgent {
    let counter = Arc::new(AtomicU64::new(0));
    MockAgent::builder(dir)
        .script(
            "capture",
            vec![
                Step::Started,
                Step::RespondFn(Arc::new(move |_| {
                    let n = counter.fetch_add(1, Ordering::SeqCst) as usize;
                    let png = &pngs[n];
                    let (w, h) = image::load_from_memory(png).map(|i| (i.width(), i.height())).unwrap();
                    let result = CaptureResult {
                        capture_id: format!("cap-{}", n + 1),
                        target: "w1".into(),
                        width: w,
                        height: h,
                        scale: 1.0,
                        stale: false,
                        stale_reason: None,
                        annotations: vec![Annotation { r#ref: "w2".into(), x: 1.0, y: 2.0, width: 3.0, height: 4.0 }],
                    };
                    serde_json::to_value(CaptureResponse { result, png_base64: STANDARD.encode(png) }).unwrap()
                })),
            ],
        )
        .start()
}

fn gray(w: u32, h: u32) -> Vec<u8> {
    png(w, h, |_, _| [200, 200, 200, 255])
}

#[tokio::test]
async fn capture_returns_metadata_and_png_and_retains() {
    let a = gray(64, 48);
    let (_d, _m, b) = setup(|d| capture_mock(d, vec![a.clone(), a.clone(), a.clone()]), |c| c.retain_captures = 2);
    let out = b.call_tool("capture", json!({"target":"w1"})).await.unwrap();
    assert_eq!(out.contents.len(), 2);
    let meta = out.json_content().unwrap();
    assert_eq!(meta["capture_id"], "cap-1");
    assert_eq!(meta["width"], 64);
    assert_eq!(meta["annotations"][0]["ref"], "w2");
    assert!(meta.get("png_base64").is_none());
    assert!(!out.text_content().contains("png_base64"));
    assert_eq!(out.images(), vec![a.as_slice()]);

    b.call_tool("capture", json!({"target":"w1"})).await.unwrap();
    b.call_tool("capture", json!({"target":"w1"})).await.unwrap();
    let e = b.call_tool("diff_captures", json!({"from":"cap-1","to":"cap-3"})).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::SnapshotUnavailable, "cap-1 was evicted");
    b.call_tool("diff_captures", json!({"from":"cap-2","to":"cap-3"})).await.unwrap();
}

#[tokio::test]
async fn capture_diff_pixels_regions_and_image() {
    let a = gray(64, 48);
    let bimg = png(64, 48, |x, y| if (20..30).contains(&x) && (20..30).contains(&y) { [0, 0, 255, 255] } else { [200, 200, 200, 255] });
    let (_d, _m, b) = setup(|d| capture_mock(d, vec![a, bimg]), |_| {});
    b.call_tool("capture", json!({"target":"w1"})).await.unwrap();
    b.call_tool("capture", json!({"target":"w1"})).await.unwrap();
    let out = b.call_tool("diff_captures", json!({"from":"cap-1","to":"cap-2"})).await.unwrap();
    let r = out.json_content().unwrap();
    assert_eq!(r["compared"], true);
    assert_eq!(r["changed_pixels"], 100);
    assert_eq!(r["total_pixels"], 64 * 48);
    assert!((r["changed_ratio"].as_f64().unwrap() - 100.0 / 3072.0).abs() < 1e-9);
    assert_eq!(r["regions"], json!([{"x":16,"y":16,"width":16,"height":16,"changed_pixels":100}]));
    let img = image::load_from_memory(out.images()[0]).unwrap().to_rgba8();
    assert_eq!(img.dimensions(), (64, 48));
    assert_eq!(img.get_pixel(25, 25).0, [255, 0, 0, 255]);
    assert_eq!(img.get_pixel(0, 0).0, [60, 60, 60, 255], "unchanged pixels are dimmed");
}

#[tokio::test]
async fn capture_diff_merges_cells_into_few_regions() {
    let a = gray(128, 128);
    let bimg = png(128, 128, |x, y| {
        let near = |cx: u32, cy: u32| x >= cx && x < cx + 20 && y >= cy && y < cy + 5;
        if near(0, 0) || near(100, 100) { [0, 0, 0, 255] } else { [200, 200, 200, 255] }
    });
    let (_d, _m, b) = setup(|d| capture_mock(d, vec![a, bimg]), |_| {});
    for _ in 0..2 {
        b.call_tool("capture", json!({"target":"w1"})).await.unwrap();
    }
    let r = b.call_tool("diff_captures", json!({"from":"cap-1","to":"cap-2"})).await.unwrap().json_content().unwrap();
    let regions = r["regions"].as_array().unwrap();
    assert_eq!(regions.len(), 2, "{regions:?}");
    assert_eq!((regions[0]["x"].as_u64(), regions[0]["y"].as_u64()), (Some(0), Some(0)));
    assert_eq!(regions[0]["width"], 32);
    assert_eq!(regions[1]["x"], 96);
    assert_eq!(r["changed_pixels"], 200);
}

#[tokio::test]
async fn capture_diff_identical_has_zero_ratio_and_no_regions() {
    let a = gray(32, 32);
    let (_d, _m, b) = setup(|d| capture_mock(d, vec![a.clone(), a]), |_| {});
    for _ in 0..2 {
        b.call_tool("capture", json!({"target":"w1"})).await.unwrap();
    }
    let r = b.call_tool("diff_captures", json!({"from":"cap-1","to":"cap-2"})).await.unwrap().json_content().unwrap();
    assert_eq!(r["changed_pixels"], 0);
    assert_eq!(r["changed_ratio"], 0.0);
    assert_eq!(r["regions"], json!([]));
}

#[tokio::test]
async fn capture_diff_size_mismatch_is_explicit() {
    let (_d, _m, b) = setup(|d| capture_mock(d, vec![gray(64, 48), gray(32, 32)]), |_| {});
    for _ in 0..2 {
        b.call_tool("capture", json!({"target":"w1"})).await.unwrap();
    }
    let out = b.call_tool("diff_captures", json!({"from":"cap-1","to":"cap-2"})).await.unwrap();
    let r = out.json_content().unwrap();
    assert_eq!(r["size_mismatch"], json!({"from":[64,48],"to":[32,32]}));
    assert_eq!(r["compared"], false);
    assert!(r.get("changed_ratio").is_none());
    assert!(out.images().is_empty());
}
