use std::fmt;
use std::path::{Component, Path, PathBuf};

use crate::error::{DataplaneError, ErrorCode};

/// 数据路径解析结果：各引擎的落盘位置。
#[derive(Debug, Clone)]
pub struct DataPaths {
    /// 用户配置的数据路径（文件或目录）。
    pub root: PathBuf,
    /// 是否是多引擎共享的目录模式。
    pub is_directory: bool,
    /// 文件引擎根目录。
    pub files: PathBuf,
    /// 时序引擎目录。
    pub ts: PathBuf,
    /// 日志索引目录。
    pub logs: PathBuf,
    /// KV 数据库文件。
    pub kv: PathBuf,
    /// SQL 数据库文件。
    pub sql: PathBuf,
}

/// 判断路径是否包含 `..` 组件。
pub fn has_dotdot(path: &Path) -> bool {
    path.components().any(|c| matches!(c, Component::ParentDir))
}

/// 判断路径是否为绝对路径。
pub fn is_absolute_poison(path: &Path) -> bool {
    path.is_absolute()
}

/// 解析数据路径并根据根目录约定构造各引擎子路径。
///
/// - 目录模式：为五类引擎分别构造子路径；不存在时创建目录。
/// - 单文件模式：仅允许该文件作为 sqlite 库，`sql` 指向该文件，其余字段为空并返回
///   `config_invalid`（由调用方决定是否仅用 sqlite）。
pub fn resolve_data_paths(data_path: &str) -> Result<DataPaths, DataplaneError> {
    let root = PathBuf::from(data_path);
    if root.as_os_str().is_empty() {
        return Err(DataplaneError::config_invalid("data_path must not be empty"));
    }
    if root.exists() && root.is_file() {
        // 单文件模式：仅允许 sqlite。
        let sql = root.clone();
        return Ok(DataPaths {
            root,
            is_directory: false,
            files: PathBuf::new(),
            ts: PathBuf::new(),
            logs: PathBuf::new(),
            kv: PathBuf::new(),
            sql,
        });
    }

    let dir = if root.exists() {
        if !root.is_dir() {
            return Err(DataplaneError::config_invalid(format!(
                "data_path {data_path:?} exists but is not a directory"
            )));
        }
        root.clone()
    } else {
        std::fs::create_dir_all(&root).map_err(|e| {
            DataplaneError::config_invalid(format!("cannot create data_path {data_path:?}: {e}"))
        })?;
        root.clone()
    };
    let files = dir.join("files");
    let ts = dir.join("ts");
    let logs = dir.join("logs");
    let kv = dir.join("kv.redb");
    let sql = dir.join("sql.sqlite");
    Ok(DataPaths {
        root: dir,
        is_directory: true,
        files,
        ts,
        logs,
        kv,
        sql,
    })
}

impl DataPaths {
    /// 为多引擎目录模式创建子目录；单文件模式返回 `config_invalid`。
    pub fn ensure_dirs(&self) -> Result<(), DataplaneError> {
        if !self.is_directory {
            return Err(DataplaneError::config_invalid(
                "single-file data_path only supports sqlite; directory mode required for other engines",
            ));
        }
        for d in [&self.files, &self.ts, &self.logs] {
            std::fs::create_dir_all(d).map_err(|e| {
                DataplaneError::config_invalid(format!("cannot create dir {}: {e}", d.display()))
            })?;
        }
        if let Some(parent) = self.kv.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                DataplaneError::config_invalid(format!("cannot create dir {}: {e}", parent.display()))
            })?;
        }
        if let Some(parent) = self.sql.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                DataplaneError::config_invalid(format!("cannot create dir {}: {e}", parent.display()))
            })?;
        }
        Ok(())
    }
}

impl fmt::Display for DataPaths {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "root={} directory={}",
            self.root.display(),
            self.is_directory
        )
    }
}

/// 校验相对路径安全：拒绝绝对路径与 `..`。
pub fn validate_relative_path(path: &str) -> Result<PathBuf, DataplaneError> {
    let p = Path::new(path);
    if p.is_absolute() {
        return Err(DataplaneError::new(
            ErrorCode::InvalidArgument,
            format!("absolute path not allowed: {path}"),
        ));
    }
    if has_dotdot(p) {
        return Err(DataplaneError::new(
            ErrorCode::InvalidArgument,
            format!("parent traversal not allowed: {path}"),
        ));
    }
    Ok(p.to_path_buf())
}