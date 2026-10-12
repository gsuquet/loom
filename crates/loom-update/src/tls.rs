//! TLS configuration of every HTTPS client of loom: the updater's GitHub client and the OTLP
//! exporter of `loom-telemetry`.

use std::sync::Arc;

use ureq::tls::{RootCerts, TlsConfig, TlsProvider};

/// rustls with the ring crypto provider and the operating system's trust store.
///
/// Server certificates are verified by `rustls-platform-verifier`, so that a TLS-inspecting
/// proxy whose root the system trusts is accepted. No bundled root certificates.
#[must_use]
pub fn tls_config() -> TlsConfig {
    TlsConfig::builder()
        .root_certs(RootCerts::PlatformVerifier)
        .provider(TlsProvider::Rustls)
        .unversioned_rustls_crypto_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .build()
}
