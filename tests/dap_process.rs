use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};

#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::net::{TcpListener, TcpStream};
#[cfg(unix)]
use std::path::Path;
#[cfg(unix)]
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};
#[cfg(unix)]
use std::thread;
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(unix)]
static RDBG_PROCESS_TEST_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn binary_serves_initialize_over_dap_stdio() {
    let request = r#"{"seq":1,"type":"request","command":"initialize","arguments":{}}"#;
    let input = format!("Content-Length: {}\r\n\r\n{request}", request.len());
    let mut child = Command::new(env!("CARGO_BIN_EXE_onec-debug-adapter"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();

    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();

    assert!(output.status.success());
    assert!(stdout.starts_with("Content-Length: "));
    assert!(stdout.contains("\"command\":\"initialize\""));
    assert!(stdout.contains("\"supportsConfigurationDoneRequest\":true"));
    assert!(stdout.contains("\"supportsSingleThreadExecutionRequests\":true"));
    assert!(stdout.contains("\"filter\":\"all\""));
}

#[cfg(unix)]
#[test]
fn adapter_detaches_debug_ui_when_dap_input_closes() {
    let _lock = RDBG_PROCESS_TEST_LOCK.lock().unwrap();
    let rdbg = FakeRdbgServer::start();
    let mut child = Command::new(env!("CARGO_BIN_EXE_onec-debug-adapter"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    send_dap_request(
        &mut stdin,
        r#"{"seq":1,"type":"request","command":"initialize","arguments":{}}"#,
    );
    assert!(read_dap_message(&mut stdout).contains("\"command\":\"initialize\""));
    send_dap_request(
        &mut stdin,
        &format!(
            r#"{{
                "seq":2,
                "type":"request",
                "command":"attach",
                "arguments":{{
                    "infoBaseAlias":"Probe",
                    "debugServerHost":"127.0.0.1",
                    "debugServerPort":{}
                }}
            }}"#,
            rdbg.port
        ),
    );
    assert!(read_dap_message(&mut stdout).contains("\"command\":\"attach\""));
    assert!(read_dap_message(&mut stdout).contains("\"event\":\"initialized\""));
    drop(stdin);
    assert!(child.wait().unwrap().success());

    let requests = rdbg.requests.lock().unwrap().clone();
    assert!(
        requests
            .iter()
            .any(|request| request.contains("cmd=attachDebugUI"))
    );
    assert!(
        requests
            .iter()
            .any(|request| request.contains("cmd=detachDebugUI"))
    );
    rdbg.shutdown();
}

#[cfg(unix)]
#[test]
fn pending_rdbg_poll_does_not_delay_step_in_request() {
    let _lock = RDBG_PROCESS_TEST_LOCK.lock().unwrap();
    let rdbg = FakeRdbgServer::start_with_held_pings(true);
    let trace_file =
        std::env::temp_dir().join(format!("onec-latency-{}.jsonl", uuid::Uuid::new_v4()));
    let trace_path = trace_file.to_string_lossy();
    let mut child = Command::new(env!("CARGO_BIN_EXE_onec-debug-adapter"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    send_dap_request(
        &mut stdin,
        r#"{"seq":1,"type":"request","command":"initialize","arguments":{}}"#,
    );
    assert!(read_dap_message(&mut stdout).contains("\"command\":\"initialize\""));
    send_dap_request(
        &mut stdin,
        &format!(
            r#"{{"seq":2,"type":"request","command":"attach","arguments":{{"infoBaseAlias":"Probe","debugServerHost":"127.0.0.1","debugServerPort":{},"trace":true,"traceFile":{:?}}}}}"#,
            rdbg.port, trace_path
        ),
    );
    assert!(read_dap_message(&mut stdout).contains("\"command\":\"attach\""));
    assert!(read_dap_message(&mut stdout).contains("\"event\":\"initialized\""));

    // Establish a DAP thread before holding the next RDBG ping indefinitely.
    send_dap_request(
        &mut stdin,
        r#"{"seq":3,"type":"request","command":"AttachDebugTargetRequest","arguments":{"Id":"target-1"}}"#,
    );
    assert!(
        read_dap_message_containing(&mut stdout, "\"command\":\"AttachDebugTargetRequest\"")
            .contains("\"success\":true")
    );
    rdbg.wait_for_ping();

    // A synchronous ping would withhold this response until the 750 ms
    // release below. The worker-based adapter must respond while it is held.
    rdbg.release_pings_after(Duration::from_millis(750));
    let started = Instant::now();
    send_dap_request(
        &mut stdin,
        r#"{"seq":4,"type":"request","command":"stepIn","arguments":{"threadId":1}}"#,
    );
    let response = read_dap_message_containing(&mut stdout, "\"command\":\"stepIn\"");
    assert!(response.contains("\"success\":true"));
    assert!(
        started.elapsed() < Duration::from_millis(400),
        "stepIn was delayed by the held RDBG poll: {:?}",
        started.elapsed()
    );
    assert!(
        read_dap_message_containing(&mut stdout, "\"event\":\"continued\"")
            .contains("\"threadId\":1")
    );

    send_dap_request(
        &mut stdin,
        r#"{"seq":5,"type":"request","command":"disconnect","arguments":{}}"#,
    );
    assert!(
        read_dap_message_containing(&mut stdout, "\"command\":\"disconnect\"")
            .contains("\"success\":true")
    );
    drop(stdin);
    assert!(child.wait().unwrap().success());
    let trace = fs::read_to_string(&trace_file).unwrap();
    assert!(trace.contains("\"event\":\"dap.step.received\""));
    assert!(trace.contains("\"event\":\"rdbg.step.response\""));
    assert!(trace.contains("\"event\":\"rdbg.poll.worker_spawned\""));
    fs::remove_file(trace_file).unwrap();
    rdbg.shutdown();
}

#[cfg(unix)]
struct FakeRdbgServer {
    port: u16,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    ping_started: Arc<(Mutex<bool>, Condvar)>,
    release_ping: Arc<(Mutex<bool>, Condvar)>,
    worker: Option<thread::JoinHandle<()>>,
}

#[cfg(unix)]
impl FakeRdbgServer {
    fn start() -> Self {
        Self::start_with_held_pings(false)
    }

    fn start_with_held_pings(hold_pings: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let ping_started = Arc::new((Mutex::new(false), Condvar::new()));
        let release_ping = Arc::new((Mutex::new(false), Condvar::new()));
        let worker_requests = Arc::clone(&requests);
        let worker_stop = Arc::clone(&stop);
        let worker_ping_started = Arc::clone(&ping_started);
        let worker_release_ping = Arc::clone(&release_ping);
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let requests = Arc::clone(&worker_requests);
                        let ping_started = Arc::clone(&worker_ping_started);
                        let release_ping = Arc::clone(&worker_release_ping);
                        thread::spawn(move || {
                            respond_to_rdbg_request(
                                stream,
                                requests,
                                hold_pings,
                                ping_started,
                                release_ping,
                            );
                        });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("fake RDBG server accept failed: {error}"),
                }
            }
        });
        Self {
            port,
            requests,
            stop,
            ping_started,
            release_ping,
            worker: Some(worker),
        }
    }

    fn wait_for_ping(&self) {
        let (started, ready) = &*self.ping_started;
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut started = started.lock().unwrap();
        while !*started {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "adapter did not start the expected RDBG long-poll"
            );
            let (next, _) = ready.wait_timeout(started, remaining).unwrap();
            started = next;
        }
    }

    fn release_pings_after(&self, delay: Duration) {
        let release_ping = Arc::clone(&self.release_ping);
        thread::spawn(move || {
            thread::sleep(delay);
            let (released, ready) = &*release_ping;
            *released.lock().unwrap() = true;
            ready.notify_all();
        });
    }

    fn shutdown(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let (released, ready) = &*self.release_ping;
        *released.lock().unwrap() = true;
        ready.notify_all();
        self.worker.take().unwrap().join().unwrap();
    }
}

#[cfg(unix)]
fn respond_to_rdbg_request(
    mut stream: TcpStream,
    requests: Arc<Mutex<Vec<String>>>,
    hold_pings: bool,
    ping_started: Arc<(Mutex<bool>, Condvar)>,
    release_ping: Arc<(Mutex<bool>, Condvar)>,
) {
    let Some(path) = read_http_request_path(&mut stream) else {
        return;
    };
    requests.lock().unwrap().push(path.clone());
    if hold_pings && path.contains("cmd=pingDebugUIParams") {
        let (started, ready) = &*ping_started;
        *started.lock().unwrap() = true;
        ready.notify_all();
        let (released, ready) = &*release_ping;
        let mut released = released.lock().unwrap();
        while !*released {
            released = ready.wait(released).unwrap();
        }
    }
    let body = if path.contains("cmd=attachDebugUI") {
        "<response><result>registered</result></response>"
    } else if path.contains("cmd=detachDebugUI") {
        "<response><result>true</result></response>"
    } else if path.contains("cmd=getDbgTargets") {
        concat!(
            "<response><id><id>target-1</id><seanceNo>1</seanceNo>",
            "<userName>Probe</userName><targetType>ManagedClient</targetType></id></response>"
        )
    } else {
        "<response/>"
    };
    let response = format!(
        concat!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/xml\r\n",
            "Content-Length: {}\r\nConnection: close\r\n\r\n{}"
        ),
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).unwrap();
}

#[cfg(unix)]
fn read_http_request_path(stream: &mut TcpStream) -> Option<String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut header = Vec::new();
    let mut byte = [0_u8; 1];
    while stream.read(&mut byte).ok()? == 1 {
        header.push(byte[0]);
        if header.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8(header)
        .ok()?
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)
        .map(str::to_owned)
}

#[cfg(unix)]
fn write_executable(path: &Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, contents).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
fn send_dap_request(stdin: &mut impl Write, request: &str) {
    write!(stdin, "Content-Length: {}\r\n\r\n{request}", request.len()).unwrap();
    stdin.flush().unwrap();
}

#[cfg(unix)]
fn read_dap_message(stdout: &mut impl BufRead) -> String {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        assert!(!line.is_empty(), "DAP adapter closed stdout unexpectedly");
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length: ") {
            content_length = Some(value.trim().parse::<usize>().unwrap());
        }
    }
    let mut body = vec![0_u8; content_length.expect("DAP response omitted Content-Length")];
    stdout.read_exact(&mut body).unwrap();
    String::from_utf8(body).unwrap()
}

#[cfg(unix)]
fn read_dap_message_containing(stdout: &mut impl BufRead, expected: &str) -> String {
    for _ in 0..8 {
        let message = read_dap_message(stdout);
        if message.contains(expected) {
            return message;
        }
    }
    panic!("did not receive DAP message containing {expected}");
}

#[cfg(unix)]
fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(1);
    while !path.is_file() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(unix)]
#[test]
fn launch_file_infobase_manages_dbgs_and_debuggee_over_rdbg() {
    let _lock = RDBG_PROCESS_TEST_LOCK.lock().unwrap();
    let root = std::env::temp_dir().join(format!("onec-launch-{}", uuid::Uuid::new_v4()));
    let home = root.join("home");
    fs::create_dir_all(home.join(".1cv8/1C/1CEStart")).unwrap();
    fs::write(
        home.join(".1cv8/1C/1CEStart/ibases.v8i"),
        "[FileDemo]\nConnect=File=\"/tmp/file-demo\";\n",
    )
    .unwrap();
    let rdbg = FakeRdbgServer::start();
    write_executable(
        &root.join("dbgs"),
        &format!(
            r#"#!/bin/sh
for argument in "$@"; do
    case "$argument" in
        --notify=*) notify="${{argument#--notify=}}" ;;
    esac
done
printf '127.0.0.1:{}' > "$notify"
printf '%s\n' "$@" > "$(dirname "$0")/dbgs.args"
while true; do sleep 1; done
"#,
            rdbg.port
        ),
    );
    write_executable(
        &root.join("1cv8c"),
        r#"#!/bin/sh
printf '%s\n' "$@" > "$(dirname "$0")/client.args"
while true; do sleep 1; done
"#,
    );

    let mut child = Command::new(env!("CARGO_BIN_EXE_onec-debug-adapter"))
        .env("HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    send_dap_request(
        &mut stdin,
        r#"{"seq":1,"type":"request","command":"initialize","arguments":{}}"#,
    );
    assert!(read_dap_message(&mut stdout).contains("\"command\":\"initialize\""));
    send_dap_request(
        &mut stdin,
        &format!(
            concat!(
                r#"{{"seq":2,"type":"request","command":"launch","arguments":{{"#,
                r#""infoBase":"FileDemo","platformPath":"{}","#,
                r#""debugServerHost":"127.0.0.1","debugServerPort":1}}}}"#
            ),
            root.display()
        ),
    );
    assert!(read_dap_message(&mut stdout).contains("\"command\":\"launch\""));
    assert!(read_dap_message(&mut stdout).contains("\"event\":\"initialized\""));
    send_dap_request(
        &mut stdin,
        r#"{"seq":3,"type":"request","command":"configurationDone","arguments":{}}"#,
    );
    assert!(read_dap_message(&mut stdout).contains("\"command\":\"configurationDone\""));
    wait_for_file(&root.join("dbgs.args"));
    wait_for_file(&root.join("client.args"));

    send_dap_request(
        &mut stdin,
        r#"{"seq":4,"type":"request","command":"disconnect","arguments":{}}"#,
    );
    let disconnect = read_dap_message_containing(&mut stdout, "\"command\":\"disconnect\"");
    assert!(disconnect.contains("\"success\":true"), "{disconnect}");
    drop(stdin);
    assert!(child.wait().unwrap().success());

    let dbgs_args = fs::read_to_string(root.join("dbgs.args")).unwrap();
    assert!(dbgs_args.contains("--portRange=1550:1559"));
    assert!(dbgs_args.contains("--addr=127.0.0.1"));
    let client_args = fs::read_to_string(root.join("client.args")).unwrap();
    assert!(client_args.contains("/IBName\nFileDemo"));
    assert!(client_args.contains("-http\n-attach"));
    assert!(client_args.contains(&format!("/DEBUGGERURL\nhttp://127.0.0.1:{}", rdbg.port)));
    let requests = rdbg.requests.lock().unwrap().clone();
    assert!(
        requests
            .iter()
            .any(|request| request.contains("rdbgTest?cmd=test"))
    );
    assert!(
        requests
            .iter()
            .any(|request| request.contains("cmd=attachDebugUI"))
    );
    assert!(
        requests
            .iter()
            .any(|request| request.contains("cmd=detachDebugUI"))
    );
    rdbg.shutdown();
    fs::remove_dir_all(root).unwrap();
}
