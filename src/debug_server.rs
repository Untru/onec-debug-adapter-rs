//! HTTP boundary for the 1C debug server.
//!
//! 1C exposes the remote-debug API below `/e1crdbg`. Keeping this module isolated
//! means DAP handling can be tested without a running 1C installation.

use anyhow::{Context, Result, bail};
use quick_xml::Reader;
use quick_xml::events::Event;
use std::fmt;
use uuid::Uuid;

const RDBG_REQUEST_NAMESPACE: &str = "http://v8.1c.ru/8.3/debugger/debugRDBGRequestResponse";

/// A successfully registered 1C Debug UI session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugUiSession {
    id: String,
    info_base_alias: String,
}

impl DebugUiSession {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn info_base_alias(&self) -> &str {
        &self.info_base_alias
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttachDebugUiResult {
    Registered,
    CredentialsRequired,
    FullCredentialsRequired,
    IbInDebug,
    NotRegistered,
    Unknown,
}

impl AttachDebugUiResult {
    fn parse(value: &str) -> Result<Self> {
        match value.trim() {
            "registered" => Ok(Self::Registered),
            "credentialsRequired" => Ok(Self::CredentialsRequired),
            "fullCredentialsRequired" => Ok(Self::FullCredentialsRequired),
            "ibInDebug" => Ok(Self::IbInDebug),
            "notRegistered" => Ok(Self::NotRegistered),
            "unknown" => Ok(Self::Unknown),
            other => bail!("unknown attachDebugUI result `{other}`"),
        }
    }
}

impl fmt::Display for AttachDebugUiResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Registered => "registered",
            Self::CredentialsRequired => "credentialsRequired",
            Self::FullCredentialsRequired => "fullCredentialsRequired",
            Self::IbInDebug => "ibInDebug",
            Self::NotRegistered => "notRegistered",
            Self::Unknown => "unknown",
        };
        formatter.write_str(value)
    }
}

#[derive(Debug, Clone)]
pub struct DebugServer {
    endpoint: String,
}

impl DebugServer {
    pub fn new(host: &str, port: u16) -> Result<Self> {
        if host.trim().is_empty() {
            bail!("debugServerHost must not be empty");
        }
        Ok(Self {
            endpoint: format!("http://{host}:{port}/e1crdbg"),
        })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Performs the same low-risk connectivity check used before attaching a UI.
    pub fn test_connection(&self) -> Result<()> {
        let url = format!("{}/rdbgTest?cmd=test", self.endpoint);
        let response = ureq::post(&url)
            .header("User-Agent", "1CV8")
            .header("Content-Type", "application/xml")
            .send_empty()
            .with_context(|| format!("cannot reach 1C debug server at {}", self.endpoint))?;
        let status = response.status();
        if !(200..300).contains(&status.as_u16()) {
            bail!("1C debug server returned HTTP {}", status);
        }
        Ok(())
    }

    /// Registers a Debug UI and returns its generated identifier.
    ///
    /// `info_base_alias` is the server-side infobase reference. For most
    /// server infobases it is the same value as the `Ref` connection-string
    /// attribute, rather than the display name in the 1C launcher.
    pub fn attach_debug_ui(&self, info_base_alias: &str) -> Result<DebugUiSession> {
        if info_base_alias.trim().is_empty() {
            bail!("infoBaseAlias must not be empty");
        }

        self.test_connection()?;
        let session = DebugUiSession {
            id: Uuid::new_v4().to_string(),
            info_base_alias: info_base_alias.to_owned(),
        };
        let body = attach_debug_ui_request(&session);
        let response = self.post_xml("attachDebugUI", &body)?;
        let result = AttachDebugUiResult::parse(&response_element(&response, "result")?)?;

        match result {
            AttachDebugUiResult::Registered => Ok(session),
            AttachDebugUiResult::CredentialsRequired
            | AttachDebugUiResult::FullCredentialsRequired => {
                bail!("1C debug server requires infobase credentials")
            }
            AttachDebugUiResult::IbInDebug => bail!("the infobase is already being debugged"),
            AttachDebugUiResult::NotRegistered => {
                bail!("the infobase is not registered for debugging")
            }
            AttachDebugUiResult::Unknown => {
                bail!("the 1C debug server returned an unknown attach result")
            }
        }
    }

    /// Unregisters the supplied Debug UI. A false result is surfaced as an
    /// error because retaining a stale UI session can prevent the next attach.
    pub fn detach_debug_ui(&self, session: &DebugUiSession) -> Result<()> {
        let body = base_request(session);
        let response = self.post_xml("detachDebugUI", &body)?;
        if response_element(&response, "result")?.trim() != "true" {
            bail!("1C debug server rejected detachDebugUI")
        }
        Ok(())
    }

    /// Configures which 1C execution contexts are automatically attached to
    /// this Debug UI. The spellings intentionally match `autoAttachTypes` in
    /// the existing VS Code extension.
    pub fn set_auto_attach_types(&self, session: &DebugUiSession, types: &[String]) -> Result<()> {
        for target_type in types {
            debug_target_type_xml_value(target_type)?;
        }
        let body = auto_attach_settings_request(session, types);
        self.post_xml("setAutoAttachSettings", &body).map(|_| ())
    }

    fn post_xml(&self, command: &str, body: &str) -> Result<String> {
        let url = format!("{}/rdbg?cmd={command}", self.endpoint);
        let mut response = ureq::post(&url)
            .header("User-Agent", "1CV8")
            .header("Content-Type", "application/xml; charset=utf-8")
            .send(body)
            .with_context(|| format!("1C debug server request `{command}` failed"))?;
        response
            .body_mut()
            .read_to_string()
            .context("cannot read XML response from 1C debug server")
    }
}

fn attach_debug_ui_request(session: &DebugUiSession) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><request xmlns=\"{RDBG_REQUEST_NAMESPACE}\"><infoBaseAlias>{}</infoBaseAlias><idOfDebuggerUI>{}</idOfDebuggerUI><userName></userName><credentials></credentials><options><foregroundAbility>true</foregroundAbility></options></request>",
        xml_escape(session.info_base_alias()),
        xml_escape(session.id())
    )
}

fn base_request(session: &DebugUiSession) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><request xmlns=\"{RDBG_REQUEST_NAMESPACE}\"><infoBaseAlias>{}</infoBaseAlias><idOfDebuggerUI>{}</idOfDebuggerUI></request>",
        xml_escape(session.info_base_alias()),
        xml_escape(session.id())
    )
}

fn auto_attach_settings_request(session: &DebugUiSession, types: &[String]) -> String {
    let target_types = types
        .iter()
        .map(|target_type| {
            format!(
                "<targetType>{}</targetType>",
                debug_target_type_xml_value(target_type).expect("validated before serialization")
            )
        })
        .collect::<String>();
    let base = base_request(session);
    base.replacen(
        "</request>",
        &format!("<autoAttachSettings>{target_types}</autoAttachSettings></request>"),
        1,
    )
}

fn debug_target_type_xml_value(target_type: &str) -> Result<&'static str> {
    match target_type {
        "Client" => Ok("Client"),
        "ManagedClient" => Ok("ManagedClient"),
        "Server" => Ok("Server"),
        "ServerEmulation" => Ok("ServerEmulation"),
        "OData" => Ok("OData"),
        "JobFileMode" => Ok("JobFileMode"),
        "MobileClient" => Ok("MobileClient"),
        "MobileServer" => Ok("MobileServer"),
        "MobileJobFileMode" => Ok("MobileJobFileMode"),
        "MobileManagedClient" => Ok("MobileManagedClient"),
        "MobileManagedServer" => Ok("MobileManagedServer"),
        "WebClient" => Ok("WEBClient"),
        "ComConnector" => Ok("COMConnector"),
        "WebService" => Ok("WEBService"),
        "HttpService" => Ok("HTTPService"),
        "Job" => Ok("JOB"),
        other => bail!("unsupported autoAttachTypes value `{other}`"),
    }
}

fn response_element(xml: &str, expected_element: &str) -> Result<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    loop {
        match reader.read_event()? {
            Event::Start(element)
                if element.local_name().as_ref() == expected_element.as_bytes() =>
            {
                return reader
                    .read_text(element.name())
                    .map(|text| text.into_owned())
                    .context("cannot read XML response value");
            }
            Event::Eof => bail!("1C debug server response has no `{expected_element}` element"),
            _ => {}
        }
    }
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn attach_request_contains_escaped_base_request_data() {
        let session = DebugUiSession {
            id: "0f45f589-6c7d-4f5f-8433-4e8f611e6a9a".to_owned(),
            info_base_alias: "Sales & <Retail>".to_owned(),
        };

        let xml = attach_debug_ui_request(&session);

        assert!(xml.contains("<infoBaseAlias>Sales &amp; &lt;Retail&gt;</infoBaseAlias>"));
        assert!(
            xml.contains("<idOfDebuggerUI>0f45f589-6c7d-4f5f-8433-4e8f611e6a9a</idOfDebuggerUI>")
        );
        assert!(xml.contains("<foregroundAbility>true</foregroundAbility>"));
    }

    #[test]
    fn parses_namespaced_response_result() {
        let xml = "<response xmlns=\"http://v8.1c.ru/8.3/debugger/debugBaseData\"><result>registered</result></response>";
        assert_eq!(response_element(xml, "result").unwrap(), "registered");
    }

    #[test]
    fn recognizes_all_documented_attach_results() {
        assert_eq!(
            AttachDebugUiResult::parse("registered").unwrap(),
            AttachDebugUiResult::Registered
        );
        assert_eq!(
            AttachDebugUiResult::parse("ibInDebug").unwrap(),
            AttachDebugUiResult::IbInDebug
        );
        assert!(AttachDebugUiResult::parse("unexpected").is_err());
    }

    #[test]
    fn converts_vscode_auto_attach_types_to_rdbg_values() {
        assert_eq!(debug_target_type_xml_value("HttpService").unwrap(), "HTTPService");
        assert_eq!(debug_target_type_xml_value("WebClient").unwrap(), "WEBClient");
        assert!(debug_target_type_xml_value("Unknown").is_err());
    }

    #[test]
    fn attaches_and_detaches_using_the_rdbg_http_endpoints() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server_thread = thread::spawn(move || {
            let responses = [
                "",
                "<response><result>registered</result></response>",
                "<response></response>",
                "<response><result>true</result></response>",
            ];
            let mut requests = Vec::new();

            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request_line = String::new();
                reader.read_line(&mut request_line).unwrap();
                let mut content_length = 0;
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = header.split_once(':') {
                        if name.eq_ignore_ascii_case("Content-Length") {
                            content_length = value.trim().parse().unwrap();
                        }
                    }
                }
                let mut body = vec![0; content_length];
                reader.read_exact(&mut body).unwrap();
                requests.push(format!(
                    "{request_line}{}",
                    String::from_utf8(body).unwrap()
                ));

                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response.len(),
                    response
                )
                .unwrap();
                stream.flush().unwrap();
            }
            requests
        });

        let server = DebugServer::new("127.0.0.1", port).unwrap();
        let session = server.attach_debug_ui("DemoBase").unwrap();
        server
            .set_auto_attach_types(&session, &["Client".to_owned(), "HttpService".to_owned()])
            .unwrap();
        server.detach_debug_ui(&session).unwrap();
        let requests = server_thread.join().unwrap();

        assert!(requests[0].starts_with("POST /e1crdbg/rdbgTest?cmd=test HTTP/1.1"));
        assert!(requests[1].starts_with("POST /e1crdbg/rdbg?cmd=attachDebugUI HTTP/1.1"));
        assert!(requests[1].contains("<infoBaseAlias>DemoBase</infoBaseAlias>"));
        assert!(requests[1].contains("<foregroundAbility>true</foregroundAbility>"));
        assert!(requests[2].starts_with("POST /e1crdbg/rdbg?cmd=setAutoAttachSettings HTTP/1.1"));
        assert!(requests[2].contains("<targetType>Client</targetType>"));
        assert!(requests[2].contains("<targetType>HTTPService</targetType>"));
        assert!(requests[3].starts_with("POST /e1crdbg/rdbg?cmd=detachDebugUI HTTP/1.1"));
        assert!(requests[3].contains(&format!(
            "<idOfDebuggerUI>{}</idOfDebuggerUI>",
            session.id()
        )));
    }
}
