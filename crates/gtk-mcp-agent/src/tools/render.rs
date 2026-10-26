//! `inspect_render`: the GSK render-node tree of a widget's last frame
//! (tools.md layout-style; secrecy plan 4.4).
//!
//! The tree comes from a `GtkWidgetPaintable` snapshot, the same source as
//! `capture`. Secrecy: targets whose subtree holds a secret object that is
//! revealing its content are refused (`redacted`); text nodes that touch a
//! secret widget are replaced and carry neither glyph count, font nor bounds
//! (the glyph count of a password field is its length), and every node inside
//! a secret widget keeps only its type (caret, selection and scroll geometry
//! encode the length too). When there are too many widgets to find every secret
//! field, the whole target counts as secret.

use crate::agent::{Agent, Ctx};
use crate::registry::{ObjState, widget_state};
use crate::tools::style::wait_one_paint;
use crate::{secrecy, selector};
use glib::prelude::*;
use gtk4 as gtk;
use gtk4::gsk::{self, RenderNodeType};
use gtk4::pango::prelude::FontExt;
use gtk4::prelude::*;
use gtk4::{gdk, graphene};
use gtk_mcp_protocol::{AgentError, ErrorCode, InspectRenderParams};
use serde_json::{Map, Value, json};
use std::rc::Rc;
use std::time::Duration;

const DEFAULT_MAX_NODES: u32 = 500;
const HARD_MAX_NODES: u32 = 5000;
const DEFAULT_MAX_DEPTH: u32 = 32;
const MAX_WALK: usize = 20_000;
const FRAME_TIMEOUT: Duration = Duration::from_millis(2000);

struct Walk {
    nodes_left: u32,
    max_depth: u32,
    truncated: bool,
    secret_regions: Vec<graphene::Rect>,
    whole_secret: bool,
    redacted_text: usize,
    emitted: u32,
}

fn rect_json(r: &graphene::Rect) -> Value {
    json!({"x": r.x(), "y": r.y(), "width": r.width(), "height": r.height()})
}

fn rgba(c: &gdk::RGBA) -> String {
    c.to_str().to_string()
}

/// `GskContainerNode` -> `container`, `GskLinearGradientNode` -> `linear_gradient`.
fn type_label(node: &gsk::RenderNode) -> String {
    let name = node.type_().name();
    let name = name.strip_prefix("Gsk").unwrap_or(name);
    let name = name.strip_suffix("Node").unwrap_or(name);
    let mut out = String::new();
    for (i, ch) in name.chars().enumerate() {
        if ch.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

fn nick(v: &glib::Value) -> Option<String> {
    glib::EnumValue::from_value(v).map(|(_, e)| e.nick().to_string())
}

macro_rules! child_of {
    ($node:expr, $t:ty) => {
        $node.downcast_ref::<$t>().map(|n| vec![n.child()]).unwrap_or_default()
    };
}

/// Key attributes and children of one node. Types whose children are not
/// exposed by the bindings are flagged `children_omitted`.
fn describe(node: &gsk::RenderNode, attrs: &mut Map<String, Value>) -> (Vec<(gsk::RenderNode, Option<gsk::Transform>)>, bool) {
    use RenderNodeType as T;
    let plain = |v: Vec<gsk::RenderNode>| v.into_iter().map(|c| (c, None)).collect::<Vec<_>>();
    match node.node_type() {
        T::ContainerNode => {
            let n = node.downcast_ref::<gsk::ContainerNode>().expect("container node");
            attrs.insert("n_children".into(), json!(n.n_children()));
            (plain((0..n.n_children()).map(|i| n.child(i)).collect()), false)
        }
        T::TransformNode => {
            let n = node.downcast_ref::<gsk::TransformNode>().expect("transform node");
            let t = n.transform();
            attrs.insert("transform".into(), json!(t.to_str().to_string()));
            (vec![(n.child(), Some(t))], false)
        }
        T::OpacityNode => {
            let n = node.downcast_ref::<gsk::OpacityNode>().expect("opacity node");
            attrs.insert("opacity".into(), json!(n.opacity()));
            (plain(vec![n.child()]), false)
        }
        T::ClipNode => {
            let n = node.downcast_ref::<gsk::ClipNode>().expect("clip node");
            attrs.insert("clip".into(), rect_json(&n.clip()));
            (plain(vec![n.child()]), false)
        }
        T::RoundedClipNode => {
            let n = node.downcast_ref::<gsk::RoundedClipNode>().expect("rounded clip node");
            attrs.insert("clip".into(), rect_json(n.clip().bounds()));
            (plain(vec![n.child()]), false)
        }
        T::ColorMatrixNode => (plain(child_of!(node, gsk::ColorMatrixNode)), false),
        T::RepeatNode => {
            let n = node.downcast_ref::<gsk::RepeatNode>().expect("repeat node");
            attrs.insert("child_bounds".into(), rect_json(&n.child_bounds()));
            (plain(vec![n.child()]), false)
        }
        T::BlurNode => {
            let n = node.downcast_ref::<gsk::BlurNode>().expect("blur node");
            attrs.insert("radius".into(), json!(n.radius()));
            (plain(vec![n.child()]), false)
        }
        T::DebugNode => {
            let n = node.downcast_ref::<gsk::DebugNode>().expect("debug node");
            attrs.insert("message".into(), json!(n.message().to_string()));
            (plain(vec![n.child()]), false)
        }
        T::ShadowNode => {
            let n = node.downcast_ref::<gsk::ShadowNode>().expect("shadow node");
            attrs.insert("n_shadows".into(), json!(n.n_shadows()));
            (plain(vec![n.child()]), false)
        }
        T::BlendNode => {
            let n = node.downcast_ref::<gsk::BlendNode>().expect("blend node");
            attrs.insert("blend_mode".into(), json!(nick(&n.blend_mode().to_value())));
            (plain(vec![n.bottom_child(), n.top_child()]), false)
        }
        T::CrossFadeNode => {
            let n = node.downcast_ref::<gsk::CrossFadeNode>().expect("cross-fade node");
            attrs.insert("progress".into(), json!(n.progress()));
            (plain(vec![n.start_child(), n.end_child()]), false)
        }
        T::MaskNode => {
            let n = node.downcast_ref::<gsk::MaskNode>().expect("mask node");
            attrs.insert("mask_mode".into(), json!(nick(&n.mask_mode().to_value())));
            (plain(vec![n.source(), n.mask()]), false)
        }
        T::ColorNode => {
            let n = node.downcast_ref::<gsk::ColorNode>().expect("color node");
            attrs.insert("color".into(), json!(rgba(&n.color())));
            (vec![], false)
        }
        T::BorderNode => {
            let n = node.downcast_ref::<gsk::BorderNode>().expect("border node");
            attrs.insert("widths".into(), json!(n.widths()));
            attrs.insert("colors".into(), json!(n.colors().iter().map(rgba).collect::<Vec<_>>()));
            (vec![], false)
        }
        T::TextureNode => {
            let n = node.downcast_ref::<gsk::TextureNode>().expect("texture node");
            let t = n.texture();
            attrs.insert("texture".into(), json!({"width": t.width(), "height": t.height()}));
            (vec![], false)
        }
        T::TextureScaleNode => {
            let n = node.downcast_ref::<gsk::TextureScaleNode>().expect("texture-scale node");
            let t = n.texture();
            attrs.insert("texture".into(), json!({"width": t.width(), "height": t.height()}));
            (vec![], false)
        }
        T::TextNode => {
            let n = node.downcast_ref::<gsk::TextNode>().expect("text node");
            attrs.insert("glyphs".into(), json!(n.num_glyphs()));
            attrs.insert("font".into(), json!(n.font().describe().to_str().to_string()));
            attrs.insert("color".into(), json!(rgba(&n.color())));
            let o = n.offset();
            attrs.insert("offset".into(), json!({"x": o.x(), "y": o.y()}));
            attrs.insert("has_color_glyphs".into(), json!(n.has_color_glyphs()));
            (vec![], false)
        }
        // Leaves without attributes worth listing.
        T::CairoNode
        | T::LinearGradientNode
        | T::RepeatingLinearGradientNode
        | T::RadialGradientNode
        | T::RepeatingRadialGradientNode
        | T::ConicGradientNode
        | T::InsetShadowNode
        | T::OutsetShadowNode
        | T::GlShaderNode => (vec![], false),
        // Node types newer than these bindings may have children we cannot read.
        _ => (vec![], true),
    }
}

fn overlaps_secret(w: &Walk, abs: &graphene::Rect) -> bool {
    w.whole_secret || w.secret_regions.iter().any(|r| r.intersection(abs).is_some())
}

fn inside_secret(w: &Walk, abs: &graphene::Rect) -> bool {
    w.whole_secret || w.secret_regions.iter().any(|r| r.contains_rect(abs))
}

/// `inside`: an ancestor lies inside a secret region. Such nodes, and nodes that
/// lie inside one themselves, keep only their type: the caret position, the
/// selection width or the scroll offset of a secret field encode its length.
fn serialize(node: &gsk::RenderNode, acc: &gsk::Transform, depth: u32, inside: bool, w: &mut Walk) -> Option<Value> {
    if w.nodes_left == 0 {
        w.truncated = true;
        return None;
    }
    w.nodes_left -= 1;
    w.emitted += 1;
    let label = type_label(node);
    let abs = acc.transform_bounds(&node.bounds());
    let inside = inside || inside_secret(w, &abs);
    if node.node_type() == RenderNodeType::TextNode && (inside || overlaps_secret(w, &abs)) {
        w.redacted_text += 1;
        return Some(json!({"type": label, "redacted": true}));
    }
    let mut attrs = Map::new();
    let (children, omitted) = describe(node, &mut attrs);
    let mut m = Map::new();
    m.insert("type".into(), json!(label));
    if inside {
        m.insert("redacted".into(), json!(true));
    } else {
        m.insert("bounds".into(), rect_json(&node.bounds()));
        m.extend(attrs);
        if omitted {
            m.insert("children_omitted".into(), json!(true));
        }
    }
    if !children.is_empty() {
        if depth + 1 >= w.max_depth {
            w.truncated = true;
            m.insert("children_cut".into(), json!(children.len()));
        } else {
            let mut out = Vec::new();
            for (child, transform) in children {
                let child_acc = match &transform {
                    Some(t) => acc.clone().transform(Some(t)),
                    None => acc.clone(),
                };
                match serialize(&child, &child_acc, depth + 1, inside, w) {
                    Some(v) => out.push(v),
                    None => break,
                }
            }
            m.insert("children".into(), Value::Array(out));
        }
    }
    Some(Value::Object(m))
}

/// Bounds of the outermost secret nodes below `root` (exclusive), in tree
/// order. `None` when the walk would visit more than `budget` nodes: the caller
/// then treats the whole target as secret instead of missing a field.
fn secret_regions_of<N>(
    root: &N,
    budget: usize,
    children: &dyn Fn(&N) -> Vec<N>,
    secret_bounds: &dyn Fn(&N) -> Option<Option<graphene::Rect>>,
) -> Option<Vec<graphene::Rect>> {
    fn walk<N>(
        n: &N,
        budget: &mut usize,
        children: &dyn Fn(&N) -> Vec<N>,
        secret_bounds: &dyn Fn(&N) -> Option<Option<graphene::Rect>>,
        out: &mut Vec<graphene::Rect>,
    ) -> bool {
        for child in children(n) {
            if *budget == 0 {
                return false;
            }
            *budget -= 1;
            match secret_bounds(&child) {
                Some(b) => out.extend(b),
                None => {
                    if !walk(&child, budget, children, secret_bounds, out) {
                        return false;
                    }
                }
            }
        }
        true
    }
    let mut out = Vec::new();
    let mut budget = budget;
    walk(root, &mut budget, children, secret_bounds, &mut out).then_some(out)
}

/// Bounds (relative to `root`'s own bounds origin) of the secret widgets under
/// `root`; `None` when there are too many widgets to tell. The caller handles
/// the case that `root` itself is secret.
fn secret_regions(root: &gtk::Widget, origin: &graphene::Rect) -> Option<Vec<graphene::Rect>> {
    let children = |w: &gtk::Widget| {
        let mut v = Vec::new();
        let mut c = w.first_child();
        while let Some(child) = c {
            c = child.next_sibling();
            v.push(child);
        }
        v
    };
    let secret_bounds = |w: &gtk::Widget| {
        secrecy::is_secret(w).then(|| {
            w.compute_bounds(root)
                .map(|b| graphene::Rect::new(b.x() - origin.x(), b.y() - origin.y(), b.width(), b.height()))
        })
    };
    secret_regions_of(root, MAX_WALK, &children, &secret_bounds)
}

pub async fn inspect_render(agent: &Rc<Agent>, p: InspectRenderParams, ctx: &Ctx) -> Result<Value, AgentError> {
    let w = selector::resolve_widget(agent, &p.target, true)?;
    let r = agent.registry.ref_for(&w);
    if widget_state(&w) == ObjState::Detached {
        return Err(AgentError::new(ErrorCode::Detached, format!("{r} is alive but not in a window")));
    }
    if !w.is_mapped() {
        return Err(AgentError::new(ErrorCode::TargetUnmapped, format!("{r} is not mapped; there is no render tree")));
    }
    if secrecy::subtree_peeking(&w) {
        return Err(AgentError::new(
            ErrorCode::Redacted,
            format!("{r} contains a secret field that is currently revealing its content (visibility=true)"),
        ));
    }
    let max_nodes = p.max_nodes.unwrap_or(DEFAULT_MAX_NODES).clamp(1, HARD_MAX_NODES);
    let max_depth = p.max_depth.unwrap_or(DEFAULT_MAX_DEPTH).max(1);

    let paintable = gtk::WidgetPaintable::new(Some(&w));
    let bounds = w
        .compute_bounds(&w)
        .ok_or_else(|| AgentError::new(ErrorCode::CaptureUnavailable, "the widget has no bounds (it has never been allocated)"))?;
    let snapshot_node = |paintable: &gtk::WidgetPaintable| {
        let s = gtk::Snapshot::new();
        paintable.snapshot(&s, f64::from(bounds.width()), f64::from(bounds.height()));
        s.to_node()
    };
    let mut node = snapshot_node(&paintable);
    if node.is_none() {
        // Pending changes: GTK has no render node until the next frame paints the widget.
        let clock = w
            .frame_clock()
            .ok_or_else(|| AgentError::new(ErrorCode::CaptureUnavailable, "the widget has no frame clock (not realized)"))?;
        wait_one_paint(&w, clock, ctx, FRAME_TIMEOUT).await;
        ctx.check()?;
        node = snapshot_node(&paintable);
    }
    let node = node.ok_or_else(|| {
        AgentError::new(ErrorCode::CaptureUnavailable, "GTK has no render data for the widget (never drawn, or outside the visible area)")
    })?;
    // The secrecy decision is taken after the wait: the widget may have changed meanwhile.
    if secrecy::subtree_peeking(&w) {
        return Err(AgentError::new(ErrorCode::Redacted, format!("{r} now contains a secret field that is revealing its content")));
    }

    // Too many widgets to find every secret field: treat the whole target as secret.
    let regions = secret_regions(&w, &bounds);
    let mut walk = Walk {
        nodes_left: max_nodes,
        max_depth,
        truncated: false,
        whole_secret: secrecy::is_secret(&w) || regions.is_none(),
        secret_regions: regions.unwrap_or_default(),
        redacted_text: 0,
        emitted: 0,
    };
    let tree = serialize(&node, &gsk::Transform::new(), 0, false, &mut walk).unwrap_or(Value::Null);
    Ok(json!({
        "target": r,
        "size": {"width": bounds.width(), "height": bounds.height()},
        "node_count": walk.emitted,
        "truncated": walk.truncated,
        "max_nodes": max_nodes,
        "max_depth": max_depth,
        "secret_regions": walk.secret_regions.len() + usize::from(walk.whole_secret),
        "redacted_text_nodes": walk.redacted_text,
        "notes": [
            "text nodes carry glyph ids, not characters: only glyph count, font and colour are listed",
            "nodes inside a secret field keep only their type (redacted: true)",
        ],
        "root": tree,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct N {
        secret: bool,
        kids: Vec<N>,
    }

    fn leaf(secret: bool) -> N {
        N { secret, kids: vec![] }
    }

    fn kids<'a>(n: &&'a N) -> Vec<&'a N> {
        n.kids.iter().collect()
    }

    fn regions(root: &N, budget: usize) -> Option<usize> {
        let secret = |n: &&N| n.secret.then(|| Some(graphene::Rect::new(0.0, 0.0, 1.0, 1.0)));
        secret_regions_of(&root, budget, &kids, &secret).map(|v| v.len())
    }

    #[test]
    fn secret_regions_fail_closed_when_the_budget_runs_out() {
        // root -> [plain -> [secret], secret]: 3 widgets visited (the secret's own
        // children never are).
        let tree = N { secret: false, kids: vec![N { secret: false, kids: vec![leaf(true)] }, leaf(true)] };
        assert_eq!(regions(&tree, 3), Some(2));
        assert_eq!(regions(&tree, 2), None, "a secret beyond the budget must not be missed silently");
        assert_eq!(regions(&leaf(false), 0), Some(0));
    }
}
