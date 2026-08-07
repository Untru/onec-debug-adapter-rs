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
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
#[cfg(unix)]
use std::thread;
#[cfg(unix)]
use std::time::{Duration, Instant};

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
}

#[cfg(unix)]
struct FakeRdbgServer {
    port: u16,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

#[cfg(unix)]
impl FakeRdbgServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_requests = Arc::clone(&requests);
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        if let Some(path) = read_http_request_path(&mut stream) {
                            worker_requests.lock().unwrap().push(path.clone());
                            let body = if path.contains("cmd=attachDebugUI") {
                                "<response><result>registered</result></response>"
                            } else if path.contains("cmd=detachDebugUI") {
                                "<response><result>true</result></response>"
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
            worker: Some(worker),
        }
    }

    fn shutdown(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.worker.take().unwrap().join().unwrap();
    }
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
    wait_for_file(&root.join("dbgs.args"));
    wait_for_file(&root.join("client.args"));

    send_dap_request(
        &mut stdin,
        r#"{"seq":3,"type":"request","command":"disconnect","arguments":{}}"#,
    );
    assert!(read_dap_message(&mut stdout).contains("\"command\":\"disconnect\""));
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
            .any(|request| request.contains("cmd=rdbgTest"))
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
