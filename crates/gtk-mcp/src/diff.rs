//! Broker-side comparisons of retained snapshots and captures
//! (`docs/widget-tree.md` "Comparing snapshots", `docs/tools.md` `diff_captures`).

use image::{ImageFormat, Rgba, RgbaImage};
use serde_json::{Map, Value, json};
use std::collections::HashMap;

// -- snapshots ---------------------------------------------------------------

struct Node<'a> {
    node: &'a Value,
}

/// Index every node by ref, in tree order (roots first, depth first).
fn index(snapshot: &Value) -> (Vec<&str>, HashMap<&str, Node<'_>>) {
    fn walk<'a>(v: &'a Value, order: &mut Vec<&'a str>, map: &mut HashMap<&'a str, Node<'a>>) {
        if let Some(r) = v.get("ref").and_then(Value::as_str) {
            order.push(r);
            map.insert(r, Node { node: v });
        }
        if let Some(children) = v.get("children").and_then(Value::as_array) {
            for c in children {
                walk(c, order, map);
            }
        }
    }
    let mut order = Vec::new();
    let mut map = HashMap::new();
    if let Some(roots) = snapshot.get("roots").and_then(Value::as_array) {
        for r in roots {
            walk(r, &mut order, &mut map);
        }
    }
    (order, map)
}

fn type_of(n: &Value) -> Value {
    n.get("type").cloned().unwrap_or(Value::Null)
}

fn child_count(n: &Value) -> usize {
    n.get("children").and_then(Value::as_array).map_or(0, Vec::len)
}

fn size_of(n: &Value, key: &str) -> Value {
    match n.get(key) {
        Some(b) => json!({ "width": b.get("width").cloned().unwrap_or(Value::Null), "height": b.get("height").cloned().unwrap_or(Value::Null) }),
        None => Value::Null,
    }
}

fn changes(a: &Value, b: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    let mut push = |field: String, from: Value, to: Value| {
        if from != to {
            out.push(json!({ "field": field, "from": from, "to": to }));
        }
    };
    let empty = Map::new();
    let sa = a.get("state").and_then(Value::as_object).unwrap_or(&empty);
    let sb = b.get("state").and_then(Value::as_object).unwrap_or(&empty);
    let mut keys: Vec<&String> = sa.keys().chain(sb.keys()).collect();
    keys.sort();
    keys.dedup();
    for k in keys {
        push(format!("state.{k}"), sa.get(k).cloned().unwrap_or(Value::Null), sb.get(k).cloned().unwrap_or(Value::Null));
    }
    for k in ["text", "label", "attached"] {
        push(k.into(), a.get(k).cloned().unwrap_or(Value::Null), b.get(k).cloned().unwrap_or(Value::Null));
    }
    push("child_count".into(), json!(child_count(a)), json!(child_count(b)));
    push("allocation".into(), size_of(a, "allocation"), size_of(b, "allocation"));
    // Position within the root is not a change of the widget itself; size is.
    push("bounds_in_root.size".into(), size_of(a, "bounds_in_root"), size_of(b, "bounds_in_root"));
    out
}

/// Compare two snapshot documents by widget ref.
pub fn diff_snapshots(from_id: &str, from: &Value, to_id: &str, to: &Value) -> Value {
    let (order_a, map_a) = index(from);
    let (order_b, map_b) = index(to);
    let added: Vec<Value> = order_b
        .iter()
        .filter(|r| !map_a.contains_key(*r))
        .map(|r| json!({ "ref": r, "type": type_of(map_b[r].node) }))
        .collect();
    let removed: Vec<Value> = order_a
        .iter()
        .filter(|r| !map_b.contains_key(*r))
        .map(|r| json!({ "ref": r, "type": type_of(map_a[r].node) }))
        .collect();
    let mut changed = Vec::new();
    for r in &order_b {
        if let Some(a) = map_a.get(r) {
            let c = changes(a.node, map_b[r].node);
            if !c.is_empty() {
                changed.push(json!({ "ref": r, "type": type_of(map_b[r].node), "changes": c }));
            }
        }
    }
    json!({
        "from": from_id,
        "to": to_id,
        "sequence": { "from": from.get("sequence"), "to": to.get("sequence") },
        "added": added,
        "removed": removed,
        "changed": changed,
    })
}

// -- captures ----------------------------------------------------------------

/// Edge of one grid cell, in pixels.
const CELL: u32 = 16;
/// Upper bound on reported regions.
const MAX_REGIONS: usize = 8;

#[derive(Clone, Copy)]
struct Region {
    x0: u32,
    y0: u32,
    x1: u32, // exclusive
    y1: u32,
    pixels: u64,
}

impl Region {
    fn area(&self) -> u64 {
        u64::from(self.x1 - self.x0) * u64::from(self.y1 - self.y0)
    }
    fn union(&self, o: &Region) -> Region {
        Region {
            x0: self.x0.min(o.x0),
            y0: self.y0.min(o.y0),
            x1: self.x1.max(o.x1),
            y1: self.y1.max(o.y1),
            pixels: self.pixels + o.pixels,
        }
    }
    fn touches(&self, o: &Region) -> bool {
        self.x0 <= o.x1 && o.x0 <= self.x1 && self.y0 <= o.y1 && o.y0 <= self.y1
    }
}

pub fn decode_png(bytes: &[u8]) -> Result<RgbaImage, String> {
    image::load_from_memory_with_format(bytes, ImageFormat::Png).map(|i| i.to_rgba8()).map_err(|e| e.to_string())
}

fn encode_png(img: &RgbaImage) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(out)
}

pub struct CaptureDiff {
    pub report: Value,
    /// PNG of the diff image; `None` when sizes differ.
    pub image: Option<Vec<u8>>,
}

/// Exact pixel comparison (any differing RGBA channel counts as changed).
///
/// Regions: pixels are binned into `CELL`x`CELL` cells; touching cells (8-connected)
/// form components whose bounding boxes become regions; boxes that overlap or
/// touch are merged repeatedly, and while more than `MAX_REGIONS` remain the
/// pair whose union adds the least extra area is merged. Each region reports
/// its box in image pixels and the number of changed pixels inside its cells.
///
/// The diff image is `to` dimmed to 30 % with changed pixels painted opaque red.
pub fn diff_captures(from_id: &str, from_png: &[u8], to_id: &str, to_png: &[u8]) -> Result<CaptureDiff, String> {
    let a = decode_png(from_png)?;
    let b = decode_png(to_png)?;
    if a.dimensions() != b.dimensions() {
        return Ok(CaptureDiff {
            report: json!({
                "from": from_id,
                "to": to_id,
                "compared": false,
                "size_mismatch": { "from": [a.width(), a.height()], "to": [b.width(), b.height()] },
            }),
            image: None,
        });
    }
    let (w, h) = a.dimensions();
    let (cols, rows) = (w.div_ceil(CELL), h.div_ceil(CELL));
    let mut cells = vec![0u64; (cols * rows) as usize];
    let mut out = RgbaImage::new(w, h);
    let mut changed = 0u64;
    for y in 0..h {
        for x in 0..w {
            let (pa, pb) = (a.get_pixel(x, y), b.get_pixel(x, y));
            if pa != pb {
                changed += 1;
                cells[((y / CELL) * cols + x / CELL) as usize] += 1;
                out.put_pixel(x, y, Rgba([255, 0, 0, 255]));
            } else {
                let [r, g, bl, _] = pb.0;
                out.put_pixel(x, y, Rgba([(r as u16 * 3 / 10) as u8, (g as u16 * 3 / 10) as u8, (bl as u16 * 3 / 10) as u8, 255]));
            }
        }
    }
    let regions = regions(&cells, cols, rows, w, h);
    let total = u64::from(w) * u64::from(h);
    let report = json!({
        "from": from_id,
        "to": to_id,
        "compared": true,
        "width": w,
        "height": h,
        "changed_pixels": changed,
        "total_pixels": total,
        "changed_ratio": if total == 0 { 0.0 } else { changed as f64 / total as f64 },
        "regions": regions.iter().map(|r| json!({
            "x": r.x0, "y": r.y0, "width": r.x1 - r.x0, "height": r.y1 - r.y0, "changed_pixels": r.pixels,
        })).collect::<Vec<_>>(),
    });
    let image = Some(encode_png(&out)?);
    Ok(CaptureDiff { report, image })
}

fn regions(cells: &[u64], cols: u32, rows: u32, w: u32, h: u32) -> Vec<Region> {
    let mut seen = vec![false; cells.len()];
    let mut out: Vec<Region> = Vec::new();
    for start in 0..cells.len() {
        if cells[start] == 0 || seen[start] {
            continue;
        }
        let mut stack = vec![start];
        seen[start] = true;
        let (mut cx0, mut cy0, mut cx1, mut cy1, mut px) = (u32::MAX, u32::MAX, 0u32, 0u32, 0u64);
        while let Some(i) = stack.pop() {
            let (cx, cy) = (i as u32 % cols, i as u32 / cols);
            cx0 = cx0.min(cx);
            cy0 = cy0.min(cy);
            cx1 = cx1.max(cx + 1);
            cy1 = cy1.max(cy + 1);
            px += cells[i];
            for dy in -1i64..=1 {
                for dx in -1i64..=1 {
                    let (nx, ny) = (i64::from(cx) + dx, i64::from(cy) + dy);
                    if nx < 0 || ny < 0 || nx >= i64::from(cols) || ny >= i64::from(rows) {
                        continue;
                    }
                    let n = (ny as u32 * cols + nx as u32) as usize;
                    if cells[n] != 0 && !seen[n] {
                        seen[n] = true;
                        stack.push(n);
                    }
                }
            }
        }
        out.push(Region {
            x0: cx0 * CELL,
            y0: cy0 * CELL,
            x1: (cx1 * CELL).min(w),
            y1: (cy1 * CELL).min(h),
            pixels: px,
        });
    }
    // Merge boxes that overlap/touch (component boxes can interleave).
    'again: loop {
        for i in 0..out.len() {
            for j in i + 1..out.len() {
                if out[i].touches(&out[j]) {
                    let u = out[i].union(&out[j]);
                    out[i] = u;
                    out.swap_remove(j);
                    continue 'again;
                }
            }
        }
        break;
    }
    while out.len() > MAX_REGIONS {
        let mut best = (0, 1, u64::MAX);
        for i in 0..out.len() {
            for j in i + 1..out.len() {
                let growth = out[i].union(&out[j]).area().saturating_sub(out[i].area() + out[j].area());
                if growth < best.2 {
                    best = (i, j, growth);
                }
            }
        }
        let u = out[best.0].union(&out[best.1]);
        out[best.0] = u;
        out.swap_remove(best.1);
    }
    out.sort_by_key(|r| (r.y0, r.x0));
    out
}
