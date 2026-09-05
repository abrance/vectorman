//! 文件存储接口与本地目录实现。
//!
//! 对应设计文档 `FileStore`（Requirement 5）。路径为相对 POSIX 路径；
//! 实现必须拒绝 `..` 与绝对路径，错误码 `invalid_argument`。

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use dataplane_core::{validate_relative_path, DataplaneError, ErrorCode};
use serde::{Deserialize, Serialize};

/// content-type 缺省值。
pub const DEFAULT_CONTENT_TYPE: &str = "application/octet-stream";

/// 对象元信息文件后缀。
const META_SUFFIX: &str = ".dpmeta";

/// 对象元信息。`size` 以字节计，等于正文长度。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectMeta {
    pub content_type: String,
    pub size: u64,
}

/// 对象正文与元信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    pub meta: ObjectMeta,
    pub bytes: Vec<u8>,
}

/// 文件存储抽象。路径为相对 POSIX 路径。
#[async_trait]
pub trait FileStore: Send + Sync {
    /// 按相对路径写入字节内容，保存 content-type 与 size。
    async fn put(&self, path: &str, bytes: Vec<u8>, content_type: Option<&str>) -> Result<(), DataplaneError>;

    /// 读取对象完整正文与元信息。缺失返回 `not_found`。
    async fn get(&self, path: &str) -> Result<Object, DataplaneError>;

    /// 读取对象元信息（不要求返回正文）。缺失返回 `not_found`。
    async fn head(&self, path: &str) -> Result<ObjectMeta, DataplaneError>;

    /// 删除对象。缺失返回 `not_found`。
    async fn delete(&self, path: &str) -> Result<(), DataplaneError>;

    /// 返回指定前缀下的相对路径列表。
    async fn list(&self, prefix: &str) -> Result<Vec<String>, DataplaneError>;
}

/// 校验对象相对路径安全：拒绝绝对路径与 `..`。
pub fn validate_object_path(path: &str) -> Result<PathBuf, DataplaneError> {
    validate_relative_path(path)
}

/// 本地操作系统目录实现：一个对象 = 一个正文文件 + 一个元信息文件。
#[derive(Debug, Clone)]
pub struct DirFileStore {
    root: PathBuf,
}

impl DirFileStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// 元信息文件路径（正文路径 + 后缀）。
fn meta_path(object_path: &Path) -> PathBuf {
    PathBuf::from(format!("{}{}", object_path.display(), META_SUFFIX))
}

fn io_err(e: std::io::Error) -> DataplaneError {
    if e.kind() == std::io::ErrorKind::NotFound {
        DataplaneError::new(ErrorCode::NotFound, e.to_string())
    } else {
        DataplaneError::new(ErrorCode::QueryFailed, e.to_string())
    }
}

/// 读取元信息；缺失时按默认 content-type 与正文长度构造。
fn read_meta(abs: &Path, bytes_len: u64) -> Result<ObjectMeta, DataplaneError> {
    let mp = meta_path(abs);
    match std::fs::read_to_string(&mp) {
        Ok(s) => serde_json::from_str(&s)
            .map_err(|e| DataplaneError::new(ErrorCode::QueryFailed, format!("invalid meta file: {e}"))),
        Err(_) => Ok(ObjectMeta {
            content_type: DEFAULT_CONTENT_TYPE.to_string(),
            size: bytes_len,
        }),
    }
}

fn walk(base: &Path, dir: &Path, prefix: &str, out: &mut Vec<String>) -> Result<(), DataplaneError> {
    for entry in std::fs::read_dir(dir).map_err(io_err)? {
        let entry = entry.map_err(io_err)?;
        let path = entry.path();
        if path.is_dir() {
            walk(base, &path, prefix, out)?;
        } else {
            let rel = path.strip_prefix(base).expect("walk stays under base");
            let rel_str = rel.to_string_lossy().into_owned();
            if rel_str.ends_with(META_SUFFIX) {
                continue;
            }
            if rel_str.starts_with(prefix) {
                out.push(rel_str);
            }
        }
    }
    Ok(())
}

async fn blocking<F, R>(f: F) -> Result<R, DataplaneError>
where
    F: FnOnce() -> Result<R, DataplaneError> + Send + 'static,
    R: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| DataplaneError::new(ErrorCode::QueryFailed, format!("blocking task panicked: {e}")))?
}

#[async_trait]
impl FileStore for DirFileStore {
    async fn put(&self, path: &str, bytes: Vec<u8>, content_type: Option<&str>) -> Result<(), DataplaneError> {
        let root = self.root.clone();
        let path = path.to_string();
        let content_type = content_type.map(str::to_string);
        blocking(move || {
            let rel = validate_object_path(&path)?;
            if rel.as_os_str().is_empty() {
                return Err(DataplaneError::new(ErrorCode::InvalidArgument, "object path must not be empty"));
            }
            let abs = root.join(rel);
            if let Some(parent) = abs.parent() {
                std::fs::create_dir_all(parent).map_err(io_err)?;
            }
            let ct = content_type.unwrap_or_else(|| DEFAULT_CONTENT_TYPE.to_string());
            std::fs::write(&abs, &bytes).map_err(io_err)?;
            let meta = ObjectMeta {
                content_type: ct,
                size: bytes.len() as u64,
            };
            let mp = meta_path(&abs);
            std::fs::write(&mp, serde_json::to_string(&meta).map_err(|e| {
                DataplaneError::new(ErrorCode::QueryFailed, format!("serialize meta: {e}"))
            })?)
            .map_err(io_err)?;
            Ok(())
        })
        .await
    }

    async fn get(&self, path: &str) -> Result<Object, DataplaneError> {
        let root = self.root.clone();
        let path = path.to_string();
        blocking(move || {
            let abs = {
                let rel = validate_object_path(&path)?;
                if rel.as_os_str().is_empty() {
                    return Err(DataplaneError::new(ErrorCode::InvalidArgument, "object path must not be empty"));
                }
                root.join(rel)
            };
            let bytes = std::fs::read(&abs).map_err(io_err)?;
            let meta = read_meta(&abs, bytes.len() as u64)?;
            Ok(Object { meta, bytes })
        })
        .await
    }

    async fn head(&self, path: &str) -> Result<ObjectMeta, DataplaneError> {
        let root = self.root.clone();
        let path = path.to_string();
        blocking(move || {
            let abs = {
                let rel = validate_object_path(&path)?;
                if rel.as_os_str().is_empty() {
                    return Err(DataplaneError::new(ErrorCode::InvalidArgument, "object path must not be empty"));
                }
                root.join(rel)
            };
            let len = std::fs::metadata(&abs).map_err(io_err)?.len();
            read_meta(&abs, len)
        })
        .await
    }

    async fn delete(&self, path: &str) -> Result<(), DataplaneError> {
        let root = self.root.clone();
        let path = path.to_string();
        blocking(move || {
            let abs = {
                let rel = validate_object_path(&path)?;
                if rel.as_os_str().is_empty() {
                    return Err(DataplaneError::new(ErrorCode::InvalidArgument, "object path must not be empty"));
                }
                root.join(rel)
            };
            match std::fs::remove_file(&abs) {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(DataplaneError::new(ErrorCode::NotFound, "object not found"));
                }
                Err(e) => return Err(io_err(e)),
            }
            let mp = meta_path(&abs);
            let _ = std::fs::remove_file(mp);
            Ok(())
        })
        .await
    }

    async fn list(&self, prefix: &str) -> Result<Vec<String>, DataplaneError> {
        let root = self.root.clone();
        let prefix = prefix.to_string();
        blocking(move || {
            if !root.exists() {
                return Ok(Vec::new());
            }
            let mut out = Vec::new();
            walk(&root, &root, &prefix, &mut out)?;
            Ok(out)
        })
        .await
    }
}