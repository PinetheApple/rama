use crate::boring::dep::boring::{
    asn1::Asn1Time,
    bn::{BigNum, MsbOption},
    hash::MessageDigest,
    nid::Nid,
    pkey::{PKey, Private},
    rsa::Rsa,
    x509::{
        extension::{BasicConstraints, KeyUsage, SubjectKeyIdentifier},
        X509NameBuilder, X509,
    },
};
use boring::{
    ssl::SslAcceptorBuilder,
    x509::extension::{AuthorityKeyIdentifier, SubjectAlternativeName},
};
use moka::sync::Cache;
use rama_core::error::{ErrorContext, OpaqueError};
use rama_net::{
    address::{Domain, Host},
    tls::{
        server::{ClientVerifyMode, SelfSignedData, ServerAuth, ServerCertIssuerKind},
        ApplicationProtocol, DataEncoding, KeyLogIntent, ProtocolVersion,
    },
};
use std::{sync::Arc, time::Duration};

#[derive(Debug, Clone)]
/// Internal data used as configuration/input for the [`super::TlsAcceptorService`].
///
/// Created by trying to turn the _rama_ opiniated [`rama_net::tls::server::ServerConfig`] into it.
pub struct TlsAcceptorData {
    pub(super) config: Arc<TlsConfig>,
}

#[derive(Debug, Clone)]
pub(super) struct TlsConfig {
    /// source for certs
    pub(super) cert_source: TlsCertSource,
    /// Optionally set the ALPN protocols supported by the service's inner application service.
    pub(super) alpn_protocols: Option<Vec<ApplicationProtocol>>,
    /// Optionally write logging information to facilitate tls interception.
    pub(super) keylog_intent: KeyLogIntent,
    /// optionally define protocol versions to support
    pub(super) protocol_versions: Option<Vec<ProtocolVersion>>,
    /// optionally define client certificates in case client auth is enabled
    pub(super) client_cert_chain: Option<Vec<X509>>,
}

#[derive(Debug, Clone)]
pub(super) struct TlsCertSource {
    kind: TlsCertSourceKind,
}

#[derive(Debug, Clone)]
enum TlsCertSourceKind {
    InMemory {
        /// Private Key of the server
        private_key: PKey<Private>,
        /// Cert Chain of the server
        cert_chain: Vec<X509>,
    },
    InMemoryIssuer {
        /// Cache for certs already issued
        cert_cache: Cache<Host, IssuedCert>,
        /// Private Key for issueing
        ca_key: PKey<Private>,
        /// CA Cert to be used for issueing
        ca_cert: X509,
    },
}

#[derive(Debug, Clone)]
struct IssuedCert {
    cert: X509,
    key: PKey<Private>,
}

impl TlsCertSource {
    pub(super) async fn issue_certs(
        &self,
        mut builder: SslAcceptorBuilder,
        server_name: Option<Host>,
    ) -> Result<SslAcceptorBuilder, OpaqueError> {
        match &self.kind {
            TlsCertSourceKind::InMemory {
                private_key,
                cert_chain,
            } => {
                for (i, ca_cert) in cert_chain.iter().enumerate() {
                    if i == 0 {
                        builder
                            .set_certificate(ca_cert.as_ref())
                            .context("build boring ssl acceptor: set Leaf CA certificate (x509)")?;
                    } else {
                        builder.add_extra_chain_cert(ca_cert.clone()).context(
                            "build boring ssl acceptor: add extra chain certificate (x509)",
                        )?;
                    }
                }
                builder
                    .set_private_key(private_key.as_ref())
                    .context("build boring ssl acceptor: set private key")?;
                builder
                    .check_private_key()
                    .context("build boring ssl acceptor: check private key")?;
            }
            TlsCertSourceKind::InMemoryIssuer {
                cert_cache,
                ca_key,
                ca_cert,
            } => match server_name.clone() {
                Some(host) => {
                    tracing::trace!(%host, "try to use cached issued cert or generate new one");
                    let issued_cert = cert_cache
                        .try_get_with(host, || {
                            issue_cert_for_ca(server_name.clone(), ca_cert, ca_key)
                        })
                        .context("fresh issue of cert + insert")?;
                    add_issued_cert_to_cert_builder(
                        server_name,
                        issued_cert,
                        ca_cert.clone(),
                        &mut builder,
                    )?;
                }
                None => {
                    let issued_cert = issue_cert_for_ca(server_name.clone(), ca_cert, ca_key)?;
                    add_issued_cert_to_cert_builder(
                        server_name,
                        issued_cert,
                        ca_cert.clone(),
                        &mut builder,
                    )?;
                }
            },
        }

        Ok(builder)
    }
}

impl TryFrom<rama_net::tls::server::ServerConfig> for TlsAcceptorData {
    type Error = OpaqueError;

    fn try_from(value: rama_net::tls::server::ServerConfig) -> Result<Self, Self::Error> {
        let client_cert_chain = match value.client_verify_mode {
            // no client auth
            ClientVerifyMode::Auto | ClientVerifyMode::Disable => None,
            // client auth enabled
            ClientVerifyMode::ClientAuth(DataEncoding::Der(bytes)) => Some(vec![X509::from_der(
                &bytes[..],
            )
            .context("boring/TlsAcceptorData: parse x509 client cert from DER content")?]),
            ClientVerifyMode::ClientAuth(DataEncoding::DerStack(bytes_list)) => Some(
                bytes_list
                    .into_iter()
                    .map(|b| {
                        X509::from_der(&b[..]).context(
                            "boring/TlsAcceptorData: parse x509 client cert from DER content",
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            ClientVerifyMode::ClientAuth(DataEncoding::Pem(raw_data)) => Some(
                X509::stack_from_pem(raw_data.as_bytes())
                    .context("boring/TlsAcceptorData: parse x509 client cert from PEM content")?,
            ),
        };

        let cert_source_kind = match value.server_auth {
            ServerAuth::SelfSigned(data) => {
                let (cert_chain, private_key) =
                    self_signed_server_auth(data).context("boring/TlsAcceptorData")?;
                TlsCertSourceKind::InMemory {
                    private_key,
                    cert_chain,
                }
            }
            ServerAuth::Single(data) => {
                // server TLS Certs
                let cert_chain = match data.cert_chain {
                    DataEncoding::Der(raw_data) => vec![X509::from_der(&raw_data[..]).context(
                        "boring/TlsAcceptorData: parse x509 server cert from DER content",
                    )?],
                    DataEncoding::DerStack(raw_data_list) => raw_data_list
                        .into_iter()
                        .map(|raw_data| {
                            X509::from_der(&raw_data[..]).context(
                                "boring/TlsAcceptorData: parse x509 server cert from DER content",
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                    DataEncoding::Pem(raw_data) => X509::stack_from_pem(raw_data.as_bytes())
                        .context(
                            "boring/TlsAcceptorData: parse x509 server cert chain from PEM content",
                        )?,
                };

                // server TLS key
                let private_key = match data.private_key {
                    DataEncoding::Der(raw_data) => PKey::private_key_from_der(&raw_data[..])
                        .context("boring/TlsAcceptorData: parse private key from DER content")?,
                    DataEncoding::DerStack(raw_data_list) => PKey::private_key_from_der(
                        &raw_data_list
                            .first()
                            .context("boring/TlsAcceptorData: get first private key raw data")?[..],
                    )
                    .context("boring/TlsAcceptorData: parse private key from DER content")?,
                    DataEncoding::Pem(raw_data) => PKey::private_key_from_pem(raw_data.as_bytes())
                        .context("boring/TlsAcceptorData: parse private key from PEM content")?,
                };

                TlsCertSourceKind::InMemory {
                    private_key,
                    cert_chain,
                }
            }

            ServerAuth::CertIssuer(data) => {
                let cert_cache = Cache::builder()
                    .time_to_live(Duration::from_secs(60 * 60 * 24 * 89))
                    .max_capacity(if data.max_cache_size == 0 {
                        8096
                    } else {
                        data.max_cache_size
                    })
                    .build();

                match data.kind {
                    ServerCertIssuerKind::SelfSigned(data) => {
                        let (ca_cert, ca_key) = self_signed_server_ca(data)
                            .context("boring/TlsAcceptorData: CA: self-signed ca")?;
                        TlsCertSourceKind::InMemoryIssuer {
                            cert_cache,
                            ca_key,
                            ca_cert,
                        }
                    }
                    ServerCertIssuerKind::Single(data) => {
                        // server TLS Certs
                        let mut cert_chain = match data.cert_chain {
                        DataEncoding::Der(raw_data) => vec![X509::from_der(&raw_data[..]).context(
                            "boring/TlsAcceptorData: CA: parse x509 server cert from DER content",
                        )?],
                        DataEncoding::DerStack(raw_data_list) => raw_data_list
                            .into_iter()
                            .map(|raw_data| {
                                X509::from_der(&raw_data[..]).context(
                                    "boring/TlsAcceptorData: CA: parse x509 server cert from DER content",
                                )
                            })
                            .collect::<Result<Vec<_>, _>>()?,
                        DataEncoding::Pem(raw_data) => X509::stack_from_pem(raw_data.as_bytes())
                            .context(
                                "boring/TlsAcceptorData: CA: parse x509 server cert chain from PEM content",
                            )?,
                    };
                        let ca_cert = cert_chain.pop().context("pop CA Cert (last) from stack")?;

                        // server TLS key
                        let ca_key = match data.private_key {
                            DataEncoding::Der(raw_data) => PKey::private_key_from_der(
                                &raw_data[..],
                            )
                            .context(
                                "boring/TlsAcceptorData: CA: parse private key from DER content",
                            )?,
                            DataEncoding::DerStack(raw_data_list) => PKey::private_key_from_der(
                                &raw_data_list.first().context(
                                    "boring/TlsAcceptorData: CA: get first private key raw data",
                                )?[..],
                            )
                            .context(
                                "boring/TlsAcceptorData: CA: parse private key from DER content",
                            )?,
                            DataEncoding::Pem(raw_data) => PKey::private_key_from_pem(
                                raw_data.as_bytes(),
                            )
                            .context(
                                "boring/TlsAcceptorData: CA: parse private key from PEM content",
                            )?,
                        };

                        TlsCertSourceKind::InMemoryIssuer {
                            cert_cache,
                            ca_key,
                            ca_cert,
                        }
                    }
                }
            }
        };

        // return the created server config, all good if you reach here
        Ok(TlsAcceptorData {
            config: Arc::new(TlsConfig {
                cert_source: TlsCertSource {
                    kind: cert_source_kind,
                },
                alpn_protocols: value.application_layer_protocol_negotiation.clone(),
                keylog_intent: value.key_logger,
                protocol_versions: value.protocol_versions.clone(),
                client_cert_chain,
            }),
        })
    }
}

fn issue_cert_for_ca(
    server_name: Option<Host>,
    ca_cert: &X509,
    ca_key: &PKey<Private>,
) -> Result<IssuedCert, OpaqueError> {
    tracing::trace!(
        host = ?server_name,
        "generate certs for host using in-memory ca cert"
    );
    let (cert, key) = self_signed_server_auth_gen_cert(
        &SelfSignedData {
            organisation_name: Some(
                ca_cert
                    .subject_name()
                    .entries_by_nid(Nid::ORGANIZATIONNAME)
                    .next()
                    .and_then(|entry| entry.data().as_utf8().ok())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "Anonymous".to_owned()),
            ),
            common_name: server_name.clone(),
            subject_alternative_names: None,
        },
        ca_cert,
        ca_key,
    )
    .with_context(|| format!("issue certs in memory for: {server_name:?}"))?;

    Ok(IssuedCert { cert, key })
}

fn add_issued_cert_to_cert_builder(
    server_name: Option<Host>,
    issued_cert: IssuedCert,
    ca_cert: X509,
    builder: &mut SslAcceptorBuilder,
) -> Result<(), OpaqueError> {
    tracing::trace!(
        host = ?server_name,
        "add issued cert for host to (boring) SslAcceptorBuilder"
    );
    builder
        .set_certificate(issued_cert.cert.as_ref())
        .context("build boring ssl acceptor: issued in-mem: set certificate (x509)")?;
    builder
        .add_extra_chain_cert(ca_cert.clone())
        .context("build boring ssl acceptor: issued in-mem: add extra chain certificate (x509)")?;
    builder
        .set_private_key(issued_cert.key.as_ref())
        .context("build boring ssl acceptor: issued in-mem: set private key")?;
    builder
        .check_private_key()
        .context("build boring ssl acceptor: issued in-mem: check private key")?;

    Ok(())
}

fn self_signed_server_auth(
    data: SelfSignedData,
) -> Result<(Vec<X509>, PKey<Private>), OpaqueError> {
    let (ca_cert, ca_privkey) = self_signed_server_auth_gen_ca(&data).context("self-signed CA")?;
    let (cert, privkey) = self_signed_server_auth_gen_cert(&data, &ca_cert, &ca_privkey)
        .context("self-signed cert using self-signed CA")?;
    Ok((vec![cert, ca_cert], privkey))
}

#[inline]
fn self_signed_server_ca(data: SelfSignedData) -> Result<(X509, PKey<Private>), OpaqueError> {
    self_signed_server_auth_gen_ca(&data)
}

fn self_signed_server_auth_gen_cert(
    data: &SelfSignedData,
    ca_cert: &X509,
    ca_privkey: &PKey<Private>,
) -> Result<(X509, PKey<Private>), OpaqueError> {
    let rsa = Rsa::generate(4096).context("generate 4096 RSA key")?;
    let privkey = PKey::from_rsa(rsa).context("create private key from 4096 RSA key")?;

    let common_name = data
        .common_name
        .clone()
        .unwrap_or(Host::Name(Domain::from_static("localhost")));

    let mut x509_name = X509NameBuilder::new().context("create x509 name builder")?;
    x509_name
        .append_entry_by_nid(
            Nid::ORGANIZATIONNAME,
            data.organisation_name.as_deref().unwrap_or("Anonymous"),
        )
        .context("append organisation name to x509 name builder")?;
    for subject_alt_name in data.subject_alternative_names.iter().flatten() {
        x509_name
            .append_entry_by_nid(Nid::SUBJECT_ALT_NAME, subject_alt_name.as_ref())
            .context("append subject alt name to x509 name builder")?;
    }
    x509_name
        .append_entry_by_nid(Nid::COMMONNAME, common_name.to_string().as_str())
        .context("append common name to x509 name builder")?;
    let x509_name = x509_name.build();

    let mut cert_builder = X509::builder().context("create x509 (cert) builder")?;
    cert_builder
        .set_version(2)
        .context("x509 cert builder: set version = 2")?;
    let serial_number = {
        let mut serial = BigNum::new().context("x509 cert builder: create big num (serial")?;
        serial
            .rand(159, MsbOption::MAYBE_ZERO, false)
            .context("x509 cert builder: randomise serial number (big num)")?;
        serial
            .to_asn1_integer()
            .context("x509 cert builder: convert serial to ASN1 integer")?
    };
    cert_builder
        .set_serial_number(&serial_number)
        .context("x509 cert builder: set serial number")?;
    cert_builder
        .set_issuer_name(ca_cert.subject_name())
        .context("x509 cert builder: set issuer name")?;
    cert_builder
        .set_pubkey(&privkey)
        .context("x509 cert builder: set pub key")?;
    cert_builder
        .set_subject_name(&x509_name)
        .context("x509 cert builder: set subject name")?;
    cert_builder
        .set_pubkey(&privkey)
        .context("x509 cert builder: set public key using private key (ref)")?;
    let not_before =
        Asn1Time::days_from_now(0).context("x509 cert builder: create ASN1Time for today")?;
    cert_builder
        .set_not_before(&not_before)
        .context("x509 cert builder: set not before to today")?;
    let not_after = Asn1Time::days_from_now(90)
        .context("x509 cert builder: create ASN1Time for 90 days in future")?;
    cert_builder
        .set_not_after(&not_after)
        .context("x509 cert builder: set not after to 90 days in future")?;

    cert_builder
        .append_extension(
            BasicConstraints::new()
                .build()
                .context("x509 cert builder: build basic constraints")?,
        )
        .context("x509 cert builder: add basic constraints as x509 extension")?;
    cert_builder
        .append_extension(
            KeyUsage::new()
                .critical()
                .non_repudiation()
                .digital_signature()
                .key_encipherment()
                .build()
                .context("x509 cert builder: create key usage")?,
        )
        .context("x509 cert builder: add key usage x509 extension")?;

    let mut subject_alt_name = SubjectAlternativeName::new();
    match common_name {
        Host::Name(domain) => {
            subject_alt_name.dns(domain.as_str());
        }
        Host::Address(addr) => {
            subject_alt_name.ip(addr.to_string().as_str());
        }
    }
    let subject_alt_name = subject_alt_name
        .build(&cert_builder.x509v3_context(Some(ca_cert), None))
        .context("x509 cert builder: build subject alt name")?;

    cert_builder
        .append_extension(subject_alt_name)
        .context("x509 cert builder: add subject alt name")?;

    let subject_key_identifier = SubjectKeyIdentifier::new()
        .build(&cert_builder.x509v3_context(Some(ca_cert), None))
        .context("x509 cert builder: build subject key id")?;
    cert_builder
        .append_extension(subject_key_identifier)
        .context("x509 cert builder: add subject key id x509 extension")?;

    let auth_key_identifier = AuthorityKeyIdentifier::new()
        .keyid(false)
        .issuer(false)
        .build(&cert_builder.x509v3_context(Some(ca_cert), None))
        .context("x509 cert builder: build auth key id")?;
    cert_builder
        .append_extension(auth_key_identifier)
        .context("x509 cert builder: set auth key id extension")?;

    cert_builder
        .sign(ca_privkey, MessageDigest::sha256())
        .context("x509 cert builder: sign cert")?;

    let cert = cert_builder.build();

    Ok((cert, privkey))
}

fn self_signed_server_auth_gen_ca(
    data: &SelfSignedData,
) -> Result<(X509, PKey<Private>), OpaqueError> {
    let rsa = Rsa::generate(4096).context("generate 4096 RSA key")?;
    let privkey = PKey::from_rsa(rsa).context("create private key from 4096 RSA key")?;

    let common_name = data
        .common_name
        .clone()
        .unwrap_or(Host::Name(Domain::from_static("localhost")));

    let mut x509_name = X509NameBuilder::new().context("create x509 name builder")?;
    x509_name
        .append_entry_by_nid(
            Nid::ORGANIZATIONNAME,
            data.organisation_name.as_deref().unwrap_or("Anonymous"),
        )
        .context("append organisation name to x509 name builder")?;
    for subject_alt_name in data.subject_alternative_names.iter().flatten() {
        x509_name
            .append_entry_by_nid(Nid::SUBJECT_ALT_NAME, subject_alt_name.as_ref())
            .context("append subject alt name to x509 name builder")?;
    }
    x509_name
        .append_entry_by_nid(Nid::COMMONNAME, common_name.to_string().as_str())
        .context("append common name to x509 name builder")?;
    let x509_name = x509_name.build();

    let mut ca_cert_builder = X509::builder().context("create x509 (cert) builder")?;
    ca_cert_builder
        .set_version(2)
        .context("x509 cert builder: set version = 2")?;
    let serial_number = {
        let mut serial = BigNum::new().context("x509 cert builder: create big num (serial")?;
        serial
            .rand(159, MsbOption::MAYBE_ZERO, false)
            .context("x509 cert builder: randomise serial number (big num)")?;
        serial
            .to_asn1_integer()
            .context("x509 cert builder: convert serial to ASN1 integer")?
    };
    ca_cert_builder
        .set_serial_number(&serial_number)
        .context("x509 cert builder: set serial number")?;
    ca_cert_builder
        .set_subject_name(&x509_name)
        .context("x509 cert builder: set subject name")?;
    ca_cert_builder
        .set_issuer_name(&x509_name)
        .context("x509 cert builder: set issuer (self-signed")?;
    ca_cert_builder
        .set_pubkey(&privkey)
        .context("x509 cert builder: set public key using private key (ref)")?;
    let not_before =
        Asn1Time::days_from_now(0).context("x509 cert builder: create ASN1Time for today")?;
    ca_cert_builder
        .set_not_before(&not_before)
        .context("x509 cert builder: set not before to today")?;
    let not_after = Asn1Time::days_from_now(90)
        .context("x509 cert builder: create ASN1Time for 90 days in future")?;
    ca_cert_builder
        .set_not_after(&not_after)
        .context("x509 cert builder: set not after to 90 days in future")?;

    ca_cert_builder
        .append_extension(
            BasicConstraints::new()
                .critical()
                .ca()
                .build()
                .context("x509 cert builder: build basic constraints")?,
        )
        .context("x509 cert builder: add basic constraints as x509 extension")?;
    ca_cert_builder
        .append_extension(
            KeyUsage::new()
                .critical()
                .key_cert_sign()
                .crl_sign()
                .build()
                .context("x509 cert builder: create key usage")?,
        )
        .context("x509 cert builder: add key usage x509 extension")?;

    let subject_key_identifier = SubjectKeyIdentifier::new()
        .build(&ca_cert_builder.x509v3_context(None, None))
        .context("x509 cert builder: build subject key id")?;
    ca_cert_builder
        .append_extension(subject_key_identifier)
        .context("x509 cert builder: add subject key id x509 extension")?;

    ca_cert_builder
        .sign(&privkey, MessageDigest::sha256())
        .context("x509 cert builder: sign cert")?;

    let cert = ca_cert_builder.build();

    Ok((cert, privkey))
}
