//! The app's HTTPS agents: one builder that carries a TLS backend on every target.
//!
//! ureq 2's cargo features only choose which TLS crates compile — **none of them wire
//! themselves in**. A builder that skips [`ureq::AgentBuilder::tls_connector`] rides
//! ureq's `NoTlsConfig` stub and answers every https ask with
//! "Unknown Scheme: cannot make HTTPS request because no TLS backend is configured"
//! (ureq's own lib.rs falls back to it). That is exactly what the 0.14.1 Windows
//! feature flip exposed: `features = ["native-tls"]` compiled schannel in, but the
//! basemap tiles and all three fleet feeds still died in transport, because the three
//! agent sites here built their agents bare.
//!
//! - Every native target but Windows: the workspace's ureq carries the `tls` feature
//!   (rustls + webpki roots) and ureq wires it itself — the builder needs nothing.
//! - Windows: ureq runs `native-tls` (the OS's schannel — the 0.14.1 "no C compiler"
//!   fix; rustls's ring wants cl.exe). Schannel's connector MUST be handed to the
//!   builder explicitly, which is what [`agent_builder`] does.

/// An agent builder with the target's TLS backend configured. Chain the usual
/// `.timeout(..)` / `.user_agent(..)` on it and `build()` as before.
pub fn agent_builder() -> ureq::AgentBuilder {
    #[cfg(windows)]
    {
        // schannel connector construction is effectively infallible on real Windows
        // installs; if it ever fails, the stock builder at least surfaces the honest
        // "no TLS backend" transport error in the feed statuses instead of a panic.
        match native_tls::TlsConnector::new() {
            Ok(connector) => ureq::AgentBuilder::new().tls_connector(std::sync::Arc::new(connector)),
            Err(e) => {
                eprintln!("terrazon: native-tls connector unavailable ({e}); https fetches will fail");
                ureq::AgentBuilder::new()
            }
        }
    }
    #[cfg(not(windows))]
    {
        ureq::AgentBuilder::new() // ureq's `tls` feature (rustls) wires itself in here
    }
}

#[cfg(test)]
mod tests {
    use super::agent_builder;

    #[test]
    fn agent_builder_builds_an_agent() {
        let agent = agent_builder().timeout(std::time::Duration::from_secs(1)).build();
        // A plain http ask (no TLS involved) reaches the transport layer on every target.
        let err = agent.get("http://127.0.0.1:9/not-a-server").call().unwrap_err().to_string();
        assert!(!err.contains("Unknown Scheme"), "unexpected scheme error: {err}");
    }

    #[test]
    fn https_is_configured_not_stubbed() {
        // The discriminator is the error's SHAPE, not its success, and the test needs no
        // network: port 9 on the loopback has no listener, but ureq checks the scheme
        // BEFORE it connects. An unwired agent (the 0.14.1–0.18.0 Windows shape) fails
        // with "Unknown Scheme: … no TLS backend is configured"; a wired one — rustls
        // here, schannel on Windows — gets as far as the connection itself.
        let agent = agent_builder().timeout(std::time::Duration::from_secs(1)).build();
        let err = agent.get("https://127.0.0.1:9/not-a-server").call().unwrap_err().to_string();
        assert!(!err.contains("Unknown Scheme"), "https is not wired on this target: {err}");
    }
}
