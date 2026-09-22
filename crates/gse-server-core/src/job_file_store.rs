//! GSE Server 作业临时文件：按 `file_id` 隔离目录，不进 sqlite。

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use gse_proto::GseError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::hashutil::hex_encode;
use crate::session::now_micros;

static FILE_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobFileMeta {
    pub file_id: String,
    pub file_name: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub created_at: String,
}

#[derive(Clone)]
pub struct JobFileStore {
    root: PathBuf,
}

pub struct JobFileWriter {
    store: JobFileStore,
    file_id: String,
    file_name: String,
    created_at: String,
    tmp_path: PathBuf,
    file: File,
    hasher: Sha256,
    size: u64,
}

impl JobFileStore {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, GseError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root).map_err(|e| GseError::new("rpc_error", e.to_string()))?;
        Ok(Self { root })
    }

    pub fn validate_id(file_id: &str) -> Result<(), GseError> {
        if file_id.is_empty()
            || file_id.contains("..")
            || file_id.contains('/')
            || file_id.contains('\\')
            || !file_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
        {
            return Err(GseError::new(
                "invalid_argument",
                format!("illegal file_id: {file_id}"),
            ));
        }
        Ok(())
    }

    pub fn new_file_id() -> String {
        let seq = FILE_SEQ.fetch_add(1, Ordering::Relaxed);
        format!("file-{}-{}", now_micros(), seq)
    }

    pub fn put(&self, file_name: &str, data: &[u8]) -> Result<JobFileMeta, GseError> {
        self.put_with_id(&Self::new_file_id(), file_name, data)
    }

    pub fn put_with_id(
        &self,
        file_id: &str,
        file_name: &str,
        data: &[u8],
    ) -> Result<JobFileMeta, GseError> {
        let mut w = self.begin_write(file_id, file_name)?;
        w.write_all(data)?;
        w.finish()
    }

    pub fn begin_write(&self, file_id: &str, file_name: &str) -> Result<JobFileWriter, GseError> {
        Self::validate_id(file_id)?;
        let dir = self.root.join(file_id);
        if dir.exists() {
            fs::remove_dir_all(&dir).map_err(|e| GseError::new("rpc_error", e.to_string()))?;
        }
        fs::create_dir_all(&dir).map_err(|e| GseError::new("rpc_error", e.to_string()))?;
        let tmp_path = dir.join("content.tmp");
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp_path)
            .map_err(|e| GseError::new("rpc_error", e.to_string()))?;
        Ok(JobFileWriter {
            store: self.clone(),
            file_id: file_id.to_string(),
            file_name: file_name.to_string(),
            created_at: now_micros().to_string(),
            tmp_path,
            file,
            hasher: Sha256::new(),
            size: 0,
        })
    }

    fn dir(&self, file_id: &str) -> Result<PathBuf, GseError> {
        Self::validate_id(file_id)?;
        Ok(self.root.join(file_id))
    }

    pub fn head(&self, file_id: &str) -> Result<JobFileMeta, GseError> {
        let path = self.dir(file_id)?.join("meta.json");
        let raw = fs::read(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                GseError::new("not_found", format!("file {file_id} not found"))
            } else {
                GseError::new("rpc_error", e.to_string())
            }
        })?;
        serde_json::from_slice(&raw).map_err(|e| GseError::new("rpc_error", e.to_string()))
    }

    pub fn content_path(&self, file_id: &str) -> Result<PathBuf, GseError> {
        let path = self.dir(file_id)?.join("content");
        if !path.is_file() {
            return Err(GseError::new(
                "not_found",
                format!("file {file_id} not found"),
            ));
        }
        Ok(path)
    }

    pub fn get(&self, file_id: &str) -> Result<(JobFileMeta, Vec<u8>), GseError> {
        let meta = self.head(file_id)?;
        let data = fs::read(self.content_path(file_id)?)
            .map_err(|e| GseError::new("rpc_error", e.to_string()))?;
        Ok((meta, data))
    }

    pub fn delete(&self, file_id: &str) -> Result<(), GseError> {
        let dir = self.dir(file_id)?;
        if !dir.exists() {
            return Err(GseError::new(
                "not_found",
                format!("file {file_id} not found"),
            ));
        }
        fs::remove_dir_all(dir).map_err(|e| GseError::new("rpc_error", e.to_string()))?;
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<JobFileMeta>, GseError> {
        let mut out = Vec::new();
        let rd = match fs::read_dir(&self.root) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(GseError::new("rpc_error", e.to_string())),
        };
        for ent in rd {
            let ent = ent.map_err(|e| GseError::new("rpc_error", e.to_string()))?;
            if !ent.path().is_dir() {
                continue;
            }
            let id = ent.file_name().to_string_lossy().into_owned();
            if let Ok(meta) = self.head(&id) {
                out.push(meta);
            }
        }
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(out)
    }

    pub fn list_unexpired(
        &self,
        now_micros: i64,
        retain_secs: u64,
    ) -> Result<Vec<JobFileMeta>, GseError> {
        let cutoff = now_micros.saturating_sub((retain_secs as i64).saturating_mul(1_000_000));
        Ok(self
            .list()?
            .into_iter()
            .filter(|m| m.created_at.parse::<i64>().unwrap_or(0) >= cutoff)
            .collect())
    }

    pub fn delete_expired(&self, now_micros: i64, retain_secs: u64) -> Result<usize, GseError> {
        let cutoff = now_micros.saturating_sub((retain_secs as i64).saturating_mul(1_000_000));
        let mut n = 0;
        for meta in self.list()? {
            let created = meta.created_at.parse::<i64>().unwrap_or(0);
            if created < cutoff {
                self.delete(&meta.file_id)?;
                n += 1;
            }
        }
        Ok(n)
    }
}

impl JobFileWriter {
    pub fn write_all(&mut self, data: &[u8]) -> Result<(), GseError> {
        self.file
            .write_all(data)
            .map_err(|e| GseError::new("rpc_error", e.to_string()))?;
        self.hasher.update(data);
        self.size += data.len() as u64;
        Ok(())
    }

    pub fn finish(self) -> Result<JobFileMeta, GseError> {
        let JobFileWriter {
            store,
            file_id,
            file_name,
            created_at,
            tmp_path,
            file,
            hasher,
            size,
        } = self;
        drop(file);
        let dir = store.root.join(&file_id);
        let final_path = dir.join("content");
        fs::rename(&tmp_path, &final_path)
            .map_err(|e| GseError::new("rpc_error", e.to_string()))?;
        let meta = JobFileMeta {
            file_id,
            file_name,
            size_bytes: size,
            sha256: hex_encode(&hasher.finalize()),
            created_at,
        };
        let raw = serde_json::to_vec_pretty(&meta)
            .map_err(|e| GseError::new("rpc_error", e.to_string()))?;
        fs::write(dir.join("meta.json"), raw)
            .map_err(|e| GseError::new("rpc_error", e.to_string()))?;
        Ok(meta)
    }

    pub fn abort(self) {
        let dir = self.store.root.join(&self.file_id);
        drop(self.file);
        let _ = fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hashutil::sha256_hex;

    fn tmp_store(name: &str) -> JobFileStore {
        let dir = std::env::temp_dir().join(format!(
            "gse-jobfiles-{}-{}-{}",
            std::process::id(),
            name,
            now_micros()
        ));
        JobFileStore::open(dir).expect("open")
    }

    #[test]
    fn rejects_illegal_file_id() {
        for id in ["", "../x", "a/b", "a\\b", "id with space"] {
            assert!(JobFileStore::validate_id(id).is_err(), "{id}");
        }
        assert!(JobFileStore::validate_id("file-job-1-0").is_ok());
    }

    #[test]
    fn put_get_delete_roundtrip() {
        let store = tmp_store("round");
        let meta = store.put("app.log", b"hello").expect("put");
        assert_eq!(meta.file_name, "app.log");
        assert_eq!(meta.size_bytes, 5);
        assert_eq!(meta.sha256, sha256_hex(b"hello"));
        let (head, data) = store.get(&meta.file_id).expect("get");
        assert_eq!(head, meta);
        assert_eq!(data, b"hello");
        store.delete(&meta.file_id).expect("delete");
        assert_eq!(store.get(&meta.file_id).unwrap_err().code, "not_found");
    }

    #[test]
    fn jobs_do_not_overwrite_each_other() {
        let store = tmp_store("iso");
        let a = store.put_with_id("file-job-a", "a.bin", b"aaa").expect("a");
        let b = store.put_with_id("file-job-b", "b.bin", b"bbb").expect("b");
        assert_eq!(store.get(&a.file_id).unwrap().1, b"aaa");
        assert_eq!(store.get(&b.file_id).unwrap().1, b"bbb");
    }

    #[test]
    fn expired_listing() {
        let store = tmp_store("exp");
        let meta = store.put("x", b"x").expect("put");
        let created: i64 = meta.created_at.parse().unwrap();
        let live = store.list_unexpired(created + 1_000, 10).expect("live");
        assert_eq!(live.len(), 1);
        let gone = store.list_unexpired(created + 20_000_000, 1).expect("gone");
        assert!(gone.is_empty());
        let n = store.delete_expired(created + 20_000_000, 1).expect("del");
        assert_eq!(n, 1);
        assert_eq!(store.head(&meta.file_id).unwrap_err().code, "not_found");
    }
}
