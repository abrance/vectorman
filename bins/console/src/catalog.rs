use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use url::Url;

pub const MAX_APPS: usize = 100;
pub const NAME_MAX: usize = 64;
pub const URL_MAX: usize = 2048;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct App {
    pub app_id: String,
    pub name: String,
    pub url: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct AppsFile {
    apps: Vec<App>,
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("{0}")]
    InvalidName(String),
    #[error("{0}")]
    InvalidUrl(String),
    #[error("app name already exists")]
    NameConflict,
    #[error("app catalog limit is {MAX_APPS}")]
    LimitExceeded,
    #[error("app not found")]
    NotFound,
    #[error("{0}")]
    Persist(String),
    #[error("{0}")]
    Corrupt(String),
}

impl CatalogError {
    pub fn code(&self) -> &'static str {
        match self {
            CatalogError::InvalidName(_) => "invalid_name",
            CatalogError::InvalidUrl(_) => "invalid_url",
            CatalogError::NameConflict => "name_conflict",
            CatalogError::LimitExceeded => "limit_exceeded",
            CatalogError::NotFound => "not_found",
            CatalogError::Persist(_) => "persist_failed",
            CatalogError::Corrupt(_) => "corrupt",
        }
    }
}

struct Inner {
    apps: Vec<App>,
    seq: u64,
}

pub struct Catalog {
    data_file: PathBuf,
    inner: Mutex<Inner>,
}

impl Catalog {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, CatalogError> {
        let data_file = path.as_ref().to_path_buf();
        if !data_file.exists() {
            return Ok(Self {
                data_file,
                inner: Mutex::new(Inner {
                    apps: Vec::new(),
                    seq: 0,
                }),
            });
        }
        let raw = std::fs::read_to_string(&data_file).map_err(|e| {
            CatalogError::Corrupt(format!("cannot read {}: {e}", data_file.display()))
        })?;
        let file: AppsFile = serde_json::from_str(&raw).map_err(|e| {
            CatalogError::Corrupt(format!("cannot parse {}: {e}", data_file.display()))
        })?;
        let seq = file
            .apps
            .iter()
            .filter_map(|a| parse_seq(&a.app_id))
            .max()
            .unwrap_or(0);
        Ok(Self {
            data_file,
            inner: Mutex::new(Inner {
                apps: file.apps,
                seq,
            }),
        })
    }

    pub async fn list(&self) -> Vec<App> {
        let inner = self.inner.lock().await;
        let mut apps = inner.apps.clone();
        apps.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then(a.app_id.cmp(&b.app_id))
        });
        apps
    }

    pub async fn create(&self, name: String, url: String) -> Result<App, CatalogError> {
        let name = validate_name(&name)?;
        validate_url(&url)?;
        let mut inner = self.inner.lock().await;
        if inner.apps.len() >= MAX_APPS {
            return Err(CatalogError::LimitExceeded);
        }
        if inner.apps.iter().any(|a| a.name == name) {
            return Err(CatalogError::NameConflict);
        }
        inner.seq += 1;
        let ts = now_micros_string();
        let app = App {
            app_id: format!("app-{ts}-{}", inner.seq),
            name,
            url,
            created_at: ts.clone(),
            updated_at: ts,
        };
        inner.apps.push(app.clone());
        if let Err(e) = persist(&self.data_file, &inner.apps) {
            inner.apps.pop();
            inner.seq -= 1;
            return Err(e);
        }
        Ok(app)
    }

    pub async fn update(
        &self,
        app_id: &str,
        name: String,
        url: String,
    ) -> Result<App, CatalogError> {
        let name = validate_name(&name)?;
        validate_url(&url)?;
        let mut inner = self.inner.lock().await;
        let idx = inner
            .apps
            .iter()
            .position(|a| a.app_id == app_id)
            .ok_or(CatalogError::NotFound)?;
        if inner
            .apps
            .iter()
            .enumerate()
            .any(|(i, a)| i != idx && a.name == name)
        {
            return Err(CatalogError::NameConflict);
        }
        let snapshot = inner.apps[idx].clone();
        inner.apps[idx].name = name;
        inner.apps[idx].url = url;
        inner.apps[idx].updated_at = now_micros_string();
        let updated = inner.apps[idx].clone();
        if let Err(e) = persist(&self.data_file, &inner.apps) {
            inner.apps[idx] = snapshot;
            return Err(e);
        }
        Ok(updated)
    }

    pub async fn delete(&self, app_id: &str) -> Result<(), CatalogError> {
        let mut inner = self.inner.lock().await;
        let idx = inner
            .apps
            .iter()
            .position(|a| a.app_id == app_id)
            .ok_or(CatalogError::NotFound)?;
        let snapshot = inner.apps[idx].clone();
        inner.apps.remove(idx);
        if let Err(e) = persist(&self.data_file, &inner.apps) {
            inner.apps.insert(idx, snapshot);
            return Err(e);
        }
        Ok(())
    }
}

fn persist(path: &Path, apps: &[App]) -> Result<(), CatalogError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| {
                CatalogError::Persist(format!("cannot create {}: {e}", parent.display()))
            })?;
        }
    }
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_vec_pretty(&AppsFile {
        apps: apps.to_vec(),
    })
    .map_err(|e| CatalogError::Persist(format!("serialize: {e}")))?;
    std::fs::write(&tmp, body)
        .map_err(|e| CatalogError::Persist(format!("write {}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        CatalogError::Persist(format!(
            "rename {} -> {}: {e}",
            tmp.display(),
            path.display()
        ))
    })
}

fn validate_name(name: &str) -> Result<String, CatalogError> {
    let name = name.trim().to_string();
    if name.is_empty() || name.chars().count() > NAME_MAX {
        return Err(CatalogError::InvalidName(format!(
            "name must be 1 to {NAME_MAX} characters"
        )));
    }
    Ok(name)
}

fn validate_url(raw: &str) -> Result<(), CatalogError> {
    if raw.is_empty() || raw.len() > URL_MAX {
        return Err(CatalogError::InvalidUrl(format!(
            "url must be 1 to {URL_MAX} bytes"
        )));
    }
    let parsed = Url::parse(raw)
        .map_err(|e| CatalogError::InvalidUrl(format!("url is not a valid URL: {e}")))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(CatalogError::InvalidUrl(
            "url scheme must be http or https".to_string(),
        ));
    }
    Ok(())
}

fn now_micros_string() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

fn parse_seq(app_id: &str) -> Option<u64> {
    let rest = app_id.strip_prefix("app-")?;
    let seq = rest.rsplit_once('-')?.1;
    seq.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_file(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("console-apps-{}-{name}.json", std::process::id()))
    }

    #[tokio::test]
    async fn create_list_roundtrip_after_reopen() {
        let path = tmp_file("roundtrip");
        let _ = std::fs::remove_file(&path);
        let cat = Catalog::open(&path).unwrap();
        let created = cat
            .create("GSE".into(), "http://127.0.0.1:7101".into())
            .await
            .unwrap();
        assert!(created.app_id.starts_with("app-"));
        drop(cat);
        let cat2 = Catalog::open(&path).unwrap();
        let listed = cat2.list().await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "GSE");
        assert_eq!(listed[0].url, "http://127.0.0.1:7101");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn rejects_empty_name_and_bad_scheme() {
        let path = tmp_file("validate");
        let _ = std::fs::remove_file(&path);
        let cat = Catalog::open(&path).unwrap();
        let empty = cat
            .create("  ".into(), "http://x".into())
            .await
            .unwrap_err();
        assert_eq!(empty.code(), "invalid_name");
        let bad = cat.create("ok".into(), "ftp://x".into()).await.unwrap_err();
        assert_eq!(bad.code(), "invalid_url");
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn name_conflict_and_not_found() {
        let path = tmp_file("conflict");
        let _ = std::fs::remove_file(&path);
        let cat = Catalog::open(&path).unwrap();
        cat.create("A".into(), "https://example.com".into())
            .await
            .unwrap();
        let err = cat
            .create("A".into(), "https://example.com/2".into())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "name_conflict");
        let nf = cat
            .update("app-nope-1", "B".into(), "https://example.com".into())
            .await
            .unwrap_err();
        assert_eq!(nf.code(), "not_found");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn limit_exceeded() {
        let path = tmp_file("limit");
        let _ = std::fs::remove_file(&path);
        let cat = Catalog::open(&path).unwrap();
        {
            let mut inner = cat.inner.lock().await;
            for i in 0..MAX_APPS {
                inner.apps.push(App {
                    app_id: format!("app-1-{i}"),
                    name: format!("n{i}"),
                    url: "http://127.0.0.1".into(),
                    created_at: "1".into(),
                    updated_at: "1".into(),
                });
            }
        }
        let err = cat
            .create("extra".into(), "http://127.0.0.1".into())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "limit_exceeded");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn corrupt_file_fails_open() {
        let path = tmp_file("corrupt");
        std::fs::write(&path, "{not json").unwrap();
        let err = match Catalog::open(&path) {
            Ok(_) => panic!("expected corrupt catalog"),
            Err(e) => e,
        };
        assert_eq!(err.code(), "corrupt");
        let _ = std::fs::remove_file(&path);
    }
}
