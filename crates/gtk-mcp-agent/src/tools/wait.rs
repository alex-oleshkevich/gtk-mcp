//! `wait_for`: bounded, cancellable wait for a condition. The condition is
//! re-evaluated whenever the watched object notifies and at least every
//! 25 ms (selectors are re-resolved every time, so widgets that appear later
//! are found). A timeout is a normal result (`satisfied: false`).

use crate::agent::{Agent, Ctx};
use crate::json::{json_to_value_unchecked, value_to_json};
use crate::{secrecy, selector, widgets};
use gio::prelude::*;
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk_mcp_protocol::{AgentError, ErrorCode, Target, WaitCondition, WaitForParams};
use serde_json::{Value, json};
use std::rc::Rc;
use std::task::Poll;
use std::time::{Duration, Instant};

const DEFAULT_TIMEOUT_MS: u64 = 5000;
const MAX_TIMEOUT_MS: u64 = 60_000;
const POLL: Duration = Duration::from_millis(25);
/// Answer this long before the request deadline so the reply wins the race
/// against the broker's own timeout.
const DEADLINE_MARGIN: Duration = Duration::from_millis(100);
const MAX_MATCHES: usize = 20;
const MAX_OBSERVED_TEXT: usize = 512;

struct Eval {
    satisfied: bool,
    matches: Vec<String>,
    observed: Value,
    reason: Option<String>,
    /// Object whose notifications should wake the loop.
    watch: Option<glib::Object>,
}

impl Eval {
    fn pending(reason: impl Into<String>) -> Self {
        Eval { satisfied: false, matches: vec![], observed: Value::Null, reason: Some(reason.into()), watch: None }
    }
}

/// The notify handler of the currently watched object; disconnected when the
/// watched object changes and on drop (completion, error, cancellation).
#[derive(Default)]
struct Watch {
    cur: Option<(glib::WeakRef<glib::Object>, glib::SignalHandlerId)>,
}

impl Watch {
    fn release(&mut self) {
        if let Some((weak, id)) = self.cur.take() {
            if let Some(o) = weak.upgrade() {
                o.disconnect(id);
            }
        }
    }

    fn update(&mut self, want: Option<&glib::Object>, tx: &async_channel::Sender<()>, property: Option<&str>) {
        let same = match (&self.cur, want) {
            (Some((weak, _)), Some(w)) => weak.upgrade().as_ref() == Some(w),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        self.release();
        if let Some(obj) = want {
            let tx = tx.clone();
            let id = obj.connect_notify_local(property, move |_, _| {
                let _ = tx.try_send(());
            });
            self.cur = Some((obj.downgrade(), id));
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.release();
    }
}

/// Resolve to all currently live matches (for exists / absent / match_count).
fn live_matches(agent: &Agent, target: &Target, stale_is_empty: bool) -> Result<Vec<glib::Object>, AgentError> {
    match target {
        Target::Ref(r) => match agent.registry.resolve(r) {
            Ok(o) => {
                let live = match o.downcast_ref::<gtk::Widget>() {
                    Some(w) => crate::registry::widget_state(w) == crate::registry::ObjState::Live,
                    None => true,
                };
                Ok(if live { vec![o] } else { vec![] })
            }
            Err(e) if e.code == ErrorCode::StaleReference && stale_is_empty => Ok(vec![]),
            Err(e) => Err(e),
        },
        Target::Select(sel) => match selector::find(agent, sel) {
            Ok(hits) => Ok(hits.into_iter().map(|h| h.widget.upcast()).collect()),
            // `nth` beyond the matches and vanished windows mean "nothing there (yet)".
            Err(e) if matches!(e.code, ErrorCode::NotFound | ErrorCode::StaleReference) => Ok(vec![]),
            Err(e) => Err(e),
        },
    }
}

enum Single {
    One(glib::Object),
    Pending(String),
}

/// Resolve to exactly one object, or report why not (yet).
fn single(agent: &Agent, target: &Target) -> Result<Single, AgentError> {
    match target {
        Target::Ref(r) => Ok(Single::One(agent.registry.resolve(r)?)),
        Target::Select(_) => {
            let mut hits = live_matches(agent, target, false)?;
            Ok(match hits.len() {
                0 => Single::Pending("the selector matched no widget".into()),
                1 => Single::One(hits.remove(0)),
                n => Single::Pending(format!("the selector matched {n} widgets; waiting for exactly one")),
            })
        }
    }
}

fn truncated(s: &str) -> String {
    s.chars().take(MAX_OBSERVED_TEXT).collect()
}

fn text_of(w: &gtk::Widget) -> Option<String> {
    widgets::text_of(w).or_else(|| widgets::label_of(w)).or_else(|| widgets::title_of(w))
}

fn need_widget(obj: &glib::Object, what: &str) -> Result<gtk::Widget, AgentError> {
    obj.clone().downcast::<gtk::Widget>().map_err(|o| {
        AgentError::new(ErrorCode::UnsupportedOperation, format!("{what} needs a widget; the target is a {}", o.type_().name()))
    })
}

fn evaluate(agent: &Rc<Agent>, cond: &WaitCondition, target: Option<&Target>) -> Result<Eval, AgentError> {
    if let WaitCondition::AppBusy { busy } = cond {
        let want = busy.unwrap_or(false);
        let cur = agent.app.is_busy();
        return Ok(Eval {
            satisfied: cur == want,
            matches: vec![],
            observed: json!(cur),
            reason: (cur != want).then(|| format!("GApplication:is-busy is {cur}")),
            watch: Some(agent.app.clone().upcast()),
        });
    }
    let target = target.ok_or_else(|| AgentError::invalid_params("this condition needs a `target`"))?;

    let counted = |objs: Vec<glib::Object>| -> (usize, Vec<String>) {
        let refs = objs.iter().take(MAX_MATCHES).map(|o| agent.registry.ref_for(o)).collect();
        (objs.len(), refs)
    };
    match cond {
        WaitCondition::Exists => {
            let (n, matches) = counted(live_matches(agent, target, false)?);
            return Ok(Eval {
                satisfied: n > 0,
                matches,
                observed: json!(n),
                reason: (n == 0).then(|| "no live match".to_string()),
                watch: None,
            });
        }
        WaitCondition::Absent => {
            let (n, matches) = counted(live_matches(agent, target, true)?);
            return Ok(Eval {
                satisfied: n == 0,
                matches,
                observed: json!(n),
                reason: (n > 0).then(|| format!("{n} live match(es) remain")),
                watch: None,
            });
        }
        WaitCondition::MatchCount { count } => {
            let (n, matches) = counted(live_matches(agent, target, false)?);
            return Ok(Eval {
                satisfied: n == *count,
                matches,
                observed: json!(n),
                reason: (n != *count).then(|| format!("{n} matches, waiting for {count}")),
                watch: None,
            });
        }
        _ => {}
    }

    let obj = match single(agent, target)? {
        Single::One(o) => o,
        Single::Pending(reason) => return Ok(Eval::pending(reason)),
    };
    let matches = vec![agent.registry.ref_for(&obj)];
    let (satisfied, observed, reason) = match cond {
        WaitCondition::Property { name, equals } => {
            let ps = obj.find_property(name).ok_or_else(|| {
                AgentError::invalid_params(format!("type {} has no property '{name}'", obj.type_().name()))
            })?;
            if !ps.flags().contains(glib::ParamFlags::READABLE) {
                return Err(AgentError::new(
                    ErrorCode::UnsupportedOperation,
                    format!("property '{name}' of {} is not readable", ps.owner_type().name()),
                ));
            }
            let cur = value_to_json(agent, &obj, &ps, &obj.property_value(ps.name()));
            if secrecy::is_secret(&obj) && secrecy::redact_property(&ps) {
                // Comparing would be an oracle for the secret value.
                (false, secrecy::redacted(), Some(format!("property '{name}' is redacted on this secret object")))
            } else {
                let want = json_to_value_unchecked(agent, &ps, equals)?;
                let want = value_to_json(agent, &obj, &ps, &want);
                let ok = cur == want;
                (ok, cur, (!ok).then(|| format!("property '{name}' differs")))
            }
        }
        WaitCondition::Text { equals, contains } => {
            if equals.is_none() && contains.is_none() {
                return Err(AgentError::invalid_params("text condition needs `equals` and/or `contains`"));
            }
            let w = need_widget(&obj, "a text condition")?;
            if secrecy::is_secret(&w) {
                (false, secrecy::redacted(), Some("secret objects never satisfy text conditions".into()))
            } else {
                match text_of(&w) {
                    None => (false, Value::Null, Some("the widget has no text".into())),
                    Some(t) => {
                        let ok = equals.as_deref().is_none_or(|e| t == e) && contains.as_deref().is_none_or(|c| t.contains(c));
                        (ok, json!(truncated(&t)), (!ok).then(|| "text differs".to_string()))
                    }
                }
            }
        }
        WaitCondition::Mapped { mapped } => {
            let w = need_widget(&obj, "the mapped condition")?;
            let (cur, want) = (w.is_mapped(), mapped.unwrap_or(true));
            (cur == want, json!(cur), (cur != want).then(|| format!("mapped is {cur}")))
        }
        WaitCondition::Sensitive { sensitive } => {
            let w = need_widget(&obj, "the sensitive condition")?;
            let (cur, want) = (w.is_sensitive(), sensitive.unwrap_or(true));
            (cur == want, json!(cur), (cur != want).then(|| format!("sensitive is {cur}")))
        }
        WaitCondition::Exists | WaitCondition::Absent | WaitCondition::MatchCount { .. } | WaitCondition::AppBusy { .. } => {
            unreachable!("handled above")
        }
    };
    Ok(Eval { satisfied, matches, observed, reason, watch: Some(obj) })
}

/// Resolves when `rx` yields or `dur` elapses, whichever is first.
async fn wake_or_sleep(rx: &async_channel::Receiver<()>, dur: Duration) {
    let recv = rx.recv();
    let sleep = glib::timeout_future(dur);
    let mut recv = std::pin::pin!(recv);
    let mut sleep = std::pin::pin!(sleep);
    std::future::poll_fn(|cx| {
        if recv.as_mut().poll(cx).is_ready() || sleep.as_mut().poll(cx).is_ready() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
    .await
}

fn condition_name(c: &WaitCondition) -> &'static str {
    match c {
        WaitCondition::Exists => "exists",
        WaitCondition::Absent => "absent",
        WaitCondition::MatchCount { .. } => "match_count",
        WaitCondition::Property { .. } => "property",
        WaitCondition::Text { .. } => "text",
        WaitCondition::Mapped { .. } => "mapped",
        WaitCondition::Sensitive { .. } => "sensitive",
        WaitCondition::AppBusy { .. } => "app_busy",
    }
}

pub async fn wait_for(agent: &Rc<Agent>, p: WaitForParams, ctx: &Ctx) -> Result<Value, AgentError> {
    let timeout = Duration::from_millis(p.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS).min(MAX_TIMEOUT_MS));
    let limit = match ctx.remaining() {
        Some(r) => timeout.min(r.saturating_sub(DEADLINE_MARGIN)),
        None => timeout,
    };
    let property = matches!(p.condition, WaitCondition::AppBusy { .. }).then_some("is-busy");
    let (tx, rx) = async_channel::bounded::<()>(1);
    let mut watch = Watch::default();
    let start = Instant::now();
    let mut last_progress = start;
    loop {
        ctx.check()?;
        while rx.try_recv().is_ok() {}
        let ev = evaluate(agent, &p.condition, p.target.as_ref())?;
        let elapsed = start.elapsed();
        if ev.satisfied || elapsed >= limit {
            let mut m = json!({
                "satisfied": ev.satisfied,
                "elapsed_ms": elapsed.as_millis() as u64,
                "target": ev.matches.first(),
                "matches": ev.matches,
                "observed": ev.observed,
            });
            if !ev.satisfied {
                m["reason"] = json!(ev.reason);
            }
            return Ok(m);
        }
        watch.update(ev.watch.as_ref(), &tx, property);
        if last_progress.elapsed() >= Duration::from_secs(1) {
            last_progress = Instant::now();
            let msg = format!("waiting for {}: {}", condition_name(&p.condition), ev.reason.as_deref().unwrap_or("not yet"));
            ctx.progress(elapsed.as_millis() as f64, Some(limit.as_millis() as f64), &msg);
        }
        wake_or_sleep(&rx, POLL.min(limit - elapsed)).await;
    }
}
