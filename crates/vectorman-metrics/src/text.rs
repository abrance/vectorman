use std::collections::BTreeMap;

/// One Prometheus sample after text encoding (histograms already expanded).
#[derive(Debug, Clone, PartialEq)]
pub struct MetricSample {
    pub name: String,
    pub tags: BTreeMap<String, String>,
    pub value: f64,
}

pub fn parse_prom_text(text: &str) -> Result<Vec<MetricSample>, String> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match parse_sample_line(line) {
            Some(s) => out.push(s),
            None => return Err(format!("line {}: invalid metric sample: {line}", i + 1)),
        }
    }
    Ok(out)
}

fn parse_sample_line(line: &str) -> Option<MetricSample> {
    let (name, rest) = split_metric_name(line)?;
    let (tags, rest) = if rest.starts_with('{') {
        parse_label_block(rest)?
    } else {
        (BTreeMap::new(), rest)
    };
    let value_tok = rest.split_whitespace().next()?;
    let value: f64 = value_tok.parse().ok()?;
    Some(MetricSample { name, tags, value })
}

fn split_metric_name(s: &str) -> Option<(String, &str)> {
    let mut end = 0;
    for (idx, c) in s.char_indices() {
        if c == '{' || c.is_whitespace() {
            end = idx;
            break;
        }
        end = idx + c.len_utf8();
    }
    if end == 0 {
        return None;
    }
    Some((s[..end].to_string(), &s[end..]))
}

fn parse_label_block(s: &str) -> Option<(BTreeMap<String, String>, &str)> {
    let mut chars = s.char_indices().peekable();
    let (_, c) = chars.next()?;
    if c != '{' {
        return None;
    }
    let mut tags = BTreeMap::new();
    loop {
        skip_ws(&mut chars);
        let (_, c) = chars.peek().copied()?;
        if c == '}' {
            chars.next();
            let rest_at = chars.peek().map(|(i, _)| *i).unwrap_or(s.len());
            return Some((tags, &s[rest_at..]));
        }
        if c == ',' {
            chars.next();
            continue;
        }
        let key = parse_ident(&mut chars, s)?;
        skip_ws(&mut chars);
        if chars.next()?.1 != '=' {
            return None;
        }
        skip_ws(&mut chars);
        let value = parse_quoted(&mut chars)?;
        tags.insert(key, value);
    }
}

fn skip_ws(chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>) {
    while matches!(chars.peek(), Some((_, c)) if c.is_whitespace()) {
        chars.next();
    }
}

fn parse_ident(
    chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
    s: &str,
) -> Option<String> {
    let start = chars.peek()?.0;
    while matches!(chars.peek(), Some((_, c)) if c.is_ascii_alphanumeric() || *c == '_') {
        chars.next();
    }
    let end = chars.peek().map(|(i, _)| *i).unwrap_or(s.len());
    if end == start {
        return None;
    }
    Some(s[start..end].to_string())
}

fn parse_quoted(chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>) -> Option<String> {
    if chars.next()?.1 != '"' {
        return None;
    }
    let mut out = String::new();
    loop {
        let (_, c) = chars.next()?;
        match c {
            '"' => return Some(out),
            '\\' => {
                let (_, n) = chars.next()?;
                out.push(n);
            }
            other => out.push(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_labeled_and_bucket_lines() {
        let text = r#"
# TYPE vectorman_process_uptime_seconds gauge
vectorman_process_uptime_seconds{component="dataserver",instance="0.0.0.0:8081",data_id="self"} 1.5
vectorman_http_request_duration_seconds_bucket{method="GET",path="/health",le="+Inf"} 3
"#;
        let samples = parse_prom_text(text).unwrap();
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].name, "vectorman_process_uptime_seconds");
        assert_eq!(samples[0].tags.get("component").unwrap(), "dataserver");
        assert_eq!(samples[0].value, 1.5);
        assert_eq!(
            samples[1].name,
            "vectorman_http_request_duration_seconds_bucket"
        );
        assert_eq!(samples[1].tags.get("le").unwrap(), "+Inf");
        assert_eq!(samples[1].value, 3.0);
    }
}
