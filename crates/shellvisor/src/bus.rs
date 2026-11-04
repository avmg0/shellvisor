//! The agent message bus: a private unix socket that `sv agent …` (from any
//! shell or agent inside Shellvisor) uses to ask the app for things that need
//! an answer — spawn (which may be refused), wait, tree, logs.
//!
//! One JSON object per connection each way: the request, then the reply.
//! The socket lives in the state folder (mode 0700), exists only while
//! Shellvisor runs, and its path reaches child shells as `$SHELLVISOR_SOCK`.
//! Nothing listens on the network.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::time::Duration;

pub const ENV_SOCK: &str = "SHELLVISOR_SOCK";
/// Set in an agent's shell: which agent is calling.
pub const ENV_AGENT: &str = "SHELLVISOR_AGENT";
/// Set in an agent's shell: the task it was started with.
pub const ENV_TASK: &str = "SHELLVISOR_TASK";

/// The socket path for this Shellvisor process.
pub fn socket_path(state: &Path) -> PathBuf {
    state
        .join("run")
        .join(format!("bus-{}.sock", std::process::id()))
}

/// Start listening. Every request is handed to `deliver` with a sender for
/// the reply; the app may answer at once or later (`wait`).
pub fn listen(
    path: &Path,
    deliver: impl Fn(Value, Sender<Value>) + Send + Sync + 'static,
) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    let deliver = std::sync::Arc::new(deliver);
    std::thread::Builder::new()
        .name("bus".into())
        .stack_size(128 * 1024)
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let deliver = deliver.clone();
                let _ = std::thread::Builder::new()
                    .name("bus-req".into())
                    .stack_size(128 * 1024)
                    .spawn(move || serve(stream, &*deliver));
            }
        })?;
    Ok(())
}

fn serve(stream: UnixStream, deliver: &(impl Fn(Value, Sender<Value>) + ?Sized)) {
    let mut line = String::new();
    let mut reader = BufReader::new(&stream);
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let reply = match serde_json::from_str::<Value>(&line) {
        Ok(req) => {
            let (tx, rx) = mpsc::channel();
            deliver(req, tx);
            // `wait` can take a long time; the client decides when to give up.
            rx.recv()
                .unwrap_or_else(|_| json!({"ok": false, "error": "Shellvisor is closing"}))
        }
        Err(e) => json!({"ok": false, "error": format!("bad request: {e}")}),
    };
    let mut w = &stream;
    let _ = w.write_all(format!("{reply}\n").as_bytes());
}

/// Send one request from `sv` and wait for the reply.
pub fn call(mut req: Value, timeout: Option<Duration>) -> Result<Value, String> {
    let path = std::env::var(ENV_SOCK).map_err(|_| {
        "Shellvisor's agent bus isn't reachable from here (no $SHELLVISOR_SOCK)".to_string()
    })?;
    if let (Some(obj), Ok(me)) = (req.as_object_mut(), std::env::var(ENV_AGENT)) {
        obj.entry("caller").or_insert(Value::String(me));
    }
    let stream = UnixStream::connect(&path).map_err(|e| format!("can't reach Shellvisor ({e})"))?;
    stream
        .set_read_timeout(timeout)
        .map_err(|e| e.to_string())?;
    let mut w = &stream;
    w.write_all(format!("{req}\n").as_bytes())
        .map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line).map_err(|e| {
        if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut {
            "timed out".to_string()
        } else {
            e.to_string()
        }
    })?;
    let v: Value = serde_json::from_str(&line).map_err(|e| format!("bad reply: {e}"))?;
    if v.get("ok") == Some(&Value::Bool(false)) {
        return Err(v
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("refused")
            .to_string());
    }
    Ok(v)
}

pub fn ok(extra: Value) -> Value {
    let mut v = json!({"ok": true});
    if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
        for (k, val) in e {
            o.insert(k.clone(), val.clone());
        }
    }
    v
}

pub fn err(msg: impl Into<String>) -> Value {
    json!({"ok": false, "error": msg.into()})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_over_the_socket() {
        let dir = std::env::temp_dir().join(format!("sv-bus-{}", std::process::id()));
        let path = dir.join("run/bus.sock");
        listen(&path, |req, reply| {
            let op = req["op"].as_str().unwrap_or("").to_string();
            let caller = req["caller"].as_str().unwrap_or("").to_string();
            if op == "later" {
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(100));
                    let _ = reply.send(ok(json!({"done": true})));
                });
            } else if op == "no" {
                let _ = reply.send(err("refused: too deep"));
            } else {
                let _ = reply.send(ok(json!({"echo": op, "caller": caller})));
            }
        })
        .unwrap();
        unsafe {
            std::env::set_var(ENV_SOCK, &path);
            std::env::set_var(ENV_AGENT, "api");
        }
        let v = call(json!({"op": "hi"}), Some(Duration::from_secs(2))).unwrap();
        assert_eq!(v["echo"], "hi");
        assert_eq!(v["caller"], "api");
        assert_eq!(call(json!({"op": "later"}), None).unwrap()["done"], true);
        assert_eq!(
            call(json!({"op": "no"}), None).unwrap_err(),
            "refused: too deep"
        );
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
        let _ = std::fs::remove_dir_all(dir);
    }
}
