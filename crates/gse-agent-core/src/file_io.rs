//! Agent 文件分块读写：供 Server 中转文件作业调用。
//!
//! 不占用脚本作业并发信号量。写入走旁路文件 `{path}.gse-tmp-{job_id}`，
//! 全部块成功且校验一致后再 rename 就位。

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use geminio::app::Error;
use geminio::Bytes;
use gse_proto::{FileReadReply, FileReadReq, FileWriteReply, FileWriteReq};
use sha2::{Digest, Sha256};

const STALE_SECS: u64 = 600;

#[derive(Clone, Default)]
pub struct FileIo {
    writes: Arc<Mutex<HashMap<String, WriteSession>>>,
}

struct WriteSession {
    path: PathBuf,
    tmp_path: PathBuf,
    hasher: Sha256,
    next_offset: u64,
    last_write: Instant,
}

impl FileIo {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn handle_read(&self, req: &Bytes) -> Result<Bytes, Error> {
        let reply = match serde_json::from_slice::<FileReadReq>(req) {
            Ok(parsed) => read_file(&parsed),
            Err(e) => err_read("", format!("invalid_argument: {e}")),
        };
        encode(&reply)
    }

    pub async fn handle_write(&self, req: &Bytes) -> Result<Bytes, Error> {
        let reply = match serde_json::from_slice::<FileWriteReq>(req) {
            Ok(parsed) => self.write_file(&parsed),
            Err(e) => err_write("", format!("invalid_argument: {e}")),
        };
        encode(&reply)
    }

    fn write_file(&self, req: &FileWriteReq) -> FileWriteReply {
        self.purge_stale();
        if let Err(code) = validate_abs_path(&req.path) {
            return err_write(&req.job_id, code);
        }
        let dest = PathBuf::from(&req.path);
        let data = match STANDARD.decode(req.data_b64.as_bytes()) {
            Ok(d) => d,
            Err(_) => return err_write(&req.job_id, "invalid_argument: bad data_b64"),
        };
        if sha256_hex(&data) != req.chunk_sha256
            && !(data.is_empty() && req.chunk_sha256.is_empty())
        {
            return err_write(&req.job_id, "checksum_mismatch");
        }

        let mut guard = match self.writes.lock() {
            Ok(g) => g,
            Err(_) => return err_write(&req.job_id, "rpc_error: lock poisoned"),
        };

        if req.offset == 0 {
            if dest.exists() {
                if dest.is_dir() {
                    return err_write(&req.job_id, "not_a_file");
                }
                return err_write(&req.job_id, "already_exists");
            }
            if let Some(parent) = dest.parent() {
                if !parent.as_os_str().is_empty() {
                    if let Err(e) = fs::create_dir_all(parent) {
                        return err_write(&req.job_id, io_code(&e));
                    }
                }
            }
            if let Some(old) = guard.remove(&req.job_id) {
                let _ = fs::remove_file(&old.tmp_path);
            }
            let tmp_path = sidecar_path(&dest, &req.job_id);
            if let Err(e) = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp_path)
            {
                return err_write(&req.job_id, io_code(&e));
            }
            guard.insert(
                req.job_id.clone(),
                WriteSession {
                    path: dest.clone(),
                    tmp_path,
                    hasher: Sha256::new(),
                    next_offset: 0,
                    last_write: Instant::now(),
                },
            );
        }

        let Some(sess) = guard.get_mut(&req.job_id) else {
            return err_write(&req.job_id, "invalid_argument: write not started");
        };
        if sess.path != dest {
            return err_write(&req.job_id, "invalid_argument: path changed");
        }
        if sess.next_offset != req.offset {
            return err_write(
                &req.job_id,
                format!(
                    "invalid_argument: expected offset {} got {}",
                    sess.next_offset, req.offset
                ),
            );
        }

        if !data.is_empty() {
            if let Err(e) = (|| {
                let mut f = OpenOptions::new().append(true).open(&sess.tmp_path)?;
                f.write_all(&data)?;
                Ok::<(), std::io::Error>(())
            })() {
                let tmp = sess.tmp_path.clone();
                guard.remove(&req.job_id);
                let _ = fs::remove_file(tmp);
                return err_write(&req.job_id, io_code(&e));
            }
            sess.hasher.update(&data);
            sess.next_offset += data.len() as u64;
            sess.last_write = Instant::now();
        }

        if !req.eof {
            return FileWriteReply {
                job_id: req.job_id.clone(),
                written: data.len() as u64,
                eof: false,
                file_sha256: None,
                error: None,
            };
        }

        let digest = hex_encode(&sess.hasher.clone().finalize());
        if let Some(expected) = req.file_sha256.as_deref() {
            if expected != digest {
                let tmp = sess.tmp_path.clone();
                guard.remove(&req.job_id);
                let _ = fs::remove_file(tmp);
                return err_write(&req.job_id, "checksum_mismatch");
            }
        }
        if dest.exists() {
            let tmp = sess.tmp_path.clone();
            guard.remove(&req.job_id);
            let _ = fs::remove_file(tmp);
            return err_write(&req.job_id, "already_exists");
        }
        let tmp = sess.tmp_path.clone();
        guard.remove(&req.job_id);
        if let Err(e) = fs::rename(&tmp, &dest) {
            let _ = fs::remove_file(&tmp);
            return err_write(&req.job_id, io_code(&e));
        }
        FileWriteReply {
            job_id: req.job_id.clone(),
            written: data.len() as u64,
            eof: true,
            file_sha256: Some(digest),
            error: None,
        }
    }

    fn purge_stale(&self) {
        let Ok(mut guard) = self.writes.lock() else {
            return;
        };
        let now = Instant::now();
        let stale: Vec<String> = guard
            .iter()
            .filter(|(_, s)| now.duration_since(s.last_write) > Duration::from_secs(STALE_SECS))
            .map(|(k, _)| k.clone())
            .collect();
        for id in stale {
            if let Some(s) = guard.remove(&id) {
                let _ = fs::remove_file(s.tmp_path);
            }
        }
    }
}

fn read_file(req: &FileReadReq) -> FileReadReply {
    if let Err(code) = validate_abs_path(&req.path) {
        return err_read(&req.job_id, code);
    }
    let path = Path::new(&req.path);
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) => return err_read(&req.job_id, io_code(&e)),
    };
    if !meta.is_file() {
        return err_read(&req.job_id, "not_a_file");
    }
    let size = meta.len();
    if req.offset == 0 && req.max_bytes > 0 && size > req.max_bytes {
        return err_read(&req.job_id, "file_too_large");
    }
    if req.offset > size {
        return err_read(&req.job_id, "invalid_argument: offset past end");
    }
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(e) => return err_read(&req.job_id, io_code(&e)),
    };
    if let Err(e) = file.seek(SeekFrom::Start(req.offset)) {
        return err_read(&req.job_id, io_code(&e));
    }
    let want = req.length.min(size.saturating_sub(req.offset)) as usize;
    let mut buf = vec![0u8; want];
    let n = match file.read(&mut buf) {
        Ok(n) => n,
        Err(e) => return err_read(&req.job_id, io_code(&e)),
    };
    buf.truncate(n);
    let eof = req.offset + n as u64 >= size;
    let file_sha256 = if eof {
        match hash_file(path) {
            Ok(h) => Some(h),
            Err(e) => return err_read(&req.job_id, io_code(&e)),
        }
    } else {
        None
    };
    FileReadReply {
        job_id: req.job_id.clone(),
        size,
        offset: req.offset,
        eof,
        data_b64: STANDARD.encode(&buf),
        chunk_sha256: sha256_hex(&buf),
        file_sha256,
        error: None,
    }
}

fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex_encode(&hasher.finalize()))
}

fn validate_abs_path(path: &str) -> Result<(), String> {
    if path.trim().is_empty() {
        return Err("invalid_argument: empty path".to_string());
    }
    let p = Path::new(path);
    if !p.is_absolute() {
        return Err("invalid_argument: path must be absolute".to_string());
    }
    Ok(())
}

fn sidecar_path(dest: &Path, job_id: &str) -> PathBuf {
    let mut s = dest.as_os_str().to_os_string();
    s.push(format!(".gse-tmp-{job_id}"));
    PathBuf::from(s)
}

fn io_code(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => "not_found".to_string(),
        std::io::ErrorKind::PermissionDenied => "permission_denied".to_string(),
        std::io::ErrorKind::AlreadyExists => "already_exists".to_string(),
        _ => format!("rpc_error: {e}"),
    }
}

fn sha256_hex(data: &[u8]) -> String {
    hex_encode(&Sha256::digest(data))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

fn err_read(job_id: &str, error: impl Into<String>) -> FileReadReply {
    FileReadReply {
        job_id: job_id.to_string(),
        size: 0,
        offset: 0,
        eof: false,
        data_b64: String::new(),
        chunk_sha256: String::new(),
        file_sha256: None,
        error: Some(error.into()),
    }
}

fn err_write(job_id: &str, error: impl Into<String>) -> FileWriteReply {
    FileWriteReply {
        job_id: job_id.to_string(),
        written: 0,
        eof: false,
        file_sha256: None,
        error: Some(error.into()),
    }
}

fn encode<T: serde::Serialize>(value: &T) -> Result<Bytes, Error> {
    let body = serde_json::to_vec(value).map_err(|e| Error::Remote(e.to_string()))?;
    Ok(Bytes::from(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "gse-fileio-{}-{}-{}",
            std::process::id(),
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn read_req(path: &str, offset: u64, length: u64, max_bytes: u64) -> FileReadReq {
        FileReadReq {
            job_id: "job-1".to_string(),
            path: path.to_string(),
            offset,
            length,
            max_bytes,
        }
    }

    #[test]
    fn relative_path_rejected() {
        let reply = read_file(&read_req("rel/a", 0, 8, 64));
        assert_eq!(
            reply.error.as_deref(),
            Some("invalid_argument: path must be absolute")
        );
    }

    #[test]
    fn missing_file_is_not_found() {
        let dir = tmp_dir("missing");
        let path = dir.join("nope.bin");
        let reply = read_file(&read_req(path.to_str().unwrap(), 0, 8, 64));
        assert_eq!(reply.error.as_deref(), Some("not_found"));
    }

    #[test]
    fn directory_is_not_a_file() {
        let dir = tmp_dir("dir");
        let reply = read_file(&read_req(dir.to_str().unwrap(), 0, 8, 64));
        assert_eq!(reply.error.as_deref(), Some("not_a_file"));
    }

    #[test]
    fn read_chunks_and_eof_sha256() {
        let dir = tmp_dir("read");
        let path = dir.join("src.bin");
        fs::write(&path, b"abcdefgh").unwrap();
        let p = path.to_str().unwrap();
        let first = read_file(&read_req(p, 0, 3, 64));
        assert!(first.error.is_none(), "{first:?}");
        assert!(!first.eof);
        assert_eq!(STANDARD.decode(&first.data_b64).unwrap(), b"abc");
        let last = read_file(&read_req(p, 3, 16, 64));
        assert!(last.eof);
        assert_eq!(STANDARD.decode(&last.data_b64).unwrap(), b"defgh");
        assert_eq!(
            last.file_sha256.as_deref(),
            Some(sha256_hex(b"abcdefgh").as_str())
        );
    }

    #[test]
    fn too_large_on_first_block() {
        let dir = tmp_dir("large");
        let path = dir.join("src.bin");
        fs::write(&path, b"abcdefgh").unwrap();
        let reply = read_file(&read_req(path.to_str().unwrap(), 0, 8, 4));
        assert_eq!(reply.error.as_deref(), Some("file_too_large"));
        assert!(reply.data_b64.is_empty());
    }

    #[test]
    fn write_creates_parents_and_refuses_existing() {
        let io = FileIo::new();
        let dir = tmp_dir("write");
        let dest = dir.join("nested/out.bin");
        let p = dest.to_str().unwrap().to_string();
        let data = b"hello";
        let chunk = sha256_hex(data);
        let file_hash = sha256_hex(data);
        let reply = io.write_file(&FileWriteReq {
            job_id: "job-w".to_string(),
            path: p.clone(),
            offset: 0,
            eof: true,
            data_b64: STANDARD.encode(data),
            chunk_sha256: chunk,
            file_sha256: Some(file_hash.clone()),
        });
        assert!(reply.error.is_none(), "{reply:?}");
        assert_eq!(fs::read(&dest).unwrap(), data);

        let again = io.write_file(&FileWriteReq {
            job_id: "job-w2".to_string(),
            path: p,
            offset: 0,
            eof: true,
            data_b64: STANDARD.encode(data),
            chunk_sha256: sha256_hex(data),
            file_sha256: Some(file_hash),
        });
        assert_eq!(again.error.as_deref(), Some("already_exists"));
        assert_eq!(fs::read(&dest).unwrap(), data);
    }

    #[test]
    fn bad_chunk_checksum_rejected() {
        let io = FileIo::new();
        let dir = tmp_dir("bad-chunk");
        let dest = dir.join("out.bin");
        let reply = io.write_file(&FileWriteReq {
            job_id: "job-b".to_string(),
            path: dest.to_str().unwrap().to_string(),
            offset: 0,
            eof: true,
            data_b64: STANDARD.encode(b"hello"),
            chunk_sha256: "deadbeef".to_string(),
            file_sha256: None,
        });
        assert_eq!(reply.error.as_deref(), Some("checksum_mismatch"));
        assert!(!dest.exists());
    }
}
