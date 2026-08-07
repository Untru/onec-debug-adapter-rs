//! HTTP boundary for the 1C debug server.
//!
//! 1C exposes the remote-debug API below `/e1crdbg`. Keeping this module isolated
//! means DAP handling can be tested without a running 1C installation.

use anyhow::{Context, Result, bail};

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
}
