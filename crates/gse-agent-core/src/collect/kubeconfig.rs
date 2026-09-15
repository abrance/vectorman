//! K8s 访问凭证解析：优先 in-cluster ServiceAccount，否则读 kubeconfig。

use std::path::PathBuf;

/// 解析得到的 apiserver 基址与 bearer token。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct K8sCredential {
    pub base_url: String,
    pub token: String,
}

const SA_DIR: &str = "/var/run/secrets/kubernetes.io/serviceaccount";

/// 解析凭证：`kubeconfig` 非空则读该路径，空则先试 in-cluster 再回退默认 kubeconfig。
pub fn resolve(kubeconfig: &str) -> Result<K8sCredential, String> {
    if !kubeconfig.is_empty() {
        return parse_kubeconfig(&std::fs::read_to_string(kubeconfig).map_err(|e| {
            format!("read kubeconfig {kubeconfig}: {e}")
        })?);
    }
    let sa_token = PathBuf::from(SA_DIR).join("token");
    if let Ok(token) = std::fs::read_to_string(&sa_token) {
        let token = token.trim().to_string();
        if !token.is_empty() {
            return Ok(K8sCredential {
                base_url: "https://kubernetes.default.svc".to_string(),
                token,
            });
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let default = format!("{home}/.kube/config");
    parse_kubeconfig(&std::fs::read_to_string(&default).map_err(|e| format!("read kubeconfig {default}: {e}"))?)
}

/// 解析 kubeconfig 文本：取首个 `server:` 与 `token:`（常见单 context 形式）。
pub fn parse_kubeconfig(text: &str) -> Result<K8sCredential, String> {
    let mut server: Option<String> = None;
    let mut token: Option<String> = None;
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
    }
    let base_url = server.ok_or_else(|| "kubeconfig missing server".to_string())?;
    let token = token.unwrap_or_default();
    Ok(K8sCredential {
        base_url: base_url.trim_end_matches('/').to_string(),
        token,
    })
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    v.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(v)
        .to_string()
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
}
