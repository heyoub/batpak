//! Opt-in server-only TLS transport (`feature = "tls"`).
//!
//! [`TlsServerConfig`] wraps a built `Arc<rustls::ServerConfig>` with no client
//! authentication (server-only TLS for 0.9.0). The accept loop hands an
//! already-accepted raw `TcpStream` to the per-connection worker, which calls
//! [`TlsServerConfig::handshake`] AFTER acquiring its concurrency permit — so a
//! slow or malicious handshake occupies only one worker+permit slot and can
//! never head-of-line-block the accept loop.
//!
//! Authentication and authorization are deliberately OUT of scope: TLS here is
//! confidentiality + server identity only. Caller domains layer auth above the
//! transport, never inside netbat.

use std::fs;
use std::io;
use std::net::TcpStream;
use std::path::Path;
use std::sync::Arc;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};

use super::error::NetbatError;

/// Blocking rustls server stream: `Read + Write` over the accepted socket, so
/// the SAME generic per-connection serve path drives both plaintext and TLS.
pub(crate) type TlsStream = StreamOwned<ServerConnection, TcpStream>;

/// Server-only TLS configuration for the request listener.
///
/// Holds a built `Arc<rustls::ServerConfig>` (no client auth). Build it from PEM
/// bytes or PEM file paths; a malformed/empty cert chain or private key, or a
/// rustls config rejection, returns a typed [`NetbatError`] — never a panic.
#[derive(Clone)]
pub struct TlsServerConfig {
    server_config: Arc<ServerConfig>,
}

impl std::fmt::Debug for TlsServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The wrapped rustls config carries private key material; keep it
        // opaque rather than risk leaking it through a derived Debug.
        f.debug_struct("TlsServerConfig").finish_non_exhaustive()
    }
}

impl TlsServerConfig {
    /// Build a server-only TLS config from a PEM cert chain and PEM private key.
    ///
    /// Wrap the result in [`TransportSecurity::Tls`](crate::TransportSecurity)
    /// and pass it to [`serve_tcp_listener_secured`](crate::serve_tcp_listener_secured)
    /// (or the subscription variant) to serve TLS connections.
    ///
    /// ```no_run
    /// use netbat::{TlsServerConfig, TransportSecurity};
    ///
    /// # fn configure(cert_chain_pem: &[u8], private_key_pem: &[u8])
    /// #     -> Result<TransportSecurity, netbat::NetbatError> {
    /// let tls = TlsServerConfig::from_pem(cert_chain_pem, private_key_pem)?;
    /// let security = TransportSecurity::Tls(tls);
    /// # Ok(security)
    /// # }
    /// ```
    ///
    /// # Errors
    /// Returns [`NetbatError::Io`] (`InvalidData`) when the cert chain is empty
    /// or unparseable, no private key is present, or rustls rejects the
    /// cert/key pair.
    pub fn from_pem(cert_chain_pem: &[u8], private_key_pem: &[u8]) -> Result<Self, NetbatError> {
        let certs = parse_cert_chain(cert_chain_pem)?;
        let key = parse_private_key(private_key_pem)?;
        let server_config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .map_err(|_| invalid_tls_data())?
                .with_no_client_auth()
                .with_single_cert(certs, key)
                .map_err(|_| invalid_tls_data())?;
        Ok(Self {
            server_config: Arc::new(server_config),
        })
    }

    /// Build a server-only TLS config from a PEM cert-chain file and PEM
    /// private-key file.
    ///
    /// # Errors
    /// Returns [`NetbatError::Io`] when either file cannot be read, or any error
    /// from [`TlsServerConfig::from_pem`] on the loaded bytes.
    pub fn from_pem_files(
        cert_chain_path: impl AsRef<Path>,
        private_key_path: impl AsRef<Path>,
    ) -> Result<Self, NetbatError> {
        let cert_chain_pem = fs::read(cert_chain_path).map_err(NetbatError::from)?;
        let private_key_pem = fs::read(private_key_path).map_err(NetbatError::from)?;
        Self::from_pem(&cert_chain_pem, &private_key_pem)
    }

    /// Drive the rustls server handshake to completion on the caller's thread,
    /// returning the blocking [`TlsStream`].
    ///
    /// Invoked from the per-connection worker AFTER the permit is held.
    /// `complete_io` loops until the handshake finishes or the socket IO fails:
    /// a cleartext peer's bytes are not a valid ClientHello, so rustls rejects
    /// them and this surfaces a typed [`NetbatError`] that the worker counts as
    /// a handshake failure and drops — never listener-fatal.
    pub(crate) fn handshake(&self, mut stream: TcpStream) -> Result<TlsStream, NetbatError> {
        let mut conn = ServerConnection::new(Arc::clone(&self.server_config))
            .map_err(|_| invalid_tls_data())?;
        conn.complete_io(&mut stream).map_err(NetbatError::from)?;
        Ok(StreamOwned::new(conn, stream))
    }
}

fn parse_cert_chain(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>, NetbatError> {
    // pki-types' PEM decoder replaces the unmaintained rustls-pemfile. Any
    // decode failure (`pem::Error`) collapses to the same typed InvalidData as
    // an empty chain — a garbage cert is InvalidData just like no cert at all.
    let certs = CertificateDer::pem_slice_iter(pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| invalid_tls_data())?;
    if certs.is_empty() {
        return Err(invalid_tls_data());
    }
    Ok(certs)
}

fn parse_private_key(pem: &[u8]) -> Result<PrivateKeyDer<'static>, NetbatError> {
    // `from_pem_slice` returns `Error::NoItemsFound` when the PEM carries no
    // key; both that and any parse error map to the typed InvalidData surface.
    PrivateKeyDer::from_pem_slice(pem).map_err(|_| invalid_tls_data())
}

/// Map any rustls cert/key/config rejection to a stable typed IO error.
///
/// `rustls::Error` carries no `io::ErrorKind`; collapsing every config-time
/// rejection to `InvalidData` keeps a malformed cert/key from ever panicking
/// while staying inside the existing [`NetbatError`] surface (no new public
/// variant, so the default build's error API is byte-identical).
fn invalid_tls_data() -> NetbatError {
    NetbatError::Io {
        kind: io::ErrorKind::InvalidData,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_pem_rejects_empty_cert_chain() {
        // An empty/garbage cert chain must surface a typed NetbatError, never a
        // panic. Pairs the InvalidData mapping with the no-certs guard.
        let err = TlsServerConfig::from_pem(b"not a pem", b"not a key")
            .expect_err("empty cert chain is rejected");
        assert_eq!(
            err,
            NetbatError::Io {
                kind: io::ErrorKind::InvalidData
            }
        );
    }
}
