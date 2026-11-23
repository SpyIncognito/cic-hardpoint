//! TLS for links between machines, both ends: the client config the fleet client and the relay
//! dial with, and a listener's acceptor with a self-signed certificate made at start. Neither
//! end checks a certificate: each protocol pins an identity key and binds its proofs to the TLS
//! session with [`export`], so TLS only carries the connection. The relay listener uses the
//! acceptor; a peer-to-peer fleet host can take the same one.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, TlsConnector, client, server};

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// TLS 1.3 that takes any server certificate (the server must still prove it holds its key).
pub fn client_config() -> io::Result<Arc<rustls::ClientConfig>> {
    let provider = provider();
    let config = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(io::Error::other)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AnyCertificate(provider)))
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// A TLS 1.3 acceptor with a fresh self-signed certificate.
pub fn acceptor() -> io::Result<TlsAcceptor> {
    let made = rcgen::generate_simple_self_signed(vec!["hardpoint".to_owned()])
        .map_err(io::Error::other)?;
    let cert: CertificateDer<'static> = made.cert.der().clone();
    let key = PrivateKeyDer::Pkcs8(made.signing_key.serialize_der().into());
    let config = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(io::Error::other)?
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .map_err(io::Error::other)?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// 32 bytes of keying material exported from the session under `label` (RFC 8446 section 7.5,
/// no context). Both ends export the same bytes only when they share one TLS session.
pub fn export(conn: &rustls::ConnectionCommon<impl Sized>, label: &[u8]) -> io::Result<[u8; 32]> {
    conn.export_keying_material([0u8; 32], label, None)
        .map_err(io::Error::other)
}

/// Dials `host:port` over TLS.
pub async fn connect(
    config: &Arc<rustls::ClientConfig>,
    host: &str,
    port: u16,
) -> io::Result<client::TlsStream<TcpStream>> {
    // A bare IPv6 address may come in brackets, as in a URL.
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let name = ServerName::try_from(host.to_owned())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let tcp = TcpStream::connect((host, port)).await?;
    tcp.set_nodelay(true)?;
    TlsConnector::from(Arc::clone(config))
        .connect(name, tcp)
        .await
}

/// A TCP listener whose connections are taken through TLS.
pub struct Listener {
    tcp: TcpListener,
    acceptor: TlsAcceptor,
}

impl Listener {
    pub async fn bind(addr: SocketAddr) -> io::Result<Self> {
        Ok(Self {
            tcp: TcpListener::bind(addr).await?,
            acceptor: acceptor()?,
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.tcp.local_addr()
    }

    /// The next connection's TCP side; [`Listener::handshake`] then does TLS, so a slow client
    /// does not hold up the next accept.
    pub async fn accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        let (tcp, from) = self.tcp.accept().await?;
        tcp.set_nodelay(true)?;
        Ok((tcp, from))
    }

    pub fn acceptor(&self) -> TlsAcceptor {
        self.acceptor.clone()
    }
}

/// The server's TLS handshake on an accepted connection.
pub async fn handshake(
    acceptor: &TlsAcceptor,
    tcp: TcpStream,
) -> io::Result<server::TlsStream<TcpStream>> {
    acceptor.accept(tcp).await
}

/// Takes any certificate: the server is pinned by its identity key, so the certificate can be
/// self-signed and renew freely. Handshake signatures are still verified.
#[derive(Debug)]
struct AnyCertificate(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn both_ends_export_the_same_binding_for_one_session_only() {
        let listener = Listener::bind(([127, 0, 0, 1], 0).into()).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let acceptor = listener.acceptor();
        let server = tokio::spawn(async move {
            let mut out = vec![];
            for _ in 0..2 {
                let (tcp, _) = listener.accept().await.unwrap();
                let tls = handshake(&acceptor, tcp).await.unwrap();
                out.push(export(tls.get_ref().1, b"EXPORTER-test").unwrap());
            }
            out
        });
        let config = client_config().unwrap();
        let mut ours = vec![];
        for _ in 0..2 {
            let tls = connect(&config, "127.0.0.1", port).await.unwrap();
            ours.push(export(tls.get_ref().1, b"EXPORTER-test").unwrap());
        }
        let theirs = server.await.unwrap();
        assert_eq!(ours, theirs);
        assert_ne!(ours[0], ours[1]);
    }
}
