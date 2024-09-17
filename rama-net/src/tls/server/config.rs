use rama_utils::str::NonEmptyString;

use crate::tls::{ApplicationProtocol, KeyLogIntent, ProtocolVersion};

#[derive(Debug, Clone)]
/// Common API to configure a TLS Server
pub struct ServerConfig {
    /// required raw (PEM-encoded) server auth certs
    pub server_auth: ServerAuth,

    /// optional supported versions by the server
    pub protocol_versions: Option<Vec<ProtocolVersion>>,

    /// optional ALPNs used for protocol negotiation with the client
    pub application_layer_protocol_negotiation: Option<Vec<ApplicationProtocol>>,

    /// optionally define how client should be verified by server
    pub client_verify_mode: ClientVerifyMode,

    /// key log intent
    pub key_logger: KeyLogIntent,
}

impl ServerConfig {
    /// Create a new [`ServerConfig`] using the given [`ServerAuth`].
    pub fn new(auth: ServerAuth) -> Self {
        Self {
            server_auth: auth,
            protocol_versions: None,
            application_layer_protocol_negotiation: None,
            client_verify_mode: ClientVerifyMode::default(),
            key_logger: KeyLogIntent::default(),
        }
    }
}

#[derive(Debug, Clone)]
/// The kind of server auth to be used.
pub enum ServerAuth {
    /// Request the tls implementation to generate self-signed single data
    SelfSigned(SelfSignedData),
    /// Single data provided by the configurator
    Single(ServerAuthData),
}

#[derive(Debug, Clone, Default)]
/// Data that can be used to configure the self-signed single data
pub struct SelfSignedData {
    /// name of the organisation
    pub organisation_name: Option<String>,
    /// common name (CN): server name protected by the SSL certificate
    ///
    /// (usually the host domain name)
    pub common_name: Option<String>,
    /// Subject Alternative Names (SAN) can be defined
    /// to create a cert which allows multiple hostnames or domains to be secured under one certificate.
    pub subject_alternative_names: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
/// Raw private key and certificate data to facilitate client authentication.
pub struct ServerAuthData {
    /// PEM-encoded (x509) private key used by client
    pub private_key_pem: NonEmptyString,
    /// PEM-encoded certificate chain as a companion to the private key
    pub cert_chain_pem: NonEmptyString,

    /// `ocsp` is a DER-encoded OCSP response
    pub ocsp: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// Mode of client verification by a (tls) server
pub enum ClientVerifyMode {
    #[default]
    /// Use the default verification approach as defined
    /// by the implementation of the used (tls) server
    Auto,
    /// Explicitly disable client verification (if possible)
    Disable,
    /// PEM-encoded certificate chain containing the acceptable client certificates
    ClientAuth(NonEmptyString),
}
