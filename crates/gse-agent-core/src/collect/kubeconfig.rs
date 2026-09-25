//! K8s 访问凭证解析：优先 in-cluster ServiceAccount，否则读 kubeconfig。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;

/// 解析得到的 apiserver 基址、bearer token 与集群 CA。
///
/// `ca_pem` 是集群自签 CA（in-cluster 的 `ca.crt`，或 kubeconfig 的
/// `certificate-authority` / `certificate-authority-data`）。k3s 这类集群的 apiserver
/// 用的是**集群自签 CA**，不带上它请求会在 TLS 校验阶段失败（而默认根证书是公有 CA）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct K8sCredential {
    pub base_url: String,
    pub token: String,
    pub ca_pem: Option<String>,
}

const SA_DIR: &str = "/var/run/secrets/kubernetes.io/serviceaccount";

/// 解析凭证：`kubeconfig` 非空则读该路径，空则先试 in-cluster 再回退默认 kubeconfig。
pub fn resolve(kubeconfig: &str) -> Result<K8sCredential, String> {
    resolve_in(SA_DIR, kubeconfig)
}

/// `resolve` 的可测版本：`sa_dir` 为 in-cluster ServiceAccount 目录。
pub fn resolve_in(sa_dir: &str, kubeconfig: &str) -> Result<K8sCredential, String> {
    if !kubeconfig.is_empty() {
        let text = std::fs::read_to_string(kubeconfig)
            .map_err(|e| format!("read kubeconfig {kubeconfig}: {e}"))?;
        return parse_kubeconfig_with_dir(&text, Path::new(kubeconfig).parent());
    }
    let dir = PathBuf::from(sa_dir);
    if let Ok(token) = std::fs::read_to_string(dir.join("token")) {
        let token = token.trim().to_string();
        if !token.is_empty() {
            return Ok(K8sCredential {
                base_url: "https://kubernetes.default.svc".to_string(),
                token,
                ca_pem: read_optional(&dir.join("ca.crt")),
            });
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let default = format!("{home}/.kube/config");
    let text =
        std::fs::read_to_string(&default).map_err(|e| format!("read kubeconfig {default}: {e}"))?;
    parse_kubeconfig_with_dir(&text, Path::new(&default).parent())
}

/// 读一个可选文件：不存在或为空都返回 `None`（CA 缺失不算致命错误，
/// 到 TLS 阶段会以清晰的握手失败暴露）。
fn read_optional(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// 解析 kubeconfig 文本：取首个 `server:` 与 `token:`（常见单 context 形式）。
pub fn parse_kubeconfig(text: &str) -> Result<K8sCredential, String> {
    parse_kubeconfig_with_dir(text, None)
}

/// 同上，另支持 CA：`certificate-authority`（路径，可相对 kubeconfig 所在目录）
/// 或 `certificate-authority-data`（base64 内联 PEM）。
pub fn parse_kubeconfig_with_dir(text: &str, dir: Option<&Path>) -> Result<K8sCredential, String> {
    let mut server: Option<String> = None;
    let mut token: Option<String> = None;
    let mut ca_path: Option<String> = None;
    let mut ca_data: Option<String> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if server.is_none() {
            if let Some(v) = line.strip_prefix("server:") {
                server = Some(unquote(v.trim()));
            }
        }
        if token.is_none() {
            if let Some(v) = line.strip_prefix("token:") {
                token = Some(unquote(v.trim()));
            }
        }
        if ca_data.is_none() {
            if let Some(v) = line.strip_prefix("certificate-authority-data:") {
                ca_data = Some(unquote(v.trim()));
            }
        }
        if ca_path.is_none() {
            if let Some(v) = line.strip_prefix("certificate-authority:") {
                ca_path = Some(unquote(v.trim()));
            }
        }
    }
    let base_url = server.ok_or_else(|| "kubeconfig missing server".to_string())?;
    let ca_pem = match (ca_data, ca_path) {
        // 内联优先：k3s 写出的 kubeconfig 用它。
        (Some(data), _) => Some(decode_base64_pem(&data)?),
        (None, Some(path)) => {
            let p = PathBuf::from(&path);
            let p = if p.is_absolute() {
                p
            } else {
                dir.map(|d| d.join(&p)).unwrap_or(p)
            };
            read_optional(&p)
        }
        (None, None) => None,
    };
    Ok(K8sCredential {
        base_url: base_url.trim_end_matches('/').to_string(),
        token: token.unwrap_or_default(),
        ca_pem,
    })
}

fn decode_base64_pem(data: &str) -> Result<String, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| format!("decode certificate-authority-data: {e}"))?;
    String::from_utf8(bytes).map_err(|e| format!("certificate-authority-data 不是 UTF-8: {e}"))
}

/// 用给定 CA（PEM）构建 rustls 配置：**只信任该 CA**。
///
/// 仅用于 apiserver 客户端；其它流量（上行到 dataserver / GSE）不受影响。
pub fn tls_config(ca_pem: &str) -> Result<Arc<rustls::ClientConfig>, String> {
    let mut roots = rustls::RootCertStore::empty();
    let mut added = 0usize;
    for cert in rustls_pemfile::certs(&mut ca_pem.as_bytes()) {
        let cert = cert.map_err(|e| format!("parse cluster CA: {e}"))?;
        roots
            .add(cert)
            .map_err(|e| format!("add cluster CA: {e}"))?;
        added += 1;
    }
    if added == 0 {
        return Err("cluster CA 里没有证书".to_string());
    }
    Ok(Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    ))
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    v.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(v)
        .to_string()
}

/// 测试用根证书与叶证书（叶证书 SAN 含 `127.0.0.1`，有效期到 2126 年）。
///
/// 形态与真实集群一致：**CA 签发 apiserver 叶证书** —— 自签的 CA 证书不能当服务端证书
/// （webpki 会以 `CaUsedAsEndEntity` 拒绝），所以这里必须是一对。
#[cfg(test)]
pub(crate) mod test_ca {
    /// 集群 CA（`ca.crt` / `certificate-authority-data` 的内容）。
    pub const CERT: &str = "-----BEGIN CERTIFICATE-----
MIIBnjCCAUWgAwIBAgIUWAMyQjkHRf9wIaCtWqnV9eAAu34wCgYIKoZIzj0EAwIw
HDEaMBgGA1UEAwwRdmVjdG9ybWFuLXRlc3QtY2EwIBcNMjYwOTI1MTQ0ODUyWhgP
MjEyNjA5MDExNDQ4NTJaMBwxGjAYBgNVBAMMEXZlY3Rvcm1hbi10ZXN0LWNhMFkw
EwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE8FsMaTzLxb6ijq16oTufZXb8Q3KPYKsV
5c8v7I1kiYlzrSEVdZpVEiWN/i0gChBAWn3CfJxIAPn91b8NkHkZp6NjMGEwHQYD
VR0OBBYEFMaR5U2wiXk/1tWwbuUv+rQZ63jNMB8GA1UdIwQYMBaAFMaR5U2wiXk/
1tWwbuUv+rQZ63jNMA8GA1UdEwEB/wQFMAMBAf8wDgYDVR0PAQH/BAQDAgIEMAoG
CCqGSM49BAMCA0cAMEQCIBTNQdbfBBPw1R2wvNX1TCm7weTItBLsi6P/XkiurIcy
AiBXsASAOl2v8LFVI+TM4PtHGenSJUB0/dN8AxX1mWLbvA==
-----END CERTIFICATE-----";

    /// apiserver 叶证书。
    pub const LEAF_CERT: &str = "-----BEGIN CERTIFICATE-----
MIIB3zCCAYSgAwIBAgIUaDgpZ+jnmODPp86ypcgkz4PVxWEwCgYIKoZIzj0EAwIw
HDEaMBgGA1UEAwwRdmVjdG9ybWFuLXRlc3QtY2EwIBcNMjYwOTI1MTQ0ODUyWhgP
MjEyNjA5MDExNDQ4NTJaMB4xHDAaBgNVBAMME3ZlY3Rvcm1hbi10ZXN0LWxlYWYw
WTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAARu/XeE7nno5ceGswzXUanrS9vbRHD9
FsRSIj6Xb8yxR7Pd7q+NsumN/tPGqXOJPysV13VNN1Su/66ZDnc/5IuAo4GfMIGc
MCcGA1UdEQQgMB6HBH8AAAGCFmt1YmVybmV0ZXMuZGVmYXVsdC5zdmMwDAYDVR0T
AQH/BAIwADAOBgNVHQ8BAf8EBAMCB4AwEwYDVR0lBAwwCgYIKwYBBQUHAwEwHQYD
VR0OBBYEFI7XcodzyOSUJKg3OLmoC9lsi5zFMB8GA1UdIwQYMBaAFMaR5U2wiXk/
1tWwbuUv+rQZ63jNMAoGCCqGSM49BAMCA0kAMEYCIQC8VCM4axVzESbHJ4m+K/oW
3deOyysL8rlAvOsgfVCnSgIhAP8JmCcEpYZDVElIchK3xIhkUkG3Bte4JTReTGNg
TtP5
-----END CERTIFICATE-----";

    /// 叶证书私钥。
    pub const LEAF_KEY: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgW1kudAV15w5882PE
+ApFPx1j6YXobYVtoGpq7/gg+7ahRANCAARu/XeE7nno5ceGswzXUanrS9vbRHD9
FsRSIj6Xb8yxR7Pd7q+NsumN/tPGqXOJPysV13VNN1Su/66ZDnc/5IuA
-----END PRIVATE KEY-----";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_server_and_token() {
        let text = r#"
apiVersion: v1
clusters:
- cluster:
    server: "https://10.0.0.1:6443"
  name: c
users:
- user:
    token: abc123
  name: u
"#;
        let cred = parse_kubeconfig(text).expect("parse");
        assert_eq!(cred.base_url, "https://10.0.0.1:6443");
        assert_eq!(cred.token, "abc123");
        assert!(cred.ca_pem.is_none());
    }

    #[test]
    fn unquoted_and_missing_token() {
        let cred = parse_kubeconfig("server: https://api.local:6443\n").expect("parse");
        assert_eq!(cred.base_url, "https://api.local:6443");
        assert!(cred.token.is_empty());
    }

    #[test]
    fn missing_server_errors() {
        assert!(parse_kubeconfig("token: x\n").is_err());
    }

    /// in-cluster：有 `ca.crt` 就带上，没有则 `None`（不致命）。
    #[test]
    fn reads_sa_token_with_and_without_ca() {
        let dir = std::env::temp_dir().join(format!("vm-sa-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("token"), "sa-token\n").unwrap();
        let cred = resolve_in(dir.to_str().unwrap(), "").expect("resolve");
        assert_eq!(cred.base_url, "https://kubernetes.default.svc");
        assert_eq!(cred.token, "sa-token");
        assert!(cred.ca_pem.is_none(), "没有 ca.crt 时不算错");

        std::fs::write(dir.join("ca.crt"), test_ca::CERT).unwrap();
        let cred = resolve_in(dir.to_str().unwrap(), "").expect("resolve");
        assert_eq!(cred.ca_pem.as_deref(), Some(test_ca::CERT));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// k3s 写出的 kubeconfig 用 `certificate-authority-data`（base64 内联 PEM）。
    #[test]
    fn parses_inline_ca_data() {
        let b64 = base64::engine::general_purpose::STANDARD.encode(test_ca::CERT.as_bytes());
        let text = format!(
            "server: https://127.0.0.1:6443\n    certificate-authority-data: {b64}\ntoken: t\n"
        );
        let cred = parse_kubeconfig(&text).expect("parse");
        assert_eq!(cred.ca_pem.as_deref(), Some(test_ca::CERT));
    }

    #[test]
    fn parses_ca_path_relative_to_kubeconfig_dir() {
        let dir = std::env::temp_dir().join(format!("vm-kc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ca.crt"), test_ca::CERT).unwrap();
        let text = "server: https://api.local:6443\n    certificate-authority: ca.crt\ntoken: t\n";
        let cred = parse_kubeconfig_with_dir(text, Some(&dir)).expect("parse");
        assert_eq!(cred.ca_pem.as_deref(), Some(test_ca::CERT));
        // 相对路径 + 没有 dir：读不到就当没有 CA，不报错
        let cred = parse_kubeconfig(text).expect("parse");
        assert!(cred.ca_pem.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tls_config_rejects_garbage_and_accepts_ca() {
        assert!(tls_config("not a pem").is_err());
        assert!(tls_config("").is_err());
        assert!(tls_config(test_ca::CERT).is_ok());
    }
}
