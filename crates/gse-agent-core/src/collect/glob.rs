//! 单层 glob：支持 `*` 与 `?`，`*` 不跨目录分隔符。

use std::path::{Path, PathBuf};

/// 单层 glob 匹配：`*` 匹配任意非 `/` 字符序列，`?` 匹配单个非 `/` 字符。
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = text.chars().collect();
    match_at(&pat, &txt)
}

fn match_at(pat: &[char], txt: &[char]) -> bool {
    if pat.is_empty() {
        return txt.is_empty();
    }
    match pat[0] {
        '*' => {
            // 匹配 0 个或多个非 `/` 字符。
            if match_at(&pat[1..], txt) {
                return true;
            }
            let mut i = 0;
            while i < txt.len() && txt[i] != '/' {
                i += 1;
                if match_at(&pat[1..], &txt[i..]) {
                    return true;
                }
            }
            false
        }
        '?' => !txt.is_empty() && txt[0] != '/' && match_at(&pat[1..], &txt[1..]),
        c => !txt.is_empty() && txt[0] == c && match_at(&pat[1..], &txt[1..]),
    }
}

/// 按模式发现文件：模式最后一个 `/` 之前为目录，之后为文件名 glob。
///
/// 目录不存在或不可读返回空列表；结果按路径排序，保证顺序稳定。
pub fn find_files(pattern: &str) -> Vec<PathBuf> {
    let Some((dir, file_pat)) = split_parent(pattern) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_type().map(|t| t.is_file()).unwrap_or(false)
                && e.file_name()
                    .to_str()
                    .map(|n| glob_match(&file_pat, n))
                    .unwrap_or(false)
        })
        .map(|e| e.path())
        .collect();
    out.sort();
    out
}

fn split_parent(pattern: &str) -> Option<(PathBuf, String)> {
    let path = Path::new(pattern);
    let file_pat = path.file_name()?.to_str()?.to_string();
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty())?;
    Some((dir.to_path_buf(), file_pat))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn star_does_not_cross_directory_separator() {
        assert!(glob_match("/var/log/*.log", "/var/log/app.log"));
        assert!(glob_match("/var/log/app*.log", "/var/log/app-1.log"));
        assert!(!glob_match("/var/log/*.log", "/var/log/nested/app.log"));
        assert!(glob_match("/var/log/*/*.log", "/var/log/nested/app.log"));
    }

    #[test]
    fn question_matches_single_char() {
        assert!(glob_match("app?.log", "app1.log"));
        assert!(!glob_match("app?.log", "app12.log"));
        assert!(!glob_match("app?.log", "app/.log"));
    }

    #[test]
    fn pod_name_patterns() {
        assert!(glob_match("nginx-*", "nginx-abc"));
        assert!(glob_match("nginx-*", "nginx-"));
        assert!(!glob_match("nginx-*", "web-abc"));
        assert!(glob_match("*", "anything"));
    }

    #[test]
    fn find_files_matches_single_layer_only() {
        let dir = std::env::temp_dir().join(format!("gse-glob-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        std::fs::write(dir.join("app-1.log"), "a").unwrap();
        std::fs::write(dir.join("app-2.log"), "b").unwrap();
        std::fs::write(dir.join("other.txt"), "c").unwrap();
        std::fs::write(dir.join("nested/app-3.log"), "d").unwrap();

        let pattern = format!("{}/*.log", dir.to_string_lossy());
        let found = find_files(&pattern);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found.iter().all(|p| p.is_file()));

        let missing = format!("{}/nope/*.log", dir.to_string_lossy());
        assert!(find_files(&missing).is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
