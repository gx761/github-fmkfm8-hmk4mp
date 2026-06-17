//! CA 生成、动态证书签发与 rustls 集成（HTTPS MITM）。
//!
//! 对应 whistle 的 `lib/https`。提供：
//! - [`CertAuthority`]：加载或生成根 CA，按 host 动态签发叶子证书并缓存，
//!   产出 rustls 服务端配置（用于对客户端伪装目标站点）；
//! - [`client_config`]：用于连接真实上游的 rustls 客户端配置（调试代理默认不校验上游证书）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair, KeyUsagePurpose,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ClientConfig, ServerConfig};
use tracing::info;

/// TLS 相关错误。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("证书生成错误: {0}")]
    Rcgen(#[from] rcgen::Error),
    #[error("rustls 错误: {0}")]
    Rustls(#[from] rustls::Error),
}

type Result<T> = std::result::Result<T, Error>;

/// 确保进程级 rustls 默认加密 provider 已安装（ring）。
fn ensure_provider() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// 根证书颁发机构：持有 CA，动态签发并缓存各 host 的服务端配置。
pub struct CertAuthority {
    issuer_key: KeyPair,
    issuer_cert: rcgen::Certificate,
    ca_pem: String,
    cache: Mutex<HashMap<String, Arc<ServerConfig>>>,
}

impl CertAuthority {
    /// 从数据目录加载 CA；不存在则生成并持久化（`ca-cert.pem` / `ca-key.pem`）。
    pub fn load_or_generate(dir: &Path) -> Result<Self> {
        ensure_provider();
        std::fs::create_dir_all(dir)?;
        let cert_path = dir.join("ca-cert.pem");
        let key_path = dir.join("ca-key.pem");

        if cert_path.exists() && key_path.exists() {
            let cert_pem = std::fs::read_to_string(&cert_path)?;
            let key_pem = std::fs::read_to_string(&key_path)?;
            let issuer_key = KeyPair::from_pem(&key_pem)?;
            let params = CertificateParams::from_ca_cert_pem(&cert_pem)?;
            let issuer_cert = params.self_signed(&issuer_key)?;
            info!(path = %cert_path.display(), "已加载根 CA");
            Ok(Self::new(issuer_key, issuer_cert, cert_pem))
        } else {
            let (issuer_key, issuer_cert) = generate_ca()?;
            let cert_pem = issuer_cert.pem();
            std::fs::write(&cert_path, &cert_pem)?;
            std::fs::write(&key_path, issuer_key.serialize_pem())?;
            info!(path = %cert_path.display(), "已生成根 CA（请在系统/浏览器中信任它）");
            Ok(Self::new(issuer_key, issuer_cert, cert_pem))
        }
    }

    fn new(issuer_key: KeyPair, issuer_cert: rcgen::Certificate, ca_pem: String) -> Self {
        Self {
            issuer_key,
            issuer_cert,
            ca_pem,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// 根 CA 证书的 PEM（用于导出/安装）。
    pub fn ca_pem(&self) -> &str {
        &self.ca_pem
    }

    /// 获取（或签发并缓存）某 host 的 rustls 服务端配置。
    pub fn server_config_for(&self, host: &str) -> Result<Arc<ServerConfig>> {
        if let Some(cfg) = self.cache.lock().unwrap().get(host) {
            return Ok(cfg.clone());
        }
        let cfg = Arc::new(self.build_server_config(host)?);
        self.cache
            .lock()
            .unwrap()
            .insert(host.to_string(), cfg.clone());
        Ok(cfg)
    }

    fn build_server_config(&self, host: &str) -> Result<ServerConfig> {
        let mut params = CertificateParams::new(vec![host.to_string()])?;
        params
            .distinguished_name
            .push(DnType::CommonName, host.to_string());
        let leaf_key = KeyPair::generate()?;
        let leaf_cert = params.signed_by(&leaf_key, &self.issuer_cert, &self.issuer_key)?;

        let chain = vec![
            CertificateDer::from(leaf_cert.der().to_vec()),
            CertificateDer::from(self.issuer_cert.der().to_vec()),
        ];
        let key = PrivateKeyDer::try_from(leaf_key.serialize_der())
            .map_err(|e| rustls::Error::General(e.to_string()))?;

        let mut cfg = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(chain, key)?;
        // 同时提供 h2 与 http/1.1，由客户端 ALPN 选择（中间人对客户端侧）。
        cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        Ok(cfg)
    }
}

/// 生成自签名根 CA。
fn generate_ca() -> Result<(KeyPair, rcgen::Certificate)> {
    let mut params = CertificateParams::new(Vec::<String>::new())?;
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "whistle-rs Root CA");
    dn.push(DnType::OrganizationName, "whistle-rs");
    params.distinguished_name = dn;
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let key = KeyPair::generate()?;
    let cert = params.self_signed(&key)?;
    Ok((key, cert))
}

/// 默认数据目录：`~/.whistle-rs`（取不到 HOME 时用当前目录）。
pub fn default_data_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".whistle-rs")
}

mod danger {
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::{DigitallySignedStruct, SignatureScheme};

    /// 接受任意上游证书的校验器（调试代理用途）。
    #[derive(Debug)]
    pub struct NoVerify;

    impl ServerCertVerifier for NoVerify {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp_response: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn verify_tls13_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            use SignatureScheme::*;
            vec![
                RSA_PKCS1_SHA256,
                RSA_PKCS1_SHA384,
                RSA_PKCS1_SHA512,
                ECDSA_NISTP256_SHA256,
                ECDSA_NISTP384_SHA384,
                RSA_PSS_SHA256,
                RSA_PSS_SHA384,
                RSA_PSS_SHA512,
                ED25519,
            ]
        }
    }
}

/// 连接真实上游的客户端配置（默认不校验上游证书，适合调试代理）。
pub fn client_config() -> Arc<ClientConfig> {
    ensure_provider();
    static CFG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    CFG.get_or_init(|| {
        let mut cfg = ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(danger::NoVerify))
            .with_no_client_auth();
        cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        Arc::new(cfg)
    })
    .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_and_signs() {
        let dir = std::env::temp_dir().join(format!("whistle-rs-test-{}", std::process::id()));
        let ca = CertAuthority::load_or_generate(&dir).unwrap();
        assert!(ca.ca_pem().contains("BEGIN CERTIFICATE"));
        // 同一 host 命中缓存返回同一 Arc。
        let a = ca.server_config_for("example.com").unwrap();
        let b = ca.server_config_for("example.com").unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        // 不同 host 各自签发。
        let c = ca.server_config_for("other.com").unwrap();
        assert!(!Arc::ptr_eq(&a, &c));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reload_uses_existing_ca() {
        let dir = std::env::temp_dir().join(format!("whistle-rs-reload-{}", std::process::id()));
        let pem1 = CertAuthority::load_or_generate(&dir)
            .unwrap()
            .ca_pem()
            .to_string();
        let pem2 = CertAuthority::load_or_generate(&dir)
            .unwrap()
            .ca_pem()
            .to_string();
        assert_eq!(pem1, pem2, "重启应复用已持久化的 CA");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
