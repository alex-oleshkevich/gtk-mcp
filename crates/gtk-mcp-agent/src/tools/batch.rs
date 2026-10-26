//! `batch` (plan 5.4): validate every step before the first dispatch, then run
//! the steps in order with one main-loop iteration between them. No rollback.
//!
//! `$N.ref` (1-based, N = the step's position in `steps`) is replaced, when it
//! is a whole string value in a later step's arguments, by the target ref of
//! step N's result: its `target` string, else its `ref`, else the single entry
//! of `matches`. Targets are resolved at step time, never at validation time.

use crate::agent::{Agent, Ctx, run_tool};
use gtk_mcp_protocol::{
    AgentError, BatchParams, BatchResult, BatchStepResult, DEFAULT_SETTLE_TIMEOUT_MS, ErrorCode, MAX_SETTLE_TIMEOUT_MS,
    tool_spec, validate_step,
};
use serde_json::{Value, json};
use std::rc::Rc;
use std::time::{Duration, Instant};

pub const MAX_STEPS: usize = 100;
const DEFAULT_STEP_MS: u64 = 10_000;

/// Yield exactly one main-loop iteration (an idle callback) to the caller.
pub async fn yield_one_iteration() {
    let (tx, rx) = async_channel::bounded::<()>(1);
    glib::idle_add_local_once(move || {
        let _ = tx.try_send(());
    });
    let _ = rx.recv().await;
}

/// The ref a later step may refer to as `$N.ref`.
pub fn result_ref(result: &Value) -> Option<String> {
    if let Some(s) = result.get("target").and_then(Value::as_str) {
        return Some(s.to_string());
    }
    if let Some(s) = result.get("ref").and_then(Value::as_str) {
        return Some(s.to_string());
    }
    match result.get("matches").and_then(Value::as_array) {
        Some(m) if m.len() == 1 => m[0].get("ref").and_then(Value::as_str).map(String::from),
        _ => None,
    }
}

/// Parse a whole-string `$N.ref` placeholder.
fn placeholder(s: &str) -> Option<usize> {
    let rest = s.strip_prefix('$')?.strip_suffix(".ref")?;
    if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    rest.parse().ok()
}

fn visit_placeholders(v: &Value, f: &mut dyn FnMut(usize)) {
    match v {
        Value::String(s) => {
            if let Some(n) = placeholder(s) {
                f(n);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| visit_placeholders(x, f)),
        Value::Object(o) => o.values().for_each(|x| visit_placeholders(x, f)),
        _ => {}
    }
}

fn substitute(v: &mut Value, refs: &[Option<String>], step: usize) -> Result<(), AgentError> {
    match v {
        Value::String(s) => {
            if let Some(n) = placeholder(s) {
                let r = refs.get(n - 1).cloned().flatten().ok_or_else(|| {
                    AgentError::new(
                        ErrorCode::InvalidParams,
                        format!("step {step}: ${n}.ref is unavailable: step {n} produced no target ref"),
                    )
                })?;
                *s = r;
            }
            Ok(())
        }
        Value::Array(a) => a.iter_mut().try_for_each(|x| substitute(x, refs, step)),
        Value::Object(o) => o.values_mut().try_for_each(|x| substitute(x, refs, step)),
        _ => Ok(()),
    }
}

fn step_budget(tool: &str, args: &Value) -> Duration {
    let ms = match tool {
        "wait_for" => args.get("timeout_ms").and_then(Value::as_u64).unwrap_or(5_000).min(60_000) + 2_000,
        "capture" => {
            args.get("settle_timeout_ms").and_then(Value::as_u64).unwrap_or(DEFAULT_SETTLE_TIMEOUT_MS).min(MAX_SETTLE_TIMEOUT_MS)
                + 5_000
        }
        _ => DEFAULT_STEP_MS,
    };
    Duration::from_millis(ms)
}

/// Validation shared by the pre-dispatch pass: tool names, params, policy,
/// limits, nesting, placeholders. Nothing has executed when this fails.
pub fn validate(agent: &Agent, p: &BatchParams) -> Result<(), AgentError> {
    if p.steps.is_empty() {
        return Err(AgentError::invalid_params("batch needs at least one step"));
    }
    if p.steps.len() > MAX_STEPS {
        return Err(AgentError::invalid_params(format!("batch has {} steps; the maximum is {MAX_STEPS}", p.steps.len())));
    }
    for (i, s) in p.steps.iter().enumerate() {
        let n = i + 1;
        let spec = validate_step(&s.tool, &s.arguments)
            .map_err(|e| AgentError::invalid_params(format!("step {n}: {e}")).with_data(json!({"step": n})))?;
        if spec.mutating && !agent.config.mutations {
            return Err(AgentError::new(
                ErrorCode::MutationsDisabled,
                format!("step {n} ({}) is mutating and the app was started without GTK_MCP_MUTATIONS=1", s.tool),
            )
            .with_data(json!({"step": n})));
        }
        let mut bad = None;
        visit_placeholders(&s.arguments, &mut |k| {
            if k == 0 || k >= n {
                bad = Some(k);
            }
        });
        if let Some(k) = bad {
            return Err(AgentError::invalid_params(format!(
                "step {n}: ${k}.ref must refer to an earlier step (steps are numbered from 1)"
            ))
            .with_data(json!({"step": n})));
        }
        debug_assert!(tool_spec(&s.tool).is_some());
    }
    Ok(())
}

pub async fn batch(agent: &Rc<Agent>, p: BatchParams, ctx: &Ctx) -> Result<Value, AgentError> {
    validate(agent, &p)?;
    let stop_on_error = p.stop_on_error.unwrap_or(true);
    let mut results: Vec<BatchStepResult> = Vec::with_capacity(p.steps.len());
    let mut refs: Vec<Option<String>> = Vec::with_capacity(p.steps.len());
    let mut stopped_at = None;

    let mut cancelled = false;
    for (i, step) in p.steps.iter().enumerate() {
        // One main-loop iteration between steps, so each step sees the effects
        // (relayout, notifications, idle work) of the previous one.
        if i > 0 {
            yield_one_iteration().await;
        }
        // Cancelled mid-batch: stop, and return what already ran so a mutation
        // that happened is reported (as a late result), never lost.
        if ctx.is_cancelled() {
            cancelled = true;
            stopped_at = Some(i);
            break;
        }
        let mut args = step.arguments.clone();
        if args.is_null() {
            args = json!({});
        }
        let outcome = match substitute(&mut args, &refs, i + 1) {
            Err(e) => Err(e),
            Ok(()) => {
                let step_deadline = Instant::now() + step_budget(&step.tool, &args);
                let deadline = ctx.deadline.map_or(step_deadline, |d| d.min(step_deadline));
                let sub = ctx.with_deadline(Some(deadline));
                run_tool(agent, &step.tool, args, &sub).await
            }
        };
        match outcome {
            Ok(v) => {
                refs.push(result_ref(&v));
                results.push(BatchStepResult { tool: step.tool.clone(), ok: true, result: Some(v), error: None });
            }
            Err(e) => {
                refs.push(None);
                results.push(BatchStepResult { tool: step.tool.clone(), ok: false, result: None, error: Some(e) });
                if stop_on_error {
                    stopped_at = Some(i);
                    break;
                }
            }
        }
    }
    Ok(serde_json::to_value(BatchResult { steps: results, stopped_at, cancelled }).expect("BatchResult serializes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_budget_is_bounded_for_huge_timeouts() {
        let cap = step_budget("capture", &json!({"settle_timeout_ms": u64::MAX}));
        assert_eq!(cap, Duration::from_millis(MAX_SETTLE_TIMEOUT_MS + 5_000));
        assert_eq!(step_budget("capture", &json!({})), Duration::from_millis(DEFAULT_SETTLE_TIMEOUT_MS + 5_000));
        assert_eq!(step_budget("wait_for", &json!({"timeout_ms": u64::MAX})), Duration::from_millis(62_000));
    }
}
