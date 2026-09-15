//! 文件日志采集：单层 glob 匹配、按 inode+offset 跟随、起点标记、清洗与攒批。

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use super::clean::{detect_level, CleanedLine, Cleaner};
use super::config::CollectorConfig;
use super::envelope::{LogsRecord, DATA_TYPE_LOGS};
use super::glob::find_files;
use super::CollectShared;

/// 一个文件里读出的完整行及其起始字节偏移。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    pub text: String,
    pub offset: u64,
}

/// 单个文件的跟随状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileState {
    pub inode: u64,
    pub offset: u64,
}

/// `head` 模式起始偏移：跳过前 `n-1` 个换行；`n<=1` 从 0 开始。
pub fn head_start_offset(text: &str, n: u64) -> u64 {
    if n <= 1 {
        return 0;
    }
    let mut seen = 0u64;
    for (idx, b) in text.bytes().enumerate() {
        if b == b'\n' {
            seen += 1;
            if seen == n - 1 {
                return (idx + 1) as u64;
            }
        }
    }
    text.len() as u64
}

/// 返回文件最后 `n` 行及它们在文件中的起始偏移。
pub fn tail_lines(text: &str, n: u64) -> Vec<LogLine> {
    if n == 0 {
        return Vec::new();
    }
    let mut starts: Vec<usize> = Vec::new();
    if !text.is_empty() {
        starts.push(0);
    }
    for (idx, b) in text.bytes().enumerate() {
        if b == b'\n' && idx + 1 < text.len() {
            starts.push(idx + 1);
        }
    }
    if starts.is_empty() {
        return Vec::new();
    }
    let take = starts.len().min(n as usize);
    (starts.len() - take..starts.len())
        .map(|i| {
            let start = starts[i];
            let end = starts.get(i + 1).copied().unwrap_or(text.len());
            LogLine {
                text: trim_newline(&text[start..end]).to_string(),
                offset: start as u64,
            }
        })
        .collect()
}

fn trim_newline(s: &str) -> &str {
    s.strip_suffix('\n')
        .and_then(|s| s.strip_suffix('\r').or(Some(s)))
        .unwrap_or(s)
}

/// 从 `offset` 读新增内容，按 `\n` 切行；不完整末行不消费。
pub fn read_new_lines(text: &str, from: u64) -> (Vec<LogLine>, u64) {
    let start = from as usize;
    if start > text.len() {
        return (Vec::new(), text.len() as u64);
    }
    let slice = &text[start..];
    let mut lines = Vec::new();
    let mut consumed = start;
    let bytes = slice.as_bytes();
    let mut line_start = 0usize;
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'\n' {
            lines.push(LogLine {
                text: trim_newline(&slice[line_start..=i]).to_string(),
                offset: (start + line_start) as u64,
            });
            line_start = i + 1;
        }
    }
    consumed += line_start;
    (lines, consumed as u64)
}

/// 依据起点标记为文件计算初始状态；`tail n>=1` 时同时返回应补发的历史行。
pub fn initial_state(path: &Path, cfg: &CollectorConfig, inode: u64) -> (FileState, Vec<LogLine>) {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    if cfg.start_mode() == "head" {
        let offset = head_start_offset(&text, cfg.start_n);
        (FileState { inode, offset }, Vec::new())
    } else {
        let history = tail_lines(&text, cfg.start_n);
        (
            FileState {
                inode,
                offset: text.len() as u64,
            },
            history,
        )
    }
}

fn read_file(path: &Path) -> Option<String> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = String::new();
    f.seek(SeekFrom::Start(0)).ok()?;
    f.read_to_string(&mut buf).ok()?;
    Some(buf)
}

/// 轮询一个路径模式，返回新增行；不存在路径向 stderr 输出一行并跳过。
pub fn poll_pattern(
    pattern: &str,
    cfg: &CollectorConfig,
    states: &mut HashMap<PathBuf, FileState>,
) -> Vec<(PathBuf, LogLine)> {
    let mut out = Vec::new();
    for path in find_files(pattern) {
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        let inode = meta.ino();
        let state = match states.get(&path) {
            Some(s) if s.inode == inode => *s,
            _ => {
                let (state, history) = initial_state(&path, cfg, inode);
                states.insert(path.clone(), state);
                for line in history {
                    out.push((path.clone(), line));
                }
                state
            }
        };
        let Some(text) = read_file(&path) else {
            continue;
        };
        let (lines, next) = read_new_lines(&text, state.offset);
        if let Some(entry) = states.get_mut(&path) {
            entry.offset = next;
        }
        for line in lines {
            out.push((path.clone(), line));
        }
    }
    // 报告当前不存在的模式（仅当整个模式无匹配文件）。
    if find_files(pattern).is_empty() {
        eprintln!("gse-agent: log path missing: {pattern}");
    }
    out
}

fn to_record(
    agent_id: &str,
    path: &Path,
    inode: u64,
    line: &LogLine,
    cleaned: CleanedLine,
) -> LogsRecord {
    let source = path.to_string_lossy().into_owned();
    LogsRecord {
        record_id: format!("{agent_id}:{source}:{inode}:{}", line.offset),
        timestamp: super::now_micros(),
        level: detect_level(&line.text),
        message: cleaned.message,
        source,
        labels: cleaned.labels,
    }
}

/// 采集任务：按上报间隔轮询所有路径，攒批后放入 `data_type=logs`。
pub async fn run(shared: Arc<CollectShared>, item_id: String, cfg: CollectorConfig) {
    let cleaner = Cleaner::new(&cfg.clean);
    let mut states: HashMap<PathBuf, FileState> = HashMap::new();
    let interval = Duration::from_secs(cfg.flush_interval_secs.max(1));
    let batch_max = cfg.batch_max_records.max(1);
    let mut pending: Vec<Value> = Vec::new();

    loop {
        tokio::time::sleep(interval).await;
        for pattern in &cfg.path_patterns {
            for (path, line) in poll_pattern(pattern, &cfg, &mut states) {
                let Some(cleaned) = cleaner.clean(&line.text) else {
                    continue;
                };
                let inode = states.get(&path).map(|s| s.inode).unwrap_or(0);
                let rec = to_record(&shared.agent_id, &path, inode, &line, cleaned);
                pending.push(serde_json::to_value(rec).unwrap_or(Value::Null));
            }
        }
        while pending.len() >= batch_max {
            let chunk: Vec<Value> = pending.drain(..batch_max).collect();
            shared.push(DATA_TYPE_LOGS, &item_id, chunk).await;
        }
        if !pending.is_empty() {
            let chunk = std::mem::take(&mut pending);
            shared.push(DATA_TYPE_LOGS, &item_id, chunk).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(mode: &str, n: u64) -> CollectorConfig {
        CollectorConfig {
            start_mode: mode.to_string(),
            start_n: n,
            ..Default::default()
        }
    }

    #[test]
    fn head_start_offset_skips_lines() {
        let text = "a\nb\nc\nd\n";
        assert_eq!(head_start_offset(text, 0), 0);
        assert_eq!(head_start_offset(text, 1), 0);
        assert_eq!(head_start_offset(text, 2), 2);
        assert_eq!(head_start_offset(text, 3), 4);
        assert_eq!(head_start_offset(text, 99), text.len() as u64);
    }

    #[test]
    fn tail_lines_returns_last_n() {
        let text = "a\nb\nc\nd\n";
        let got = tail_lines(text, 2);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].text, "c");
        assert_eq!(got[0].offset, 4);
        assert_eq!(got[1].text, "d");
        assert_eq!(got[1].offset, 6);
        assert!(tail_lines(text, 0).is_empty());
        assert_eq!(tail_lines(text, 9).len(), 4);
    }

    #[test]
    fn read_new_lines_keeps_partial_tail() {
        let text = "line1\nline2\npart";
        let (lines, next) = read_new_lines(text, 0);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "line1");
        assert_eq!(lines[1].text, "line2");
        assert_eq!(next, 12);

        // 补全后从 slice 起点读到完整行。
        let (lines, next) = read_new_lines("line1\nline2\npartial\n", 0);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[2].text, "partial");
        assert_eq!(next, 20);
    }

    #[test]
    fn poll_pattern_tracks_append_and_gap() {
        let dir = std::env::temp_dir().join(format!("gse-logfile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("app.log");
        std::fs::write(&file, "one\ntwo\n").unwrap();

        // tail 0：不读已有行。
        let pattern = format!("{}/*.log", dir.to_string_lossy());
        let mut states = HashMap::new();
        let first = poll_pattern(&pattern, &cfg("tail", 0), &mut states);
        assert!(first.is_empty(), "{first:?}");

        std::fs::write(&file, "one\ntwo\nthree\n").unwrap();
        let second = poll_pattern(&pattern, &cfg("tail", 0), &mut states);
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].1.text, "three");

        // 已有状态后不重复。
        let third = poll_pattern(&pattern, &cfg("tail", 0), &mut states);
        assert!(third.is_empty(), "{third:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn poll_pattern_head_reads_from_n() {
        let dir = std::env::temp_dir().join(format!("gse-logfile-head-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("app.log");
        std::fs::write(&file, "one\ntwo\nthree\n").unwrap();

        let pattern = format!("{}/*.log", dir.to_string_lossy());
        let mut states = HashMap::new();
        let lines = poll_pattern(&pattern, &cfg("head", 2), &mut states);
        let texts: Vec<&str> = lines.iter().map(|(_, l)| l.text.as_str()).collect();
        assert_eq!(texts, vec!["two", "three"]);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
