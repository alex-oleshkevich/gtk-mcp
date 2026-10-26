use gtk_mcp_protocol::*;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::io::Cursor;

fn rt<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(v: T) -> Value {
    let j = serde_json::to_value(&v).unwrap();
    let back: T = serde_json::from_value(j.clone()).unwrap();
    assert_eq!(v, back, "round trip of {j}");
    j
}

fn ack() -> HelloAck {
    HelloAck {
        protocol: PROTOCOL_VERSION,
        agent_version: "0.1.0".into(),
        pid: 42,
        app_id: Some("dev.example.App".into()),
        gtk_version: [4, 24, 1],
        adw_version: Some([1, 10, 0]),
        capabilities: vec![
            Capability::Adwaita,
            Capability::LayerShell,
            Capability::AccessibleId,
            Capability::LogForwarding,
            Capability::InstanceCount,
        ],
        mutations_enabled: true,
    }
}

#[test]
fn constants() {
    assert_eq!(PROTOCOL_VERSION, 1);
    assert_eq!(MAX_FRAME_BYTES, 32 * 1024 * 1024);
    assert_eq!(method::HELLO, "hello");
    assert_eq!(method::CANCEL, "$/cancel");
    assert_eq!(method::EVENT, "$/event");
    assert_eq!(method::LOG, "$/log");
    assert_eq!(method::PROGRESS, "$/progress");
    assert_eq!(method::LATE_RESULT, "$/late_result");
}

#[test]
fn handshake_types() {
    let j = rt(Hello { protocol: 1, broker_version: "0.1.0".into() });
    assert_eq!(j, json!({"protocol": 1, "broker_version": "0.1.0"}));
    let j = rt(ack());
    for k in [
        "protocol", "agent_version", "pid", "app_id", "gtk_version", "adw_version", "capabilities",
        "mutations_enabled",
    ] {
        assert!(j.get(k).is_some(), "HelloAck.{k}");
    }
    assert_eq!(j["capabilities"], json!(["adwaita", "layer_shell", "accessible_id", "log_forwarding", "instance_count"]));
    assert_eq!(rt(RequestMeta { deadline_ms: 900 }), json!({"deadline_ms": 900}));
    assert_eq!(rt(Cancel { id: 7 }), json!({"id": 7}));
    assert_eq!(rt(LateResult { id: 7, result: json!({"x": 1}) }), json!({"id": 7, "result": {"x": 1}}));
}

#[test]
fn error_codes_serialize_snake_case() {
    use ErrorCode::*;
    let all = [
        InvalidParams, NotFound, AmbiguousTarget, StaleReference, Detached, TargetUnmapped, UnsupportedOperation,
        InvalidValue, MutationsDisabled, LaunchDisabled, Redacted, Timeout, NotStarted, TargetUnresponsive,
        CaptureUnavailable, SnapshotUnavailable, SessionExited, VersionMismatch, AgentInactive, Unavailable,
    ];
    assert_eq!(all.len(), 20);
    for c in all {
        let j = serde_json::to_value(c).unwrap();
        assert_eq!(j, json!(c.as_str()));
        assert_eq!(serde_json::from_value::<ErrorCode>(j).unwrap(), c);
    }
    let e = AgentError::new(AmbiguousTarget, "two").with_data(json!({"candidates": ["w1", "w2"]}));
    let j = rt(e.clone());
    assert_eq!(j["code"], "ambiguous_target");
    assert_eq!(j["data"]["candidates"][1], "w2");
    // RPC mapping keeps the typed error.
    let rpc: RpcError = e.clone().into();
    assert_eq!(rpc.into_agent_error(), e);
    assert_eq!(RpcError::from(AgentError::invalid_params("x")).code, RPC_INVALID_PARAMS);
}

#[test]
fn targets_and_selectors() {
    assert_eq!(rt(Target::Ref("w12".into())), json!("w12"));
    let sel = Selector {
        window: Some(WindowSel::Monitor("DP-1".into())),
        automation_id: Some("a".into()),
        buildable_id: Some("b".into()),
        type_name: Some("GtkButton".into()),
        css_class: Some("suggested-action".into()),
        text: Some("Save".into()),
        text_contains: Some("av".into()),
        role: Some("button".into()),
        state: Some(StateFilter {
            visible: Some(true), mapped: Some(true), sensitive: Some(true), has_focus: Some(false),
            expanded: Some(false), active: Some(true), checked: Some(false), selected: Some(true),
        }),
        ancestor: Some(Box::new(Selector { type_name: Some("GtkBox".into()), ..Default::default() })),
        include_detached: Some(true),
        nth: Some(1),
    };
    let j = rt(Target::Select(sel));
    for k in [
        "window", "automation_id", "buildable_id", "type_name", "css_class", "text", "text_contains", "role", "state",
        "ancestor", "include_detached", "nth",
    ] {
        assert!(j.get(k).is_some(), "Selector.{k}");
    }
    for k in ["visible", "mapped", "sensitive", "has_focus", "expanded", "active", "checked", "selected"] {
        assert!(j["state"].get(k).is_some(), "StateFilter.{k}");
    }
    // A selector object is not mistaken for a ref, and typos are rejected.
    assert!(matches!(serde_json::from_value::<Target>(json!({"text": "Save"})).unwrap(), Target::Select(_)));
    assert!(serde_json::from_value::<Target>(json!({"txet": "Save"})).is_err());
    for w in [
        WindowSel::Ref("w1".into()), WindowSel::Title("T".into()), WindowSel::Monitor("DP-1".into()),
        WindowSel::AppWindowId(3), WindowSel::LayerNamespace("panel".into()),
    ] {
        rt(w);
    }
    assert_eq!(serde_json::to_value(WindowSel::AppWindowId(3)).unwrap(), json!({"app_window_id": 3}));
    assert_eq!(serde_json::to_value(WindowSel::LayerNamespace("p".into())).unwrap(), json!({"layer_namespace": "p"}));
}

#[test]
fn interact() {
    let ops = vec![
        InteractOp::Activate,
        InteractOp::Focus,
        InteractOp::Fill { text: "x".into() },
        InteractOp::SetChecked { checked: true },
        InteractOp::SetActive { active: false },
        InteractOp::SetValue { value: 0.5 },
        InteractOp::SetExpanded { expanded: true },
        InteractOp::Select { index: 2 },
        InteractOp::SelectPage { name: "p".into() },
        InteractOp::Popup,
        InteractOp::Popdown,
        InteractOp::ScrollTo,
        InteractOp::ScrollBy { dx: 1.0, dy: -2.0 },
        InteractOp::Close,
        InteractOp::Play,
        InteractOp::Pause,
        InteractOp::Seek { microseconds: -5 },
    ];
    for op in ops {
        let p = InteractParams {
            session: "s1".into(),
            target: Target::Ref("w8".into()),
            op: op.clone(),
            observe: Some(Observe { snapshot_depth: Some(2), capture: Some(true) }),
        };
        let j = rt(p);
        assert_eq!(j["operation"], op.name());
        assert_eq!(j["session"], "s1");
    }
    let j = json!({"session": "s", "target": "w8", "operation": "set_expanded", "expanded": true});
    let p: InteractParams = serde_json::from_value(j).unwrap();
    assert_eq!(p.op, InteractOp::SetExpanded { expanded: true });
    assert!(serde_json::from_value::<InteractParams>(json!({"session": "s", "target": "w8", "operation": "zap"})).is_err());

    rt(InteractResult {
        target: "w8".into(),
        observed: json!({"expanded": true}),
        observation: Some(json!({})),
        observation_error: Some(AgentError::new(ErrorCode::Timeout, "t")),
    });
    rt(InteractResult { target: "w8".into(), observed: json!({}), observation: None, observation_error: None });
}

#[test]
fn batch_capture_session() {
    rt(BatchStep { tool: "interact".into(), arguments: json!({"target": "$1.ref"}) });
    rt(BatchParams {
        session: "s".into(),
        steps: vec![BatchStep { tool: "wait_for".into(), arguments: json!({}) }],
        stop_on_error: Some(false),
    });
    rt(CaptureParams {
        session: "s".into(), target: Target::Ref("w1".into()), settle: Some(true), settle_timeout_ms: Some(1000),
        annotate: Some(true), scale: Some(2.0),
    });
    let res = CaptureResult {
        capture_id: "cap-1".into(), target: "w1".into(), width: 10, height: 20, scale: 1.0, stale: true,
        stale_reason: Some("no_frame".into()),
        annotations: vec![Annotation { r#ref: "w2".into(), x: 1.0, y: 2.0, width: 3.0, height: 4.0 }],
    };
    let j = rt(res.clone());
    assert_eq!(j["annotations"][0]["ref"], "w2");
    let resp = CaptureResponse { result: res, png_base64: "AAAA".into() };
    let j = rt_nopartial(&resp);
    assert_eq!(j["capture_id"], "cap-1");
    assert_eq!(j["png_base64"], "AAAA");

    rt(SessionState::Running);
    rt(SessionState::Incompatible);
    let j = rt(SessionState::Exited { code: None, signal: Some(11), in_flight: Some(9) });
    assert_eq!(j, json!({"exited": {"code": null, "signal": 11, "in_flight": 9}}));
    rt(SessionInfo {
        session: "s1".into(), pid: 1, app_id: None, launched_by_broker: false, state: SessionState::Running,
        hello: Some(ack()),
    });
}

fn rt_nopartial<T: Serialize + DeserializeOwned>(v: &T) -> Value {
    let j = serde_json::to_value(v).unwrap();
    let back: T = serde_json::from_value(j.clone()).unwrap();
    assert_eq!(serde_json::to_value(&back).unwrap(), j);
    j
}

#[test]
fn tool_params_round_trip() {
    let j = json!({"session": "s", "target": {"automation_id": "x"}, "condition": "mapped"});
    let p: WaitForParams = serde_json::from_value(j).unwrap();
    assert_eq!(p.condition, WaitCondition::Mapped { mapped: None });
    let j = json!({"session": "s", "target": "w1", "condition": "property", "name": "label", "equals": "Hi"});
    rt_nopartial(&serde_json::from_value::<WaitForParams>(j).unwrap());
    let j = json!({"session": "s", "target": "w1", "operation": "replace", "start": 0, "end": 1, "text": "x"});
    rt_nopartial(&serde_json::from_value::<EditTextParams>(j).unwrap());
    // validate_step covers shape, nesting and lifecycle/broker-only tools
    assert!(validate_step("interact", &json!({"target": "w1", "operation": "activate"})).is_ok());
    assert!(validate_step("batch", &json!({"steps": []})).unwrap_err().contains("nested"));
    assert!(validate_step("diff_snapshots", &json!({"from": "a", "to": "b"})).is_err());
    assert!(validate_step("nope", &json!({})).unwrap_err().contains("unknown"));
    assert!(validate_step("interact", &json!({"target": "w1"})).is_err());
}

#[test]
fn rpc_messages() {
    let req = Request::new(5, "snapshot", json!({"session": "s", "view": "full"}), Some(1500));
    assert_eq!(req.meta(), Some(RequestMeta { deadline_ms: 1500 }));
    let p: SnapshotParams = req.parse_params().unwrap(); // _meta is ignored
    assert_eq!(p.view, Some(SnapshotView::Full));
    let mut buf = Vec::new();
    write_frame(&mut buf, &req).unwrap();
    assert_eq!(*buf.last().unwrap(), b'\n');
    match parse_message(&buf).unwrap() {
        Message::Request(r) => assert_eq!(r, req),
        m => panic!("{m:?}"),
    }
    let n = Notification::new(method::EVENT, json!({"k": 1}));
    assert!(matches!(parse_message(&serde_json::to_vec(&n).unwrap()).unwrap(), Message::Notification(_)));
    let ok = Response::ok(5, json!({"a": 1}));
    assert!(matches!(parse_message(&serde_json::to_vec(&ok).unwrap()).unwrap(), Message::Response(_)));
    let er = Response::err(Some(5), AgentError::new(ErrorCode::NotFound, "nf"));
    match parse_message(&serde_json::to_vec(&er).unwrap()).unwrap() {
        Message::Response(r) => assert_eq!(r.error.unwrap().into_agent_error().code, ErrorCode::NotFound),
        m => panic!("{m:?}"),
    }
    // malformed frames keep the recoverable id
    let (id, _) = parse_message(br#"{"jsonrpc":"2.0","id":9,"method":5}"#).unwrap_err();
    assert_eq!(id, Some(9));
    assert!(parse_message(b"not json").is_err());
    assert!(parse_message(br#"{"id":1,"method":"x"}"#).is_err());
}

#[test]
fn frames_are_split_and_capped() {
    let data = b"{\"a\":1}\n\n{\"b\":2}\nunterminated";
    let mut r = FrameReader::new(Cursor::new(&data[..]));
    assert_eq!(r.next_frame().unwrap(), Some(Frame::Line(b"{\"a\":1}".to_vec())));
    assert_eq!(r.next_frame().unwrap(), Some(Frame::Line(b"{\"b\":2}".to_vec())));
    assert_eq!(r.next_frame().unwrap(), Some(Frame::Line(b"unterminated".to_vec())));
    assert_eq!(r.next_frame().unwrap(), None);

    // Oversized line is reported, discarded, and the next frame still parses.
    let mut big = vec![b'x'; 100];
    big.push(b'\n');
    big.extend_from_slice(b"{\"ok\":true}\n");
    let mut r = FrameReader::with_limit(Cursor::new(big), 50);
    assert_eq!(r.next_frame().unwrap(), Some(Frame::Oversized { len: 100 }));
    assert_eq!(r.next_frame().unwrap(), Some(Frame::Line(b"{\"ok\":true}".to_vec())));
    // exactly at the cap is accepted
    let mut exact = vec![b'y'; 50];
    exact.push(b'\n');
    let mut r = FrameReader::with_limit(Cursor::new(exact), 50);
    assert!(matches!(r.next_frame().unwrap(), Some(Frame::Line(l)) if l.len() == 50));
    // oversized unterminated tail
    let mut r = FrameReader::with_limit(Cursor::new(vec![b'z'; 60]), 50);
    assert_eq!(r.next_frame().unwrap(), Some(Frame::Oversized { len: 60 }));
}

#[test]
fn real_cap_rejects_oversized_frames() {
    let mut data = vec![b'a'; MAX_FRAME_BYTES + 1];
    data.push(b'\n');
    data.extend_from_slice(b"{}\n");
    let mut r = FrameReader::new(Cursor::new(data));
    assert_eq!(r.next_frame().unwrap(), Some(Frame::Oversized { len: MAX_FRAME_BYTES + 1 }));
    assert!(matches!(r.next_frame().unwrap(), Some(Frame::Line(_))));
    // writing refuses too
    let huge = json!({"s": "a".repeat(MAX_FRAME_BYTES)});
    let mut sink = Vec::new();
    assert!(write_frame(&mut sink, &huge).is_err());
    assert!(sink.is_empty());
}

#[test]
fn snapshot_text_matches_json_hierarchy() {
    let snap = json!({
        "roots": [{
            "ref": "w1", "type": "GtkWindow", "title": "Preferences", "state": {"mapped": true, "visible": true, "sensitive": true},
            "bounds_in_root": {"x": 0, "y": 0, "width": 480, "height": 640},
            "children": [{
                "ref": "w3", "type": "GtkBox", "state": {"mapped": true}, "children": [
                    {"ref": "w8", "type": "GtkExpander", "label": "Advanced options", "state": {"expanded": false, "mapped": true}, "children": [
                        {"ref": "w9", "type": "GtkEntry", "attached": false, "via": "GtkExpander:child", "text": "localhost", "state": {"mapped": false}, "children": []}]},
                    {"ref": "w11", "type": "GtkPasswordEntry", "secret": true, "text_redacted": true, "children": []},
                    {"ref": "w13", "type": "GtkMenuButton", "children": [
                        {"ref": "w14", "type": "GtkPopover", "native": true, "children_collapsed": true, "child_count": 3, "children": []}]},
                    {"ref": "w12", "type": "GtkButton", "label": "Save", "state": {"sensitive": false}, "children": []}
                ]}]
        }],
        "node_count": 9, "complete": true, "truncated": false
    });
    let t = snapshot::render_text(&snap, 128);
    assert!(t.contains("GtkWindow \"Preferences\" [w1] 480x640 mapped"), "{t}");
    assert!(t.contains("GtkExpander \"Advanced options\" [w8] expanded=false"), "{t}");
    assert!(t.contains("GtkEntry [w9] detached via=GtkExpander:child mapped=false text=\"localhost\""), "{t}");
    assert!(t.contains("GtkPasswordEntry [w11] secret text=<redacted>"), "{t}");
    assert!(t.contains("GtkPopover [w14] native closed (3 children)"), "{t}");
    assert!(t.contains("└── GtkButton \"Save\" [w12] sensitive=false"), "{t}");
    // every ref in the JSON appears exactly once in the text
    for r in ["w1", "w3", "w8", "w9", "w11", "w12", "w13", "w14"] {
        assert_eq!(t.matches(&format!("[{r}]")).count(), 1, "{r}");
    }
    assert!(!t.contains("localhostx"));
}
