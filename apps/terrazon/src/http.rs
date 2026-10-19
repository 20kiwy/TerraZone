//! The app's HTTPS agents: one builder that carries the same rustls TLS on every target.
//!
//! ureq 2's cargo features only choose which TLS crates compile — **none of them wire
//! themselves in**. A builder that skips [`ureq::AgentBuilder::tls_connector`] rides
//! ureq's `NoTlsConfig` stub and answers every https ask with
//! "Unknown Scheme: cannot make HTTPS request because no TLS backend is configured"
//! (ureq's own lib.rs falls back to it). That is what the 0.14.1 Windows feature flip
//! exposed, and it shaped this module.
//!
//! The story since 0.22.0 is **one TLS stack on every target: rustls**.
//!
//! - Linux/macOS: the workspace's ureq carries the `tls` feature (rustls + ring) and ureq
//!   wires it itself — the builder needs nothing.
//! - Windows: ureq runs with **no** TLS feature (ring and aws-lc-rs both want a C compiler,
//!   and the Windows build has shipped zero-C-compiler since 0.14.1). The agent gets
//!   [`tls::Connector`] — rustls driven by the **pure-Rust RustCrypto provider**
//!   (`rustls-rustcrypto`) with the webpki root bundle. Same ClientHello family as the
//!   live-verified Linux build.
//!
//! Why schannel had to go (0.14.1–0.21.0 wired the OS's TLS through `native-tls`): the
//! radars' CDN kept refusing the Marine bootstrap on Windows — the homepage that mints the
//! `__cf_bm` session cookie answered 403 to schannel's handshake while the identical
//! request through rustls answered 200. The user saw it as "Marine never works in
//! Windows". The handshake was the tell: the same wire bytes as the Linux build was the
//! only fix that could not drift.
//!
//! [`tls::Connector`] compiles on every target so the Linux test suite drives the exact
//! code the Windows binary ships (a rig test handshakes it against the real vendors with
//! `cargo test -- --ignored`).

/// An agent builder with the target's TLS backend configured. Chain the usual
/// `.timeout(..)` / `.user_agent(..)` on it and `build()` as before.
pub fn agent_builder() -> ureq::AgentBuilder {
    #[cfg(windows)]
    {
        ureq::AgentBuilder::new().tls_connector(std::sync::Arc::new(tls::Connector))
    }
    #[cfg(not(windows))]
    {
        ureq::AgentBuilder::new() // ureq's `tls` feature (rustls) wires itself in here
    }
}

/// The rustls connector every target builds and tests, and the Windows agents ride: a
/// process-wide `ClientConfig` over the pure-Rust RustCrypto provider and the webpki root
/// bundle (the same roots ureq's own `tls` feature ships on Linux — no OS cert store is
/// consulted, so a bare Windows install behaves exactly like the rig).
#[cfg_attr(not(windows), allow(dead_code))] // the Linux/macOS builds carry it for the tests
pub mod tls {
    use std::io::{Read, Write};
    use std::sync::Arc;

    use ureq::{ReadWrite, TlsConnector};

    /// The connector handed to every Windows agent. Stateless — the config is shared.
    #[derive(Debug)]
    pub struct Connector;

    impl TlsConnector for Connector {
        fn connect(&self, dns_name: &str, io: Box<dyn ReadWrite>) -> Result<Box<dyn ReadWrite>, ureq::Error> {
            let Some(cfg) = config() else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "tls unavailable: the pure-Rust provider failed to build its config",
                )
                .into());
            };
            let server_name = rustls::pki_types::ServerName::try_from(dns_name.to_string())
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("bad DNS name `{dns_name}`: {e}")))?;
            let conn = rustls::ClientConnection::new(Arc::clone(cfg), server_name)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("tls connection setup failed: {e}")))?;
            Ok(Box::new(Stream(rustls::StreamOwned::new(conn, io))))
        }
    }

    /// The process-wide client config: RustCrypto's pure-Rust provider (AES-GCM,
    /// ChaCha20Poly1305, x25519/P-256 key exchange, RSA/ECDSA verification — no C in any
    /// of them) over the webpki roots. TLS 1.2 and 1.3. `None` only if the provider build
    /// somehow refused (it cannot, with its own suites and versions) — the connector then
    /// answers every https ask with a clear transport error instead of aborting the app.
    fn config() -> Option<&'static Arc<rustls::ClientConfig>> {
        static CONFIG: std::sync::OnceLock<Option<Arc<rustls::ClientConfig>>> = std::sync::OnceLock::new();
        CONFIG
            .get_or_init(|| {
                rustls::ClientConfig::builder_with_provider(Arc::new(rustls_rustcrypto::provider()))
                    .with_safe_default_protocol_versions()
                    .ok()
                    .map(|builder| Arc::new(builder.with_root_certificates(roots()).with_no_client_auth()))
            })
            .as_ref()
    }

    /// The webpki root bundle (Mozilla's set ureq itself ships for rustls on Linux).
    fn roots() -> rustls::RootCertStore {
        let mut store = rustls::RootCertStore::empty();
        store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        store
    }

    /// rustls over a ureq socket: close_notify surfaces as EOF (a truncated answer reads as
    /// an ended body, the way ureq's own rustls stream treats it), everything else passes.
    struct Stream(rustls::StreamOwned<rustls::ClientConnection, Box<dyn ReadWrite>>);

    impl std::fmt::Debug for Stream {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("rustls::StreamOwned<ClientConnection>")
        }
    }

    impl ReadWrite for Stream {
        fn socket(&self) -> Option<&std::net::TcpStream> {
            self.0.get_ref().socket()
        }
    }

    impl Read for Stream {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.0.read(buf) {
                Ok(n) => Ok(n),
                Err(ref e) if e.kind() == std::io::ErrorKind::ConnectionAborted => Ok(0),
                Err(e) => Err(e),
            }
        }
    }

    impl Write for Stream {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.write(buf)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.0.flush()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// The config builds once over the pure-Rust provider (the OnceLock path).
        #[test]
        fn config_builds_over_the_pure_rust_provider() {
            assert!(config().is_some(), "the pure-Rust provider builds its config");
            let first = config().map(Arc::as_ptr);
            let second = config().map(Arc::as_ptr);
            assert_eq!(first, second, "the config is built once");
        }

        /// An unwired builder dies with the stub's "Unknown Scheme"; ours gets as far as the
        /// connection itself — the discriminator is the error's SHAPE (ureq checks the
        /// scheme before it connects, so the test needs no network).
        #[test]
        fn connector_wires_https_not_the_stub() {
            let agent = crate::http::agent_builder().timeout(std::time::Duration::from_secs(1)).build();
            let err = agent.get("https://127.0.0.1:9/not-a-server").call().unwrap_err().to_string();
            assert!(!err.contains("Unknown Scheme"), "https is not wired on this target: {err}");
        }

        /// The rig's live proof (network): the exact connector the Windows binary ships
        /// handshakes the radars' CDN — the ask schannel kept failing (`cargo test -p
        /// terrazon -- --ignored` on a connected machine).
        #[test]
        #[ignore = "the live vendors; the rig runs it where the network is"]
        fn the_connector_handshakes_the_radars_cdn() {
            let agent = ureq::AgentBuilder::new().tls_connector(Arc::new(Connector)).timeout(std::time::Duration::from_secs(15)).build();
            let resp = agent
                .get("https://www.marinetraffic.com/")
                .set("User-Agent", "MarineTraffic/5.1.10 (iOS)")
                .set("Accept", "text/html,application/xhtml+xml")
                .call()
                .expect("the Marine bootstrap handshakes over the pure-Rust rustls");
            let cookies: Vec<_> = resp.all("set-cookie");
            assert_eq!(resp.status(), 200, "the CDN answers the rustls handshake");
            assert!(cookies.iter().any(|c| c.starts_with("__cf_bm=")), "the session cookie mints: {cookies:?}");
        }
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
}
