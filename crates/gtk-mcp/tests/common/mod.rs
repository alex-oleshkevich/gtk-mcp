//! Scriptable mock agent: a thread listening on `<dir>/<pid>.sock`, speaking
//! the NDJSON protocol through the protocol crate's framing.
#![allow(dead_code)]

use gtk_mcp::{Broker, Config, Policy};
use gtk_mcp_protocol::*;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::BufReader;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub enum Step {
    /// `$/progress {id, state: "started"}`
    Started,
    Sleep(u64),
    /// `$/progress {id, progress, total}`
    Progress(f64, Option<f64>),
    /// `$/event` notification with these params.
    Event(Value),
    /// `$/late_result {id, result}`
    LateResult(Value),
    Respond(Value),
    RespondFn(Arc<dyn Fn(&Request) -> Value + Send + Sync>),
    Error(AgentError),
    /// Close the connection.
    Close,
}

pub fn script_respond(v: Value) -> Vec<Step> {
    vec![Step::Started, Step::Respond(v)]
}

#[derive(Default)]
struct Shared {
    received: Mutex<Vec<Value>>,
    writers: Mutex<Vec<Arc<Mutex<UnixStream>>>>,
    scripts: Mutex<HashMap<String, Vec<Step>>>,
}

pub struct MockAgent {
    pub pid: u32,
    pub path: PathBuf,
    shared: Arc<Shared>,
    child: Child,
}

pub struct MockBuilder<'a> {
    dir: &'a Path,
    ack: Box<dyn Fn(u32) -> HelloAck>,
    scripts: HashMap<String, Vec<Step>>,
    hello_hang: bool,
}

pub fn default_ack(pid: u32) -> HelloAck {
    HelloAck {
        protocol: PROTOCOL_VERSION,
        agent_version: "0.0.0-mock".into(),
        pid,
        app_id: Some("org.example.Mock".into()),
        gtk_version: [4, 22, 1],
        adw_version: Some([1, 9, 0]),
        capabilities: vec![Capability::Adwaita, Capability::AccessibleId],
        mutations_enabled: false,
    }
}

impl<'a> MockBuilder<'a> {
    pub fn ack(mut self, f: impl Fn(&mut HelloAck) + 'static) -> Self {
        self.ack = Box::new(move |pid| {
            let mut a = default_ack(pid);
            f(&mut a);
            a
        });
        self
    }
    pub fn mutations(self, on: bool) -> Self {
        self.ack(move |a| a.mutations_enabled = on)
    }
    pub fn script(mut self, method: &str, steps: Vec<Step>) -> Self {
        self.scripts.insert(method.into(), steps);
        self
    }
    /// Answer `method` immediately with `result` (after signalling `started`).
    pub fn respond(self, method: &str, result: Value) -> Self {
        self.script(method, script_respond(result))
    }
    /// Never answer `hello`.
    pub fn hang_on_hello(mut self) -> Self {
        self.hello_hang = true;
        self
    }
    pub fn start(self) -> MockAgent {
        let child = Command::new("sleep").arg("600").stdout(Stdio::null()).spawn().expect("spawn sleep");
        let pid = child.id();
        std::fs::create_dir_all(self.dir).unwrap();
        let path = self.dir.join(format!("{pid}.sock"));
        let listener = UnixListener::bind(&path).unwrap();
        let shared = Arc::new(Shared::default());
        *shared.scripts.lock().unwrap() = self.scripts;
        let ack = (self.ack)(pid);
        let hang = self.hello_hang;
        let sh = shared.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { break };
                let sh = sh.clone();
                let ack = ack.clone();
                std::thread::spawn(move || serve(conn, sh, ack, hang));
            }
        });
        MockAgent { pid, path, shared, child }
    }
}

fn write(w: &Arc<Mutex<UnixStream>>, msg: &impl serde::Serialize) {
    let g = w.lock().unwrap();
    let _ = write_frame(&mut &*g, msg);
}

fn serve(conn: UnixStream, sh: Arc<Shared>, ack: HelloAck, hang: bool) {
    let writer = Arc::new(Mutex::new(conn.try_clone().unwrap()));
    sh.writers.lock().unwrap().push(writer.clone());
    let mut reader = FrameReader::new(BufReader::new(conn));
    while let Ok(Some(Frame::Line(line))) = reader.next_frame() {
        match parse_message(&line) {
            Ok(Message::Request(req)) if req.method == method::HELLO => {
                if !hang {
                    write(&writer, &Response::ok(req.id, serde_json::to_value(&ack).unwrap()));
                }
            }
            Ok(Message::Request(req)) => {
                sh.received.lock().unwrap().push(serde_json::to_value(&req).unwrap());
                let steps = sh
                    .scripts
                    .lock()
                    .unwrap()
                    .get(&req.method)
                    .cloned()
                    .unwrap_or_else(|| vec![Step::Respond(json!({ "echo": req.method }))]);
                let w = writer.clone();
                std::thread::spawn(move || run_script(w, req, steps));
            }
            Ok(Message::Notification(n)) => {
                sh.received.lock().unwrap().push(serde_json::to_value(&n).unwrap());
            }
            _ => {}
        }
    }
}

fn run_script(w: Arc<Mutex<UnixStream>>, req: Request, steps: Vec<Step>) {
    for s in steps {
        match s {
            Step::Started => write(&w, &Notification::new(method::PROGRESS, json!({ "id": req.id, "state": "started" }))),
            Step::Sleep(ms) => std::thread::sleep(Duration::from_millis(ms)),
            Step::Progress(p, t) => {
                write(&w, &Notification::new(method::PROGRESS, json!({ "id": req.id, "progress": p, "total": t, "message": "working" })))
            }
            Step::Event(v) => write(&w, &Notification::new(method::EVENT, v)),
            Step::LateResult(v) => write(&w, &Notification::new(method::LATE_RESULT, json!({ "id": req.id, "result": v }))),
            Step::Respond(v) => write(&w, &Response::ok(req.id, v)),
            Step::RespondFn(f) => write(&w, &Response::ok(req.id, f(&req))),
            Step::Error(e) => write(&w, &Response::err(Some(req.id), e)),
            Step::Close => {
                let _ = w.lock().unwrap().shutdown(std::net::Shutdown::Both);
            }
        }
    }
}

impl MockAgent {
    pub fn builder(dir: &Path) -> MockBuilder<'_> {
        MockBuilder { dir, ack: Box::new(default_ack), scripts: HashMap::new(), hello_hang: false }
    }
    pub fn start(dir: &Path) -> MockAgent {
        Self::builder(dir).start()
    }

    /// Requests received after the hello: `(id, method, params)`.
    pub fn requests(&self) -> Vec<(u64, String, Value)> {
        self.shared
            .received
            .lock()
            .unwrap()
            .iter()
            .filter(|v| v.get("id").is_some())
            .map(|v| (v["id"].as_u64().unwrap(), v["method"].as_str().unwrap().to_owned(), v["params"].clone()))
            .collect()
    }
    pub fn cancels(&self) -> Vec<u64> {
        self.shared
            .received
            .lock()
            .unwrap()
            .iter()
            .filter(|v| v["method"] == method::CANCEL)
            .map(|v| v["params"]["id"].as_u64().unwrap())
            .collect()
    }
    pub fn wait_cancel(&self, timeout: Duration) -> Option<u64> {
        let t = Instant::now();
        while t.elapsed() < timeout {
            if let Some(id) = self.cancels().first() {
                return Some(*id);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }
    /// Send a notification on every open connection.
    pub fn push(&self, method: &str, params: Value) {
        for w in self.shared.writers.lock().unwrap().iter() {
            write(w, &Notification::new(method, params.clone()));
        }
    }
    pub fn close_connections(&self) {
        for w in self.shared.writers.lock().unwrap().drain(..) {
            let _ = w.lock().unwrap().shutdown(std::net::Shutdown::Both);
        }
    }
}

impl Drop for MockAgent {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn broker_with(dir: &Path, policy: Policy, tweak: impl FnOnce(&mut Config)) -> Arc<Broker> {
    let mut cfg = Config::with_socket_dir(dir);
    tweak(&mut cfg);
    Broker::new(policy, cfg).unwrap()
}

pub fn allow_all() -> Policy {
    Policy { allow_mutations: true, ..Policy::default() }
}

/// Poll `f` until it returns true (max ~3 s).
pub fn wait_until(mut f: impl FnMut() -> bool) -> bool {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(3) {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

pub fn png(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
    let mut img = image::RgbaImage::new(w, h);
    for (x, y, p) in img.enumerate_pixels_mut() {
        *p = image::Rgba(f(x, y));
    }
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
    out
}
