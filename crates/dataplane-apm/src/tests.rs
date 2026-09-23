//! dataplane-apm 的集成测试：摘要累加器、端点表、迁移与 sink 组合。

use std::collections::BTreeMap;
use std::sync::Arc;

use dataplane_core::{DataplaneError, SqlValue};
use dataplane_ingest::trace::TraceSpan;
use dataplane_ingest::{DataEnvelope, DataType};
use dataplane_sql::{RelationalStore, SqliteRelationalStore};

use crate::accumulator::TraceSummaryAccumulator;
use crate::sink::ApmSink;
use crate::{bootstrap, tables, ApmSinkConfig};

const NOW: i64 = 1_710_000_000_000_000;

fn store(dir: &std::path::Path) -> Arc<dyn RelationalStore> {
    Arc::new(SqliteRelationalStore::new(dir.join("sql.sqlite")).unwrap())
}

fn envelope() -> DataEnvelope {
    DataEnvelope {
        batch_id: "b1".into(),
        data_type: DataType::Traces,
        data_id: "apm-1".into(),
        agent_id: "agent-1".into(),
        host_id: "host-1".into(),
        sent_at_micros: NOW,
        records: Vec::new(),
    }
}

#[allow(clippy::too_many_arguments)]
fn span(
    trace_id: &str,
    span_id: &str,
    parent: &str,
    service: &str,
    name: &str,
    start_ts: i64,
    duration_micros: i64,
    status: &str,
) -> TraceSpan {
    TraceSpan {
        record_id: format!("{trace_id}:{span_id}"),
        timestamp: start_ts,
        trace_id: trace_id.to_string(),
        span_id: span_id.to_string(),
        parent_span_id: parent.to_string(),
        name: name.to_string(),
        kind: "server".to_string(),
        start_unix_nano: start_ts * 1_000,
        end_unix_nano: (start_ts + duration_micros) * 1_000,
        status_code: status.to_string(),
        service: service.to_string(),
        collector: "otlp".to_string(),
        labels: BTreeMap::new(),
        ..TraceSpan::default()
    }
}

async fn summary_row(sql: &dyn RelationalStore, trace_id: &str) -> Vec<SqlValue> {
    let result = sql
        .execute(
            &format!(
                "SELECT start_ts, max_end_ts, duration_micros, root_service, root_operation,
                        root_start_ts, span_count, error_count, status, services_json
                 FROM {} WHERE trace_id = ?1",
                tables::TRACE_SUMMARY
            ),
            &[SqlValue::Text(trace_id.to_string())],
        )
        .await
        .unwrap();
    result.rows.first().cloned().unwrap_or_default()
}

fn as_i64(v: &SqlValue) -> i64 {
    match v {
        SqlValue::Integer(i) => *i,
        _ => panic!("expected integer, got {v:?}"),
    }
}

fn as_text(v: &SqlValue) -> String {
    match v {
        SqlValue::Text(s) => s.clone(),
        _ => panic!("expected text, got {v:?}"),
    }
}

#[tokio::test]
async fn out_of_order_spans_merge_extremes_and_pick_earliest_root() {
    let dir = tempfile::tempdir().unwrap();
    let sql = store(dir.path());
    bootstrap(sql.as_ref()).await.unwrap();
    let acc = TraceSummaryAccumulator::new(ApmSinkConfig::default());
    let env = envelope();
    let trace = "aaaa0000000000000000000000000001";

    // 12 条 span，乱序到达；其中 2 条是根（parent 为空），更早的那个应成为根。
    let mut spans = vec![
        span(
            trace,
            "0000000000000001",
            "",
            "late-root",
            "GET /late",
            NOW,
            500,
            "ok",
        ),
        span(
            trace,
            "0000000000000002",
            "",
            "gateway",
            "GET /checkout",
            NOW - 300,
            3_300,
            "ok",
        ),
    ];
    for i in 0..9 {
        let id = format!("00000000000001{i:02x}");
        spans.push(span(
            trace,
            &id,
            "0000000000000002",
            if i % 3 == 0 { "order-api" } else { "payment" },
            "inner",
            NOW - 200 + i,
            10 + i,
            if i == 4 { "error" } else { "ok" },
        ));
    }
    // 故意打乱顺序：先中间的、再最早的、最后最晚的。
    let reordered: Vec<TraceSpan> = spans
        .iter()
        .skip(2)
        .chain(spans.iter().take(1))
        .chain(spans.iter().skip(1).take(1))
        .cloned()
        .collect();
    assert_eq!(reordered.len(), 11);
    for s in &reordered {
        acc.observe(s, &env);
    }
    acc.flush(sql.as_ref(), NOW).await.unwrap();

    let row = summary_row(sql.as_ref(), trace).await;
    assert_eq!(as_i64(&row[0]), NOW - 300, "start_ts 取最小 start");
    assert_eq!(as_i64(&row[1]), NOW + 3_000, "max_end_ts 取最大 end");
    assert_eq!(as_i64(&row[2]), 3_300, "duration = max_end - min_start");
    assert_eq!(as_text(&row[3]), "gateway", "根取 start_ts 最小的根 span");
    assert_eq!(as_text(&row[4]), "GET /checkout");
    assert_eq!(
        as_i64(&row[5]),
        NOW - 300,
        "root_start_ts 落库以便跨 flush 比较"
    );
    assert_eq!(as_i64(&row[6]), 11);
    assert_eq!(as_i64(&row[7]), 1);
    assert_eq!(as_text(&row[8]), "error");
    let services: Vec<String> = serde_json::from_str(&as_text(&row[9])).unwrap();
    assert_eq!(
        services,
        vec!["gateway", "late-root", "order-api", "payment"]
    );
}

#[tokio::test]
async fn later_root_with_smaller_start_replaces_existing_root() {
    let dir = tempfile::tempdir().unwrap();
    let sql = store(dir.path()).clone();
    bootstrap(sql.as_ref()).await.unwrap();
    let acc = TraceSummaryAccumulator::new(ApmSinkConfig::default());
    let env = envelope();
    let trace = "aaaa0000000000000000000000000002";

    acc.observe(
        &span(
            trace,
            "0000000000000001",
            "",
            "late-root",
            "GET /late",
            NOW,
            100,
            "ok",
        ),
        &env,
    );
    acc.flush(sql.as_ref(), NOW).await.unwrap();
    // 更早的根在第二次 flush 才到达：必须替换掉已有根。
    acc.observe(
        &span(
            trace,
            "0000000000000002",
            "",
            "gateway",
            "GET /checkout",
            NOW - 900,
            100,
            "ok",
        ),
        &env,
    );
    acc.flush(sql.as_ref(), NOW).await.unwrap();

    let row = summary_row(sql.as_ref(), trace).await;
    assert_eq!(as_text(&row[3]), "gateway");
    assert_eq!(as_i64(&row[5]), NOW - 900);
    assert_eq!(as_i64(&row[6]), 2, "跨 flush 累加不重复也不丢");
}

#[tokio::test]
async fn repeated_flush_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let sql = store(dir.path());
    bootstrap(sql.as_ref()).await.unwrap();
    let acc = TraceSummaryAccumulator::new(ApmSinkConfig::default());
    let env = envelope();
    let trace = "aaaa0000000000000000000000000003";
    for i in 0..3 {
        acc.observe(
            &span(
                trace,
                &format!("000000000000000{i}"),
                "0000000000000000",
                "svc",
                "op",
                NOW + i,
                10,
                "ok",
            ),
            &env,
        );
    }
    assert_eq!(acc.flush(sql.as_ref(), NOW).await.unwrap(), 1);
    assert_eq!(
        acc.flush(sql.as_ref(), NOW).await.unwrap(),
        0,
        "无增量时不写"
    );
    let row = summary_row(sql.as_ref(), trace).await;
    assert_eq!(as_i64(&row[6]), 3);
}

#[tokio::test]
async fn reload_continues_counting_from_persisted_row() {
    let dir = tempfile::tempdir().unwrap();
    let sql = store(dir.path());
    bootstrap(sql.as_ref()).await.unwrap();
    let env = envelope();
    let trace = "aaaa0000000000000000000000000004";

    let acc = TraceSummaryAccumulator::new(ApmSinkConfig::default());
    for i in 0..2 {
        acc.observe(
            &span(
                trace,
                &format!("000000000000000{i}"),
                "0000000000000000",
                "svc",
                "op",
                NOW + i,
                10,
                "ok",
            ),
            &env,
        );
    }
    acc.flush(sql.as_ref(), NOW).await.unwrap();

    // 模拟进程重启：新实例回载 5 分钟内的 trace，继续累加。
    let restarted = TraceSummaryAccumulator::new(ApmSinkConfig::default());
    let loaded = restarted
        .reload(sql.as_ref(), NOW + 1_000_000)
        .await
        .unwrap();
    assert_eq!(loaded, 1);
    restarted.observe(
        &span(
            trace,
            "0000000000000009",
            "0000000000000000",
            "svc",
            "op",
            NOW + 5,
            10,
            "ok",
        ),
        &env,
    );
    restarted
        .flush(sql.as_ref(), NOW + 1_000_000)
        .await
        .unwrap();
    let row = summary_row(sql.as_ref(), trace).await;
    assert_eq!(as_i64(&row[6]), 3, "回载后继续累加不重复");

    // 超出回载窗口的 trace 不回载。
    let later = TraceSummaryAccumulator::new(ApmSinkConfig::default());
    let loaded = later
        .reload(sql.as_ref(), NOW + 6 * 60 * 1_000_000)
        .await
        .unwrap();
    assert_eq!(loaded, 0, "6 分钟前的 trace 不回载");
}

#[tokio::test]
async fn capacity_evicts_oldest_clean_entries_after_flush() {
    let dir = tempfile::tempdir().unwrap();
    let sql = store(dir.path());
    bootstrap(sql.as_ref()).await.unwrap();
    let config = ApmSinkConfig {
        max_live_traces: 2,
        ..ApmSinkConfig::default()
    };
    let acc = TraceSummaryAccumulator::new(config);
    let env = envelope();
    for i in 0..3 {
        let trace = format!("aaaa000000000000000000000000000{i}");
        acc.observe(
            &span(
                &trace,
                "0000000000000001",
                "",
                "svc",
                "op",
                NOW + i,
                10,
                "ok",
            ),
            &env,
        );
    }
    acc.flush(sql.as_ref(), NOW).await.unwrap();
    assert!(acc.live_len() <= 2, "容量上限生效，实际 {}", acc.live_len());
    assert_eq!(acc.dropped_traces(), 1);
    // 被淘汰的条目已经落库，不丢数据。
    for i in 0..3 {
        let trace = format!("aaaa000000000000000000000000000{i}");
        let row = summary_row(sql.as_ref(), &trace).await;
        assert_eq!(as_i64(&row[6]), 1, "trace {trace} 应已落库");
    }
}

#[tokio::test]
async fn sink_registers_endpoints_and_looks_them_up() {
    let dir = tempfile::tempdir().unwrap();
    let sql = store(dir.path());
    bootstrap(sql.as_ref()).await.unwrap();
    let sink = ApmSink::new(
        sql.clone(),
        ApmSinkConfig {
            endpoint_cache_ttl_secs: 0,
            ..ApmSinkConfig::default()
        },
    );
    use dataplane_ingest::trace::TraceSink;

    let mut s = span(
        "aaaa0000000000000000000000000005",
        "0000000000000001",
        "",
        "order-api",
        "GET /orders",
        NOW,
        10,
        "ok",
    );
    s.resource.insert("host.ip".into(), "10.0.0.9".into());
    s.resource
        .insert("k8s.pod.name".into(), "order-api-7c9f".into());
    s.resource
        .insert("service.instance.id".into(), "order-api-1".into());
    s.attributes.insert("server.port".into(), "8080".into());
    sink.observe_span(&s, &envelope()).await.unwrap();

    let report = sink.flush_now(NOW).await.unwrap();
    assert_eq!(report.traces, 1);
    assert_eq!(report.endpoints, 1);

    assert_eq!(
        sink.lookup_service("10.0.0.9", 8080, "").await.unwrap(),
        Some("order-api".into()),
        "按 ip:port 反查"
    );
    assert_eq!(
        sink.lookup_service("", 0, "order-api-7c9f").await.unwrap(),
        Some("order-api".into()),
        "按 Pod 名反查"
    );
    assert_eq!(
        sink.lookup_service("10.0.0.1", 9999, "nope").await.unwrap(),
        None
    );

    // 保留期清理：端点 last_seen 用墙上时钟（observe 时刻），因此清理时间也要基于当前时间。
    let now_wall = crate::now_micros();
    sink.observe_span(&s, &envelope()).await.unwrap();
    sink.flush_now(NOW).await.unwrap();
    assert_eq!(
        sink.purge_expired(now_wall).await.unwrap(),
        0,
        "未过期的端点不清理"
    );
    let purged = sink
        .purge_expired(now_wall + 31 * 86_400 * 1_000_000)
        .await
        .unwrap();
    assert_eq!(purged, 1, "超过 30 天的端点应被清理");
}

#[tokio::test]
async fn v1_database_is_migrated_to_v2() {
    let dir = tempfile::tempdir().unwrap();
    let sql = store(dir.path());
    // 构造 v1 库：没有 root_start_ts 列，版本行为 1。
    sql.execute(
        "CREATE TABLE obs_schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        &[],
    )
    .await
    .unwrap();
    sql.execute(
        "CREATE TABLE apm_trace_summary (
            trace_id TEXT PRIMARY KEY, start_ts INTEGER NOT NULL,
            max_end_ts INTEGER NOT NULL DEFAULT 0, duration_micros INTEGER NOT NULL,
            root_service TEXT NOT NULL, root_operation TEXT NOT NULL,
            span_count INTEGER NOT NULL, error_count INTEGER NOT NULL, status TEXT NOT NULL,
            services_json TEXT NOT NULL, collector TEXT NOT NULL, agent_id TEXT NOT NULL,
            host_id TEXT NOT NULL DEFAULT '', data_id TEXT NOT NULL, updated_ts INTEGER NOT NULL)",
        &[],
    )
    .await
    .unwrap();
    sql.execute(
        "INSERT INTO obs_schema_meta (key, value) VALUES ('schema_version', '1')",
        &[],
    )
    .await
    .unwrap();

    bootstrap(sql.as_ref()).await.unwrap();
    assert_eq!(crate::read_version(sql.as_ref()).await.unwrap(), Some(2));

    // 迁移后可以写入 root_start_ts，且重复 bootstrap 幂等。
    let acc = TraceSummaryAccumulator::new(ApmSinkConfig::default());
    let trace = "aaaa0000000000000000000000000006";
    acc.observe(
        &span(trace, "0000000000000001", "", "svc", "op", NOW, 5, "ok"),
        &envelope(),
    );
    acc.flush(sql.as_ref(), NOW).await.unwrap();
    let row = summary_row(sql.as_ref(), trace).await;
    assert_eq!(as_i64(&row[5]), NOW);
    bootstrap(sql.as_ref()).await.unwrap();
}

#[tokio::test]
async fn missing_endpoint_resource_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let sql = store(dir.path());
    bootstrap(sql.as_ref()).await.unwrap();
    let sink = ApmSink::new(sql, ApmSinkConfig::default());
    let mut s = span(
        "aaaa0000000000000000000000000007",
        "0000000000000001",
        "",
        "svc",
        "op",
        NOW,
        5,
        "ok",
    );
    s.resource.clear();
    s.attributes.clear();
    use dataplane_ingest::trace::TraceSink;
    sink.observe_span(&s, &envelope()).await.unwrap();
    let report = sink.flush_now(NOW).await.unwrap();
    assert_eq!(report.endpoints, 1, "缺 host.ip/port 时仍登记服务名本身");
    assert_eq!(report.traces, 1);
}

#[allow(dead_code)]
fn assert_error_code(e: DataplaneError, code: &str) {
    assert_eq!(e.code.as_str(), code);
}
