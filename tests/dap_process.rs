use std::io::Write;
use std::process::{Command, Stdio};

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
