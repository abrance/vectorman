//! Prometheus HTTP 查询（dataserver 的 `/api/v1/query`，VM 的查询口同协议）。

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::Value;

use crate::http;
use crate::util::wait_until;

#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub metric: BTreeMap<String, String>,
    /// 数值（matrix 时取最后一个点）。
    pub value: f64,
    /// 秒级时间戳（Prom 原样返回）。
    pub timestamp: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Response {
    pub result_type: String,
    pub samples: Vec<Sample>,
    /// 原始 `data.result`，需要断言「查不到」而非「解析失败」时用。
    pub raw: Value,
}

impl Response {
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// 所有样本值求和（只有一个标量时就是它的值）。
    pub fn value_sum(&self) -> f64 {
        self.samples.iter().map(|s| s.value).sum()
    }

    pub fn first(&self) -> &Sample {
        self.samples
            .first()
            .unwrap_or_else(|| panic!("期望至少一个样本，实际为空: raw={}", self.raw))
    }

    /// 断言恰好一个样本，且值等于 `expected`（浮点容差 1e-9）。
    pub fn assert_single_scalar(&self, expected: f64) {
        assert_eq!(
            self.samples.len(),
            1,
            "期望 1 个样本，实际 {}: raw={}",
            self.samples.len(),
            self.raw
        );
        let got = self.samples[0].value;
        assert!(
            (got - expected).abs() < 1e-9,
            "期望值 {expected}，实际 {got}: raw={}",
            self.raw
        );
    }
}

pub fn query_at(base: &str, promql: &str, time_secs: Option<f64>) -> Result<Response, String> {
    let mut url = format!(
        "{}/api/v1/query?query={}",
        base.trim_end_matches('/'),
        urlencoding::encode(promql)
    );
    if let Some(t) = time_secs {
        url.push_str(&format!("&time={t}"));
    }
    let (status, body) = http::get(&url)?;
    if !(200..300).contains(&status) {
        return Err(format!(
            "Prom 查询期望 2xx，实际 {status}: {}（{promql}）",
            http::tail(&body)
        ));
    }
    parse(&body).map_err(|e| format!("{e}，promql={promql}，body={}", http::tail(&body)))
}

pub fn query(base: &str, promql: &str) -> Result<Response, String> {
    query_at(base, promql, None)
}

/// 查询并解析，失败直接 panic（断言失败时信息足够定位）。
pub fn must_query(base: &str, promql: &str, time_secs: Option<f64>) -> Response {
    query_at(base, promql, time_secs).unwrap_or_else(|e| panic!("{e}"))
}

/// 轮询直到 `pred` 成立；返回成立时的响应。
pub fn wait_query(
    base: &str,
    promql: &str,
    time_secs: Option<f64>,
    timeout: Duration,
    pred: impl Fn(&Response) -> bool,
) -> Response {
    wait_until(
        &format!("Prom 查询 {promql} 满足断言"),
        timeout,
        || {
            let resp = query_at(base, promql, time_secs).unwrap_or_else(|e| panic!("{e}"));
            pred(&resp).then_some(resp)
        },
    )
}

fn as_f64(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.parse::<f64>().ok()))
}

fn parse(body: &str) -> Result<Response, String> {
    let value: Value = serde_json::from_str(body).map_err(|e| format!("返回非 JSON: {e}"))?;
    if value["status"] != "success" {
        return Err(format!("Prom status={}", value["status"]));
    }
    let result_type = value["data"]["resultType"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let raw = value["data"]["result"].clone();
    let mut samples = Vec::new();
    for item in raw.as_array().map(Vec::as_slice).unwrap_or_default() {
        let mut metric = BTreeMap::new();
        if let Some(map) = item["metric"].as_object() {
            for (k, v) in map {
                metric.insert(k.clone(), v.as_str().unwrap_or_default().to_string());
            }
        }
        // vector: value=[ts,"1"]；matrix: values=[[ts,"1"],…]
        let point: Option<Vec<Value>> = match item["value"].as_array() {
            Some(point) => Some(point.clone()),
            None => item["values"]
                .as_array()
                .and_then(|values| values.last())
                .and_then(|point| point.as_array())
                .cloned(),
        };
        let Some(point) = point else { continue };
        let timestamp = point.first().and_then(as_f64).unwrap_or_default();
        // dataserver 的代理口返回数字，VictoriaMetrics 返回字符串；两种都要吃。
        let value = point.get(1).and_then(as_f64).unwrap_or(f64::NAN);
        samples.push(Sample {
            metric,
            value,
            timestamp,
        });
    }
    Ok(Response {
        result_type,
        samples,
        raw,
    })
}
