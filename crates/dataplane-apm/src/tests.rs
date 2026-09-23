//! dataplane-apm 的集成测试：摘要累加器、端点表、迁移与 sink 组合。

use std::collections::BTreeMap;
use std::sync::Arc;

use dataplane_core::{DataplaneError, SqlValue};
use dataplane_ingest::trace::TraceSpan;
use dataplane_ingest::{DataEnvelope, DataType};
use dataplane_sql::{RelationalStore, SqliteRelationalStore};

use crate::accumulator::TraceSummaryAccumulator;
use crate::red::RedSamples;
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
        Arc::new(RedSamples::new(1_000)),
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
    let sink = ApmSink::new(
        sql,
        ApmSinkConfig::default(),
        Arc::new(RedSamples::new(1_000)),
    );
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

#[allow(clippy::too_many_arguments)]
fn edge_span(
    trace_id: &str,
    span_id: &str,
    parent: &str,
    kind: &str,
    service: &str,
    start_ts: i64,
    duration_micros: i64,
    status: &str,
    attrs: &[(&str, &str)],
) -> TraceSpan {
    let mut span = span(
        trace_id,
        span_id,
        parent,
        service,
        "op",
        start_ts,
        duration_micros,
        status,
    );
    span.kind = kind.to_string();
    for (k, v) in attrs {
        span.attributes.insert((*k).to_string(), (*v).to_string());
    }
    span
}

async fn edge_rows(sql: &dyn RelationalStore) -> Vec<Vec<SqlValue>> {
    sql.execute(
        &format!(
            "SELECT bucket_start, src_service, dst_service, calls, errors, duration_sum, duration_max
             FROM {} ORDER BY src_service, dst_service",
            tables::EDGE_SUMMARY
        ),
        &[],
    )
    .await
    .unwrap()
    .rows
}

#[tokio::test]
async fn edge_pairs_client_and_server_in_both_orders() {
    let dir = tempfile::tempdir().unwrap();
    let sql = store(dir.path());
    bootstrap(sql.as_ref()).await.unwrap();
    let edges = crate::edge::EdgeAccumulator::new(1_000, Arc::new(RedSamples::new(1_000)));
    let env = envelope();
    let trace = "bbbb0000000000000000000000000001";

    // 场景 1：client 先到，server 后到。
    edges.observe(
        &edge_span(
            trace,
            "0000000000000001",
            "0000000000000000",
            "client",
            "gateway",
            NOW,
            900,
            "ok",
            &[],
        ),
        &env,
    );
    assert_eq!(edges.pending_len(), 1, "client 在等 server");
    assert!(edges.observe(
        &edge_span(
            trace,
            "0000000000000002",
            "0000000000000001",
            "server",
            "order-api",
            NOW + 10,
            800,
            "ok",
            &[]
        ),
        &env
    ));
    assert_eq!(edges.pending_len(), 0, "配对后不再悬挂");

    // 场景 2：server 先到，client 后到（反向补齐）。
    let trace2 = "bbbb0000000000000000000000000002";
    edges.observe(
        &edge_span(
            trace2,
            "0000000000000003",
            "0000000000000004",
            "server",
            "payment",
            NOW + 20,
            700,
            "ok",
            &[],
        ),
        &env,
    );
    assert_eq!(edges.pending_len(), 1, "server 在等 client");
    assert!(edges.observe(
        &edge_span(
            trace2,
            "0000000000000004",
            "0000000000000000",
            "client",
            "gateway",
            NOW + 25,
            1_100,
            "error",
            &[]
        ),
        &env
    ));

    let written = edges.flush(sql.as_ref(), NOW, None).await.unwrap();
    assert_eq!(written, 2);
    let rows = edge_rows(sql.as_ref()).await;
    assert_eq!(rows.len(), 2);
    // 边方向固定 client → server。
    assert_eq!(as_text(&rows[0][1]), "gateway");
    assert_eq!(as_text(&rows[0][2]), "order-api");
    assert_eq!(as_i64(&rows[0][3]), 1, "calls");
    assert_eq!(as_i64(&rows[0][4]), 0, "errors");
    assert_eq!(as_i64(&rows[0][5]), 900, "duration_sum 取 client 耗时");
    assert_eq!(as_text(&rows[1][2]), "payment");
    assert_eq!(as_i64(&rows[1][4]), 1, "client status=error 计入 errors");
    assert_eq!(as_i64(&rows[1][5]), 1_100);
    // 桶按 client 时间戳落到整分钟。
    assert_eq!(as_i64(&rows[0][0]) % (60 * 1_000_000), 0);
}

#[tokio::test]
async fn edge_counts_one_call_when_client_has_many_servers() {
    let dir = tempfile::tempdir().unwrap();
    let sql = store(dir.path());
    bootstrap(sql.as_ref()).await.unwrap();
    let edges = crate::edge::EdgeAccumulator::new(1_000, Arc::new(RedSamples::new(1_000)));
    let env = envelope();
    let trace = "bbbb0000000000000000000000000003";
    // 两个 server span 先到，同一个 client 后到。
    for id in ["0000000000000011", "0000000000000012"] {
        edges.observe(
            &edge_span(
                trace,
                id,
                "0000000000000010",
                "server",
                "order-api",
                NOW + 5,
                100,
                "ok",
                &[],
            ),
            &env,
        );
    }
    edges.observe(
        &edge_span(
            trace,
            "0000000000000010",
            "0000000000000000",
            "client",
            "gateway",
            NOW,
            950,
            "ok",
            &[],
        ),
        &env,
    );
    edges.flush(sql.as_ref(), NOW, None).await.unwrap();
    let rows = edge_rows(sql.as_ref()).await;
    assert_eq!(rows.len(), 1, "同一逻辑边只有一行");
    assert_eq!(as_i64(&rows[0][3]), 1, "多条 server 只算一次调用");
    assert_eq!(as_i64(&rows[0][5]), 950);
}

#[tokio::test]
async fn edge_falls_back_to_unknown_and_normalizes_via_resolver() {
    let dir = tempfile::tempdir().unwrap();
    let sql = store(dir.path());
    bootstrap(sql.as_ref()).await.unwrap();
    let edges = crate::edge::EdgeAccumulator::new(1_000, Arc::new(RedSamples::new(1_000)));
    let env = envelope();

    // 配对不到且没有可用对端标识 → unknown。
    edges.observe(
        &edge_span(
            "bbbb0000000000000000000000000004",
            "0000000000000021",
            "0000000000000000",
            "client",
            "gateway",
            NOW,
            500,
            "ok",
            &[],
        ),
        &env,
    );
    // 配对不到但有 server.address → unknown:<addr>，由 resolver 归一。
    edges.observe(
        &edge_span(
            "bbbb0000000000000000000000000005",
            "0000000000000022",
            "0000000000000000",
            "client",
            "gateway",
            NOW,
            600,
            "ok",
            &[("server.address", "10.0.0.9"), ("server.port", "8080")],
        ),
        &env,
    );

    struct FixedResolver;
    #[async_trait::async_trait]
    impl crate::edge::ServiceResolver for FixedResolver {
        async fn resolve(
            &self,
            host_ip: &str,
            port: i64,
            _pod: &str,
        ) -> Result<Option<String>, DataplaneError> {
            if (host_ip, port) == ("10.0.0.9", 8080) {
                return Ok(Some("order-api".to_string()));
            }
            Ok(None)
        }
    }
    // 配对要等桶关闭才能判定「找不到对端」，因此用桶结束后的时间 flush。
    edges
        .flush(sql.as_ref(), NOW + 61_000_000, Some(&FixedResolver))
        .await
        .unwrap();
    let rows = edge_rows(sql.as_ref()).await;
    let dsts: Vec<String> = rows.iter().map(|r| as_text(&r[2])).collect();
    assert!(
        dsts.contains(&"order-api".to_string()),
        "归一后落真实服务名: {dsts:?}"
    );
    assert!(
        dsts.contains(&"unknown".to_string()),
        "无可解析目标时落 unknown: {dsts:?}"
    );
}

#[tokio::test]
async fn edge_dedupes_replayed_spans_and_bounds_pending() {
    let dir = tempfile::tempdir().unwrap();
    let sql = store(dir.path());
    bootstrap(sql.as_ref()).await.unwrap();
    let edges = crate::edge::EdgeAccumulator::new(2, Arc::new(RedSamples::new(1_000)));
    let env = envelope();
    let trace = "bbbb0000000000000000000000000006";
    let client = edge_span(
        trace,
        "0000000000000031",
        "0000000000000000",
        "client",
        "gw",
        NOW,
        100,
        "ok",
        &[],
    );
    let server = edge_span(
        trace,
        "0000000000000032",
        "0000000000000031",
        "server",
        "svc",
        NOW + 1,
        90,
        "ok",
        &[],
    );
    edges.observe(&client, &env);
    edges.observe(&server, &env);
    // 重放同样的 client+server：不得重复计数。
    edges.observe(&client, &env);
    edges.observe(&server, &env);
    edges.flush(sql.as_ref(), NOW, None).await.unwrap();
    let rows = edge_rows(sql.as_ref()).await;
    assert_eq!(as_i64(&rows[0][3]), 1, "重放不重复计数");

    // 容量上限：塞入超过容量的待配对 span。
    for i in 0..5 {
        edges.observe(
            &edge_span(
                &format!("bbbb0000000000000000000000001{i:02x}"),
                &format!("00000000000001{i:02x}"),
                "0000000000000000",
                "client",
                "gw",
                NOW,
                10,
                "ok",
                &[],
            ),
            &env,
        );
    }
    assert!(
        edges.pending_len() <= 2,
        "容量上限生效: {}",
        edges.pending_len()
    );
    assert!(edges.dropped_pending() > 0);
}

fn ts_store(dir: &std::path::Path) -> Arc<dyn dataplane_ts::TimeSeriesStore> {
    Arc::new(
        dataplane_ts::TsinkTimeSeriesStore::new(
            dir.join("ts"),
            dataplane_ts::TsRetentionConfig {
                enforced: false,
                ..dataplane_ts::TsRetentionConfig::default()
            },
        )
        .unwrap(),
    )
}

#[tokio::test]
async fn aggregator_writes_service_red_and_edge_metrics() {
    use crate::aggregator::ApmAggregator;

    let dir = tempfile::tempdir().unwrap();
    let sql = store(dir.path());
    bootstrap(sql.as_ref()).await.unwrap();
    let ts = ts_store(dir.path());
    let samples = Arc::new(RedSamples::new(1_000));

    // 已关闭的两个分钟桶：B1 是目标桶。
    let b1 = NOW - 120_000_000;
    let b2 = NOW - 60_000_000;
    samples.observe_span(b1, "order-api", "GET /orders", "server", "ok", 100);
    samples.observe_span(b1, "order-api", "GET /orders", "server", "ok", 200);
    samples.observe_span(b1, "order-api", "GET /orders", "server", "error", 900);
    // 非 server span 不进 apm_service_*。
    samples.observe_span(b1, "order-api", "db.query", "internal", "ok", 50);
    samples.observe_span(b2, "payment", "POST /pay", "server", "ok", 300);
    // 边样本（p95 来源）。
    samples.observe_edge(b1, "gateway", "order-api", "server", "ok", 400);
    samples.observe_edge(b1, "gateway", "order-api", "server", "ok", 600);
    samples.observe_edge(b1, "gateway", "order-api", "server", "error", 1_000);

    // 边摘要有权威计数。
    sql.execute(
        &format!(
            "INSERT INTO {} (bucket_start, src_service, dst_service, span_kind, calls, errors,
                duration_sum, duration_max, agent_id, data_id)
             VALUES (?1, 'gateway', 'order-api', 'server', 3, 1, 2000, 1000, 'agent-1', 'apm-1')",
            tables::EDGE_SUMMARY
        ),
        &[SqlValue::Integer(b1)],
    )
    .await
    .unwrap();

    let aggregator = ApmAggregator::new(sql.clone(), ts.clone(), samples.clone());
    let report = aggregator.run_once(NOW).await.unwrap();
    assert!(
        report.service_points > 0 && report.edge_points > 0,
        "{report:?}"
    );
    assert!(report.buckets.contains(&b1), "{report:?}");

    let requests = ts
        .query_instant(
            "apm_service_requests_total{service=\"order-api\",status=\"ok\"}",
            Some(b1),
        )
        .await
        .unwrap();
    assert_eq!(
        requests
            .result
            .first()
            .and_then(|s| s.value)
            .map(|(_, v)| v),
        Some(2.0),
        "server span 计数：{requests:?}"
    );

    let errors = ts
        .query_instant("apm_service_errors_total{service=\"order-api\"}", Some(b1))
        .await
        .unwrap();
    assert_eq!(
        errors.result.first().and_then(|s| s.value).map(|(_, v)| v),
        Some(1.0)
    );

    let internal = ts
        .query_instant(
            "apm_service_requests_total{operation=\"db.query\"}",
            Some(b1),
        )
        .await
        .unwrap();
    assert!(
        internal.result.iter().all(|s| s.value.is_none()),
        "internal span 不应产生 apm_service_*: {internal:?}"
    );

    let duration_p95 = ts
        .query_instant(
            "apm_service_duration_micros{service=\"order-api\",field=\"p95\"}",
            Some(b1),
        )
        .await
        .unwrap();
    assert_eq!(
        duration_p95
            .result
            .first()
            .and_then(|s| s.value)
            .map(|(_, v)| v),
        Some(900.0),
        "p95 由 label field 区分: {duration_p95:?}"
    );
    let duration_all = ts
        .query_instant(
            "apm_service_duration_micros{service=\"order-api\"}",
            Some(b1),
        )
        .await
        .unwrap();
    let mut values: Vec<f64> = duration_all
        .result
        .iter()
        .filter_map(|s| s.value.map(|(_, v)| v))
        .collect();
    values.sort_by(|a, b| a.partial_cmp(b).unwrap());
    // avg=400, p50=200, p95=900, p99=900, max=900
    assert_eq!(
        values,
        vec![200.0, 400.0, 900.0, 900.0, 900.0],
        "avg/p50/p95/p99/max 应各自成为一条序列: {duration_all:?}"
    );

    let edge_calls = ts
        .query_instant("apm_edge_requests_total{src_service=\"gateway\"}", Some(b1))
        .await
        .unwrap();
    assert_eq!(
        edge_calls
            .result
            .first()
            .and_then(|s| s.value)
            .map(|(_, v)| v),
        Some(3.0),
        "边 calls 取 sqlite 权威值: {edge_calls:?}"
    );
    let edge_p95 = ts
        .query_instant(
            "apm_edge_duration_micros{src_service=\"gateway\"}",
            Some(b1),
        )
        .await
        .unwrap();
    let mut edge_values: Vec<f64> = edge_p95
        .result
        .iter()
        .filter_map(|s| s.value.map(|(_, v)| v))
        .collect();
    edge_values.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert_eq!(edge_values, vec![666.6666666666666, 1000.0], "avg + p95");

    // 样本取出后不重复结算。
    let second = aggregator.run_once(NOW).await.unwrap();
    assert_eq!(second.service_points, 0, "同批样本只结算一次");
    assert_eq!(second.edge_points, 0, "边样本也只结算一次");
}

#[tokio::test]
async fn aggregator_empty_bucket_writes_nothing() {
    use crate::aggregator::ApmAggregator;

    let dir = tempfile::tempdir().unwrap();
    let sql = store(dir.path());
    bootstrap(sql.as_ref()).await.unwrap();
    let ts = ts_store(dir.path());
    let samples = Arc::new(RedSamples::new(1_000));
    let aggregator = ApmAggregator::new(sql, ts.clone(), samples);
    let report = aggregator.run_once(NOW).await.unwrap();
    assert_eq!(report.service_points, 0);
    assert_eq!(report.edge_points, 0);
    let any = ts
        .query_instant("apm_service_requests_total", Some(NOW))
        .await
        .unwrap();
    assert!(any.result.is_empty(), "空桶不写零值: {any:?}");
}
