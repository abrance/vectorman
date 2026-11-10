//! 极简 HTTP 客户端：只用阻塞 ureq，测试都是同步线程，不引入额外运行时。

use std::sync::OnceLock;
use std::time::Duration;

use serde_json::Value;

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(15))
            .build()
    })
}

pub fn get(url: &str) -> Result<(u16, String), String> {
    match agent().get(url).call() {
        Ok(resp) => Ok((resp.status(), resp.into_string().unwrap_or_default())),
        Err(ureq::Error::Status(code, resp)) => {
            Ok((code, resp.into_string().unwrap_or_default()))
        }
        Err(e) => Err(format!("GET {url} 传输失败: {e}")),
    }
}

pub fn post_json(url: &str, body: &str) -> Result<(u16, String), String> {
    match agent()
        .post(url)
        .set("Content-Type", "application/json")
        .send_string(body)
    {
        Ok(resp) => Ok((resp.status(), resp.into_string().unwrap_or_default())),
        Err(ureq::Error::Status(code, resp)) => {
            Ok((code, resp.into_string().unwrap_or_default()))
        }
        Err(e) => Err(format!("POST {url} 传输失败: {e}")),
    }
}

pub fn put_json(url: &str, body: &str) -> Result<(u16, String), String> {
    match agent()
        .put(url)
        .set("Content-Type", "application/json")
        .send_string(body)
    {
        Ok(resp) => Ok((resp.status(), resp.into_string().unwrap_or_default())),
        Err(ureq::Error::Status(code, resp)) => {
            Ok((code, resp.into_string().unwrap_or_default()))
        }
        Err(e) => Err(format!("PUT {url} 传输失败: {e}")),
    }
}

/// GET 并解析 JSON；非 2xx 直接 panic（带上 URL 与 body 尾部）。
pub fn get_json_ok(url: &str) -> Value {
    let (status, body) = get(url).unwrap_or_else(|e| panic!("{e}"));
    assert!(
        (200..300).contains(&status),
        "GET {url} 期望 2xx，实际 {status}: {}",
        tail(&body)
    );
    serde_json::from_str(&body)
        .unwrap_or_else(|e| panic!("GET {url} 返回非 JSON（{e}）: {}", tail(&body)))
}

/// POST JSON 并解析 JSON；非 2xx 直接 panic。
pub fn post_json_ok(url: &str, body: &str) -> Value {
    let (status, resp) = post_json(url, body).unwrap_or_else(|e| panic!("{e}"));
    assert!(
        (200..300).contains(&status),
        "POST {url} 期望 2xx，实际 {status}: {}",
        tail(&resp)
    );
    serde_json::from_str(&resp)
        .unwrap_or_else(|e| panic!("POST {url} 返回非 JSON（{e}）: {}", tail(&resp)))
}

pub fn tail(body: &str) -> &str {
    let limit = 800;
    if body.len() <= limit {
        return body;
    }
    let mut start = body.len() - limit;
    while !body.is_char_boundary(start) {
        start += 1;
    }
    &body[start..]
}
