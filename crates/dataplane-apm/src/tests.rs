//! dataplane-apm 的集成测试：摘要累加器、端点表、迁移与 sink 组合。

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::json;

use dataplane_core::{DataplaneError, SqlValue};
use dataplane_ingest::trace::TraceSpan;
use dataplane_ingest::{DataEnvelope, DataType};
use dataplane_log::LogStore;
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
    assert_eq!(
        crate::read_version(sql.as_ref()).await.unwrap(),
        Some(crate::SCHEMA_VERSION)
    );

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

/// eBPF 边：反查顺序（静态映射 → 端点表 `(ip,port)` → Pod → `unknown-<ip>`）与覆盖写幂等。
#[tokio::test]
async fn ebpf_edge_upsert_and_resolve_order() {
    use dataplane_ingest::{DataEnvelope, DataType, EbpfEdge, EdgeSink};

    let dir = tempfile::tempdir().unwrap();
    let sql = store(dir.path());
    crate::bootstrap(sql.as_ref()).await.unwrap();
    // `EndpointRegistry::observe/flush` 都是 `&self`（内部可变），一个实例即可。
    let endpoints = std::sync::Arc::new(crate::endpoint::EndpointRegistry::new(30, 60));
    let aliases = std::sync::Arc::new(crate::alias::AliasCache::new(60));
    let sink = crate::ebpf_edge::EbpfEdgeSink::new(sql.clone(), endpoints.clone(), aliases.clone());

    let envelope = DataEnvelope {
        batch_id: "b1".into(),
        data_type: DataType::EbpfEdges,
        data_id: "item-ebpf".into(),
        agent_id: "agent-1".into(),
        host_id: "host-1".into(),
        sent_at_micros: NOW,
        records: Vec::new(),
    };

    let mut edge: EbpfEdge = serde_json::from_value(serde_json::json!({
        "record_id": "agent-1:1:a",
        "timestamp": NOW,
        "bucket_micros": 10_000_000i64,
        "protocol": "tcp",
        "src_ip": "10.0.0.5",
        "src_port": 40000,
        "dst_ip": "10.0.0.9",
        "dst_port": 8080,
        "src_process": "java",
        "connections": 2,
        "bytes_sent": 10,
        "bytes_recv": 20,
        "duration_micros_sum": 5,
        "duration_micros_max": 5,
        "tcp_retrans": 0,
        "tcp_resets": 0,
        "failures": 0,
        "latency_hist": [1]
    }))
    .unwrap();

    // 两边都没命中：落 unknown-<ip>。
    assert_eq!(sink.resolve_src(&edge).await.unwrap(), "unknown-10.0.0.5");
    assert_eq!(sink.resolve_dst(&edge).await.unwrap(), "unknown-10.0.0.9");

    // 静态映射命中进程名（优先级最高）。缓存必须显式失效才看得到新配置
    // （生产路径由 `ApmSink::upsert_alias` 触发，这条断言同时锁住该契约）。
    crate::alias::upsert_alias(
        sql.as_ref(),
        &crate::alias::AliasUpsert {
            match_kind: "process_name".into(),
            match_value: "java".into(),
            service: "order-api".into(),
            enabled: Some(true),
            note: None,
        },
        NOW,
    )
    .await
    .unwrap();
    aliases.invalidate();
    assert_eq!(sink.resolve_src(&edge).await.unwrap(), "order-api");

    // 目标侧靠端点表 (ip, port) 命中。上面那次未命中的查询已把负面结果缓存 60 秒，
    // 所以这里换一个**新缓存**的写入器来验证命中路径，同时断言旧实例仍返回未识别。
    let mut span = span(
        "bbbb0000000000000000000000000001",
        "0000000000000001",
        "",
        "pay-api",
        "GET /pay",
        NOW,
        5,
        "ok",
    );
    span.resource.insert("host.ip".into(), "10.0.0.9".into());
    // 监听端口取自 span 属性（`server.port` 优先，其次 `net.host.port`）。
    span.attributes.insert("server.port".into(), "8080".into());
    endpoints.observe(&span, NOW);
    endpoints.flush(sql.as_ref()).await.unwrap();
    assert_eq!(
        sink.resolve_dst(&edge).await.unwrap(),
        "unknown-10.0.0.9",
        "负面结果在 TTL 内保持（设计里明确要求缓存未命中，避免每批查 sqlite）"
    );
    let sink = crate::ebpf_edge::EbpfEdgeSink::new(
        sql.clone(),
        std::sync::Arc::new(crate::endpoint::EndpointRegistry::new(30, 60)),
        aliases.clone(),
    );
    assert_eq!(sink.resolve_dst(&edge).await.unwrap(), "pay-api");

    // Agent 已填写的服务名不被覆盖。
    edge.src_service = "given-src".into();
    assert_eq!(sink.resolve_src(&edge).await.unwrap(), "given-src");

    // 落库 + 覆盖写幂等。
    edge.src_service.clear();
    EdgeSink::observe_edge(&sink, &edge, &envelope)
        .await
        .unwrap();
    EdgeSink::observe_edge(&sink, &edge, &envelope)
        .await
        .unwrap();
    assert_eq!(sink.written(), 2, "两次写入都记数，但表里只有一行");
    let rows = sql
        .execute(
            "SELECT src_service, dst_service, connections, latency_hist FROM ebpf_edges",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(rows.rows.len(), 1, "record_id 主键覆盖写");
    assert_eq!(as_text(&rows.rows[0][0]), "order-api");
    assert_eq!(as_text(&rows.rows[0][1]), "pay-api");
    assert_eq!(as_i64(&rows.rows[0][2]), 2);
    assert_eq!(as_text(&rows.rows[0][3]), "[1]");
}

/// eBPF 边指标派生：游标推进、迟到边被滞后窗口覆盖、重写同一点不翻倍。
#[tokio::test]
async fn ebpf_metrics_aggregation_uses_watermark_and_is_idempotent() {
    use crate::ebpf_metrics::{read_watermark, write_watermark, EbpfMetricsAggregator, EdgeRow};

    let dir = tempfile::tempdir().unwrap();
    let sql = store(dir.path());
    crate::bootstrap(sql.as_ref()).await.unwrap();
    let ts = ts_store(dir.path());

    // 首次运行只对齐游标，不回溯历史。
    let agg = EbpfMetricsAggregator::new(sql.clone(), ts.clone(), 60);
    let report = agg.run_once(NOW).await.unwrap();
    assert_eq!(report.points, 0);
    // 首次运行把游标对齐到「当前时间 − 滞后 60 秒」所在分钟。
    let first = (NOW - 60_000_000) / 60_000_000 * 60_000_000;
    assert_eq!(read_watermark(sql.as_ref()).await.unwrap(), Some(first));

    // 造两条边：一条在滞后窗口内（会被处理），一条「迟到」到上一分钟。
    let insert = |record_id: &str, bucket_start: i64| {
        let sql = sql.clone();
        let record_id = record_id.to_string();
        async move {
            sql.execute(
                &format!(
                    "INSERT OR REPLACE INTO {} (record_id, bucket_start, bucket_micros, protocol,
                        src_ip, src_port, dst_ip, dst_port, src_service, dst_service,
                        connections, bytes_sent, bytes_recv, duration_sum, duration_max,
                        tcp_retrans, tcp_resets, failures, failure_reason, latency_hist,
                        agent_id, data_id)
                     VALUES (?1,?2,10000000,'tcp','10.0.0.5',40000,'10.0.0.9',8080,'order-api','pay-api',
                        2,10,20,100,50,1,0,0,'','[0,2]','agent-1','item')",
                    crate::tables::EBPF_EDGES
                ),
                &[
                    dataplane_core::SqlValue::Text(record_id),
                    dataplane_core::SqlValue::Integer(bucket_start),
                ],
            )
            .await
            .unwrap();
        }
    };
    let later = NOW + 60_000_000; // 下一分钟
    insert("e-2", later).await;

    // 滞后 60 秒：现在只处理到 later - 60s，所以这条边还不该被处理。
    let report = agg.run_once(later).await.unwrap();
    assert_eq!(report.rows, 0, "滞后窗口内的桶不处理");

    // 再往前推进一分钟，边被处理。
    let report = agg.run_once(later + 60_000_000).await.unwrap();
    assert_eq!(report.rows, 1);
    assert!(
        report.points >= 4,
        "连接数/请求数/耗时等都要出点: {report:?}"
    );

    let bucket = later / 60_000_000 * 60_000_000;
    /// `query_instant` 的 `eval_time` 单位是**微秒**（Prom HTTP 层才把秒换算成微秒）。
    async fn value_of(
        ts: &std::sync::Arc<dyn dataplane_ts::TimeSeriesStore>,
        measurement: &str,
        at_micros: i64,
    ) -> f64 {
        let result = ts
            .query_instant(measurement, Some(at_micros))
            .await
            .unwrap_or_else(|e| panic!("query {measurement}: {}", e.message));
        result
            .result
            .iter()
            .filter_map(|series| series.value)
            .map(|(_, value)| value)
            .sum()
    }
    assert_eq!(
        value_of(&ts, "ebpf_edge_connections_total", bucket).await,
        2.0
    );
    assert_eq!(value_of(&ts, "apm_edge_requests_total", bucket).await, 2.0);
    assert_eq!(value_of(&ts, "ebpf_tcp_retrans_total", bucket).await, 1.0);

    // 重写同一批点（模拟「写完点后崩溃、游标未推进」）：值不翻倍。
    let rows = vec![EdgeRow {
        bucket_start: later,
        src_service: "order-api".into(),
        dst_service: "pay-api".into(),
        src_ip: "10.0.0.5".into(),
        src_port: 40_000,
        dst_ip: "10.0.0.9".into(),
        dst_port: 8_080,
        protocol: "tcp".into(),
        connections: 2,
        bytes_sent: 10,
        bytes_recv: 20,
        duration_sum: 100,
        tcp_retrans: 1,
        tcp_resets: 0,
        failures: 0,
        failure_reason: String::new(),
        latency_hist: vec![0, 2],
    }];
    for point in crate::ebpf_metrics::aggregate(&rows) {
        ts.write(point).await.unwrap();
    }
    assert_eq!(
        value_of(&ts, "ebpf_edge_connections_total", bucket).await,
        2.0,
        "TimeSeriesStore 对相同 measurement+labels+timestamp 是覆盖语义"
    );

    // 手动回退游标可重放：再跑一轮不报错，且值仍然不翻倍。
    write_watermark(sql.as_ref(), bucket - 60_000_000)
        .await
        .unwrap();
    let report = agg.run_once(later + 120_000_000).await.unwrap();
    assert_eq!(report.rows, 1);
    eprintln!("report2 = {report:?}");
    // 自检：手工写一个点再查（隔离「存储本身是否可查」）。
    ts.write(dataplane_ts::TsPoint {
        measurement: "probe_metric".into(),
        tags: std::collections::BTreeMap::new(),
        field_name: "value".into(),
        field_value: 7.0,
        timestamp: bucket,
    })
    .await
    .unwrap();
    let probe = ts
        .query_instant("probe_metric", Some(bucket / 1_000_000))
        .await
        .unwrap();
    eprintln!("probe => {:?}", probe.result);
    for measurement in [
        "ebpf_edge_connections_total",
        "ebpf_edge_connections",
        "apm_edge_requests_total",
        "apm_edge_requests",
        "ebpf_tcp_retrans_total",
        "apm_edge_duration_micros",
    ] {
        let result = ts
            .query_instant(measurement, Some(bucket / 1_000_000))
            .await
            .unwrap();
        eprintln!("{measurement} => {:?}", result.result);
    }
    assert_eq!(
        value_of(&ts, "ebpf_edge_connections_total", bucket).await,
        2.0
    );
    assert_eq!(
        read_watermark(sql.as_ref()).await.unwrap(),
        Some((later + 120_000_000 - 60_000_000) / 60_000_000 * 60_000_000)
    );
}

/// 边查询：`source` 单路、缺省合并汇总、过滤器与非法时间范围。
#[tokio::test]
async fn edge_search_merges_sources_and_filters() {
    use crate::query::{search_edges, EdgeSearchQuery, SOURCE_MERGED};

    let dir = tempfile::tempdir().unwrap();
    let sql = store(dir.path());
    crate::bootstrap(sql.as_ref()).await.unwrap();

    let bucket = 1_710_000_000_000_000i64;
    // OTLP 侧一条（无协议）。
    sql.execute(
        &format!(
            "INSERT INTO {} (bucket_start, src_service, dst_service, span_kind, calls, errors,
                duration_sum, duration_max, agent_id, data_id) VALUES (?1,'order-api','pay-api','client',4,1,400,200,'a1','i1')",
            crate::tables::EDGE_SUMMARY
        ),
        &[SqlValue::Integer(bucket)],
    )
    .await
    .unwrap();
    // eBPF 侧同一条逻辑边（带协议），以及一条不同目标。
    for (rid, dst, proto, conn, fail) in [
        ("e1", "pay-api", "tcp", 6, 2),
        ("e2", "search-api", "tcp", 1, 0),
    ] {
        sql.execute(
            &format!(
                "INSERT INTO {} (record_id, bucket_start, bucket_micros, protocol, src_ip, src_port,
                    dst_ip, dst_port, src_service, dst_service, connections, bytes_sent, bytes_recv,
                    duration_sum, duration_max, tcp_retrans, tcp_resets, failures, failure_reason,
                    latency_hist, agent_id, data_id)
                 VALUES (?1,?2,10000000,?3,'10.0.0.5',40000,'10.0.0.9',8080,'order-api',?4,?5,100,200,600,300,1,0,?6,'refused','[1]','a2','i2')",
                crate::tables::EBPF_EDGES
            ),
            &[
                SqlValue::Text(rid.to_string()),
                SqlValue::Integer(bucket),
                SqlValue::Text(proto.to_string()),
                SqlValue::Text(dst.to_string()),
                SqlValue::Integer(conn),
                SqlValue::Integer(fail),
            ],
        )
        .await
        .unwrap();
    }

    let query = |source: Option<&str>, protocol: Option<&str>| EdgeSearchQuery {
        from_ts: Some(bucket - 60_000_000),
        to_ts: Some(bucket + 60_000_000),
        src_service: None,
        dst_service: None,
        src_ip: None,
        dst_ip: None,
        dst_port: None,
        protocol: protocol.map(str::to_string),
        source: source.map(str::to_string),
        agent_id: None,
        min_requests: None,
        limit: Some(10),
        offset: None,
    };

    // 单路。
    let otlp = search_edges(sql.as_ref(), &query(Some("otlp"), None))
        .await
        .unwrap();
    assert_eq!(otlp.total, 1);
    assert_eq!(otlp.edges[0].calls, 4);
    assert_eq!(otlp.edges[0].source, "otlp");
    let ebpf = search_edges(sql.as_ref(), &query(Some("ebpf"), None))
        .await
        .unwrap();
    assert_eq!(ebpf.total, 2);
    assert!(ebpf.edges.iter().all(|row| row.source == "ebpf"));
    let pay = ebpf
        .edges
        .iter()
        .find(|row| row.dst_service == "pay-api")
        .unwrap();
    assert_eq!(pay.connections, 6);
    assert_eq!(pay.failures, 2);
    assert_eq!(pay.bytes_sent, 100);
    assert_eq!(pay.dst_port, 8080);
    assert_eq!(
        pay.protocol, "tcp",
        "协议是行的字段（OTLP 侧为空，取 eBPF 侧的值）"
    );
    assert_eq!(pay.tcp_retrans, 1);
    assert_eq!(pay.duration_avg_micros, 100, "600 / 6");
    assert!(pay.span_kind.is_empty(), "eBPF 不产生 span");

    // 合并：同一条逻辑边相加，且标为 merged。
    let merged = search_edges(sql.as_ref(), &query(None, None))
        .await
        .unwrap();
    assert_eq!(merged.total, 2, "order-api→pay-api 合并成一行，另一条独立");
    let pay = merged
        .edges
        .iter()
        .find(|row| row.dst_service == "pay-api")
        .unwrap();
    assert_eq!(pay.source, SOURCE_MERGED);
    assert_eq!(pay.calls, 10, "4 (otlp) + 6 (ebpf)");
    assert_eq!(pay.errors, 3);
    assert_eq!(pay.duration_sum, 1_000);
    assert_eq!(pay.bytes_sent, 100, "OTLP 侧没有字节，只有 eBPF 侧贡献");
    assert_eq!(
        pay.protocol, "tcp",
        "协议是行的字段（OTLP 侧为空，取 eBPF 侧的值）"
    );

    // 协议过滤只影响 eBPF 侧。
    let by_proto = search_edges(sql.as_ref(), &query(None, Some("tcp")))
        .await
        .unwrap();
    assert_eq!(by_proto.total, 2, "OTLP 侧无协议，被过滤掉后只剩 eBPF 两条");

    // 非法时间范围 → invalid_argument。
    let mut bad = query(None, None);
    bad.from_ts = Some(bucket + 60_000_000);
    bad.to_ts = Some(bucket);
    let err = search_edges(sql.as_ref(), &bad).await.unwrap_err();
    assert_eq!(err.code, dataplane_core::ErrorCode::InvalidArgument);
}

/// 能力状态读取：区分「不可用」与「没上报」，标签缺失不 panic。
#[tokio::test]
async fn capability_report_reads_latest_state() {
    use crate::ebpf_metrics::capability_report;

    let dir = tempfile::tempdir().unwrap();
    let ts = ts_store(dir.path());

    // 没上报时是空报告（前端据此显示「无 Agent 上报」而不是「不可用」）。
    let empty = capability_report(ts.as_ref()).await.unwrap();
    assert_eq!(empty.reported, 0);
    assert!(empty.agents.is_empty());

    // 用「现在」附近的时间戳：能力是状态，查询按回看窗口取最新样本（测试里不能写死历史时间）。
    let now = crate::now_micros();
    let mut tags = std::collections::BTreeMap::new();
    for (k, v) in [
        ("agent_id", "agent-1"),
        ("item_id", "item-ebpf"),
        ("kernel_ok", "false"),
        ("btf_ok", "true"),
        ("capability_ok", "true"),
        ("kernel_release", "5.4.0"),
        ("reason", "eBPF preflight failed: kernel >= 5.8"),
    ] {
        tags.insert(k.to_string(), v.to_string());
    }
    ts.write(dataplane_ts::TsPoint {
        measurement: "agent_ebpf_capability".into(),
        tags: tags.clone(),
        field_name: "available".into(),
        field_value: 0.0,
        timestamp: now - 3_600_000_000,
    })
    .await
    .unwrap();

    let report = capability_report(ts.as_ref()).await.unwrap();
    assert_eq!(report.reported, 1);
    let entry = &report.agents[0];
    assert_eq!(entry.agent_id, "agent-1");
    assert!(!entry.available);
    assert!(!entry.kernel_ok);
    assert!(entry.btf_ok);
    assert!(entry.capability_ok);
    assert_eq!(entry.kernel_release, "5.4.0");
    assert!(entry.reason.contains("kernel >= 5.8"));

    // 后来变得可用：同标签写 1 覆盖旧值。
    ts.write(dataplane_ts::TsPoint {
        measurement: "agent_ebpf_capability".into(),
        tags,
        field_name: "available".into(),
        field_value: 1.0,
        timestamp: now - 60_000_000,
    })
    .await
    .unwrap();
    let report = capability_report(ts.as_ref()).await.unwrap();
    assert_eq!(report.reported, 1, "同标签是覆盖，不会变成两条");
    assert!(report.agents[0].available);
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

#[test]
fn query_limit_and_window_helpers() {
    use crate::query::{
        clamp_limit, resolve_window, DEFAULT_LIMIT, DEFAULT_WINDOW_MICROS, MAX_LIMIT,
    };

    assert_eq!(clamp_limit(None), DEFAULT_LIMIT);
    assert_eq!(clamp_limit(Some(0)), 1);
    assert_eq!(clamp_limit(Some(10)), 10);
    assert_eq!(clamp_limit(Some(usize::MAX)), MAX_LIMIT);

    let (from, to) = resolve_window(None, None, NOW).unwrap();
    assert_eq!(to, NOW);
    assert_eq!(from, NOW - DEFAULT_WINDOW_MICROS, "缺省最近 1 小时");
    let (from, to) = resolve_window(Some(1), None, NOW).unwrap();
    assert_eq!((from, to), (1, NOW));
    assert!(resolve_window(Some(10), Some(1), NOW).is_err());
}

fn apm_retention_config(max_bytes: u64) -> crate::retention::ApmRetentionConfig {
    crate::retention::ApmRetentionConfig {
        default_retention_days: 3,
        endpoint_retention_days: 30,
        max_bytes,
        evict_step_secs: 3_600,
        max_rounds: 24,
    }
}

async fn seed_trace_row(sql: &dyn RelationalStore, trace_id: &str, start_ts: i64) {
    sql.execute(
        "INSERT OR REPLACE INTO apm_trace_summary (trace_id, start_ts, max_end_ts, duration_micros,
            root_service, root_operation, root_start_ts, span_count, error_count, status,
            services_json, collector, agent_id, host_id, data_id, updated_ts)
         VALUES (?1,?2,?2,1,'svc','op',?2,1,0,'ok','[\"svc\"]','otlp','agent-1','','item-1',?2)",
        &[
            SqlValue::Text(trace_id.to_string()),
            SqlValue::Integer(start_ts),
        ],
    )
    .await
    .unwrap();
}

async fn seed_edge_row(sql: &dyn RelationalStore, bucket: i64) {
    sql.execute(
        "INSERT OR REPLACE INTO apm_edge_summary (bucket_start, src_service, dst_service, span_kind,
            calls, errors, duration_sum, duration_max, agent_id, data_id)
         VALUES (?1,'gw','svc','server',1,0,10,10,'agent-1','item-1')",
        &[SqlValue::Integer(bucket)],
    )
    .await
    .unwrap();
}

async fn seed_trace_detail(log: &dyn LogStore, id: &str, ts: i64) {
    let mut labels = std::collections::BTreeMap::new();
    labels.insert("data_type".to_string(), "traces".to_string());
    labels.insert("trace_id".to_string(), "a".repeat(32));
    labels.insert("data_id".to_string(), "item-1".to_string());
    log.append(dataplane_log::LogRecord {
        id: id.to_string(),
        timestamp: ts,
        level: "info".into(),
        message: "span".into(),
        labels,
        payload: None,
    })
    .await
    .unwrap();
}

async fn seed_log_line(log: &dyn LogStore, id: &str, ts: i64) {
    let mut labels = std::collections::BTreeMap::new();
    labels.insert("data_type".to_string(), "logs".to_string());
    labels.insert("data_id".to_string(), "log-1".to_string());
    log.append(dataplane_log::LogRecord {
        id: id.to_string(),
        timestamp: ts,
        level: "info".into(),
        message: "line".into(),
        labels,
        payload: None,
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn retention_expires_old_apm_data_and_keeps_other_types() {
    use crate::retention::ApmRetention;
    use dataplane_log::TantivyLogStore;

    let dir = tempfile::tempdir().unwrap();
    let sql = store(dir.path());
    bootstrap(sql.as_ref()).await.unwrap();
    let ts = ts_store(dir.path());
    let log = TantivyLogStore::new(dir.path().join("logs")).unwrap();

    let old = NOW - 10 * 86_400 * 1_000_000;
    let fresh = NOW - 60_000_000;
    seed_trace_row(sql.as_ref(), "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", old).await;
    seed_trace_row(sql.as_ref(), "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", fresh).await;
    seed_edge_row(sql.as_ref(), old).await;
    seed_edge_row(sql.as_ref(), fresh).await;
    seed_trace_detail(&log, "d-old", old).await;
    seed_trace_detail(&log, "d-fresh", fresh).await;
    seed_log_line(&log, "l-old", old).await;
    sql.execute(
        "INSERT INTO apm_service_endpoint (service, instance_id, pod_name, node_name, host_ip,
            listen_port, collector, first_seen_ts, last_seen_ts)
         VALUES ('stale','stale-1','','','',0,'otlp',?1,?1)",
        &[SqlValue::Integer(NOW - 40 * 86_400 * 1_000_000)],
    )
    .await
    .unwrap();

    let retention = ApmRetention::new(apm_retention_config(0));
    let report = retention
        .run(sql.as_ref(), &log, ts.as_ref(), dir.path(), NOW)
        .await
        .unwrap();

    assert_eq!(report.summaries_deleted, 1, "只删过期的摘要");
    assert_eq!(report.edges_deleted, 1);
    assert_eq!(report.details_deleted, 1, "只删过期的 traces 明细");
    assert_eq!(report.endpoints_deleted, 1, "40 天前的端点超过 30 天");
    assert_eq!(report.stopped_reason, None);

    // 新数据保留；logs 类型的旧数据不动。
    let traces = sql
        .execute("SELECT trace_id FROM apm_trace_summary", &[])
        .await
        .unwrap();
    assert_eq!(traces.rows.len(), 1);
    assert_eq!(
        traces.rows[0][0],
        SqlValue::Text("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into())
    );
    let remaining: Vec<String> = log
        .search(dataplane_log::LogFilter::default())
        .await
        .unwrap()
        .iter()
        .map(|r| r.id.clone())
        .collect();
    assert!(remaining.contains(&"d-fresh".to_string()));
    assert!(
        remaining.contains(&"l-old".to_string()),
        "logs 不受 APM 保留策略影响"
    );
    assert!(!remaining.contains(&"d-old".to_string()));
}

#[tokio::test]
async fn retention_evicts_oldest_when_over_size_budget() {
    use crate::retention::ApmRetention;
    use dataplane_log::TantivyLogStore;

    let dir = tempfile::tempdir().unwrap();
    let sql = store(dir.path());
    bootstrap(sql.as_ref()).await.unwrap();
    let ts = ts_store(dir.path());
    let log = TantivyLogStore::new(dir.path().join("logs")).unwrap();

    // 3 小时粒度递增的旧数据（都在默认保留期内，只有 size 上限才会删它们）。
    let base = NOW - 6 * 3_600 * 1_000_000;
    for i in 0..6 {
        let ts_i = base + i * 3_600 * 1_000_000;
        seed_trace_row(sql.as_ref(), &format!("{:0>32}", i), ts_i).await;
        seed_edge_row(sql.as_ref(), ts_i).await;
        seed_trace_detail(&log, &format!("d-{i}"), ts_i).await;
    }

    // 预算设为 1 字节：必然超限，只能一路淘汰到没有 APM 数据可删。
    let retention = ApmRetention::new(apm_retention_config(1));
    let report = retention
        .run(sql.as_ref(), &log, ts.as_ref(), dir.path(), NOW)
        .await
        .unwrap();

    assert!(report.evict_rounds >= 1, "应有淘汰轮次: {report:?}");
    assert_eq!(
        report.stopped_reason.as_deref(),
        Some("no_apm_data_left"),
        "APM 数据删完后应停止而不是继续越权删除: {report:?}"
    );
    assert!(report.bytes_after <= report.bytes_before, "{report:?}");

    // 最久远的先被删掉；因为一直删到没数据，最后一条也留不下。
    let left = sql
        .execute("SELECT COUNT(*) FROM apm_trace_summary", &[])
        .await
        .unwrap();
    assert_eq!(
        left.rows[0][0],
        SqlValue::Integer(0),
        "超限会一路淘汰最久远的"
    );
    let logs_left = log
        .search(dataplane_log::LogFilter::default())
        .await
        .unwrap();
    assert!(logs_left.is_empty(), "只删 traces 明细");
}

#[tokio::test]
async fn retention_without_budget_leaves_fresh_data_alone() {
    use crate::retention::ApmRetention;
    use dataplane_log::TantivyLogStore;

    let dir = tempfile::tempdir().unwrap();
    let sql = store(dir.path());
    bootstrap(sql.as_ref()).await.unwrap();
    let ts = ts_store(dir.path());
    let log = TantivyLogStore::new(dir.path().join("logs")).unwrap();
    seed_trace_row(sql.as_ref(), &"c".repeat(32), NOW - 60_000_000).await;

    let retention = ApmRetention::new(apm_retention_config(0));
    let report = retention
        .run(sql.as_ref(), &log, ts.as_ref(), dir.path(), NOW)
        .await
        .unwrap();
    assert_eq!(report.evict_rounds, 0, "未配置上限就不做容量淘汰");
    assert!(report.stopped_reason.is_none());
    let left = sql
        .execute("SELECT COUNT(*) FROM apm_trace_summary", &[])
        .await
        .unwrap();
    assert_eq!(left.rows[0][0], SqlValue::Integer(1), "保留期内不删");
    assert!(crate::retention::dir_size(dir.path()) > 0, "目录计量可用");
}

#[tokio::test]
async fn alias_cache_matches_by_priority_and_invalidates() {
    use crate::alias::{
        alias_id, set_alias_enabled, upsert_alias, AliasCache, AliasMatchKind, AliasUpsert,
    };

    let dir = tempfile::tempdir().unwrap();
    let sql = store(dir.path());
    bootstrap(sql.as_ref()).await.unwrap();
    let cache = AliasCache::new(60);
    let host = "10.0.0.9";
    let pod = "order-api-7c9f";
    let process = "java";

    assert_eq!(
        cache
            .resolve(sql.as_ref(), host, pod, process)
            .await
            .unwrap(),
        None,
        "空表未命中"
    );

    // cidr 先建立。
    upsert_alias(
        sql.as_ref(),
        &AliasUpsert {
            match_kind: "cidr".into(),
            match_value: "10.0.0.0/8".into(),
            service: "legacy".into(),
            ..AliasUpsert::default()
        },
        NOW,
    )
    .await
    .unwrap();
    cache.invalidate();
    assert_eq!(
        cache
            .resolve(sql.as_ref(), host, pod, process)
            .await
            .unwrap(),
        Some("legacy".into())
    );

    // pod_prefix 优先于 cidr。
    upsert_alias(
        sql.as_ref(),
        &AliasUpsert {
            match_kind: "pod_prefix".into(),
            match_value: "order-api".into(),
            service: "order-api".into(),
            ..AliasUpsert::default()
        },
        NOW + 1,
    )
    .await
    .unwrap();
    cache.invalidate();
    assert_eq!(
        cache
            .resolve(sql.as_ref(), host, pod, process)
            .await
            .unwrap(),
        Some("order-api".into())
    );

    // process_name 优先于 pod_prefix。
    upsert_alias(
        sql.as_ref(),
        &AliasUpsert {
            match_kind: "process_name".into(),
            match_value: "java".into(),
            service: "java-svc".into(),
            ..AliasUpsert::default()
        },
        NOW + 2,
    )
    .await
    .unwrap();
    cache.invalidate();
    assert_eq!(
        cache
            .resolve(sql.as_ref(), host, pod, process)
            .await
            .unwrap(),
        Some("java-svc".into())
    );

    // 停用后回落到 pod_prefix。
    let id = alias_id(AliasMatchKind::ProcessName, "java");
    assert!(set_alias_enabled(sql.as_ref(), &id, false, NOW + 3)
        .await
        .unwrap());
    cache.invalidate();
    assert_eq!(
        cache
            .resolve(sql.as_ref(), host, pod, process)
            .await
            .unwrap(),
        Some("order-api".into())
    );

    // 进程名不匹配时不会命中 process_name。
    assert_eq!(
        cache
            .resolve(sql.as_ref(), "192.168.1.1", "", "python")
            .await
            .unwrap(),
        None,
        "cidr 不匹配且无其它规则"
    );
}

#[tokio::test]
async fn edge_target_normalization_prefers_alias_over_endpoint() {
    use crate::alias::AliasUpsert;
    use dataplane_ingest::trace::TraceSink;

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

    // 端点表登记的 10.0.0.9:8080 = endpoint-svc。
    let mut registered = span(
        "cccc0000000000000000000000000001",
        "0000000000000001",
        "",
        "endpoint-svc",
        "GET /orders",
        NOW,
        10,
        "ok",
    );
    registered
        .resource
        .insert("host.ip".into(), "10.0.0.9".into());
    registered
        .attributes
        .insert("server.port".into(), "8080".into());
    sink.observe_span(&registered, &envelope()).await.unwrap();
    sink.flush_now(NOW).await.unwrap();
    assert_eq!(
        sink.lookup_service("10.0.0.9", 8080, "").await.unwrap(),
        Some("endpoint-svc".into())
    );

    // 静态映射把同一网段归一为 alias-svc。
    sink.upsert_alias(
        &AliasUpsert {
            match_kind: "cidr".into(),
            match_value: "10.0.0.0/8".into(),
            service: "alias-svc".into(),
            ..AliasUpsert::default()
        },
        NOW,
    )
    .await
    .unwrap();

    // 一条配不上对的 client span（dst 兜底为 unknown:10.0.0.9）flush 后应归一为 alias-svc。
    let unpaired = edge_span(
        "cccc0000000000000000000000000002",
        "0000000000000002",
        "0000000000000000",
        "client",
        "gateway",
        NOW,
        500,
        "ok",
        &[("server.address", "10.0.0.9"), ("server.port", "8080")],
    );
    sink.observe_span(&unpaired, &envelope()).await.unwrap();
    // 兜底边要等桶关闭才产生。
    sink.flush_now(NOW + 61_000_000).await.unwrap();

    let rows = edge_rows(sql.as_ref()).await;
    let dsts: Vec<String> = rows.iter().map(|r| as_text(&r[2])).collect();
    assert!(
        dsts.contains(&"alias-svc".to_string()),
        "静态映射优先于端点表: {dsts:?}"
    );
}

async fn seed_summary_full(sql: &dyn RelationalStore, trace_id: &str, start_ts: i64) {
    sql.execute(
        "INSERT OR REPLACE INTO apm_trace_summary (trace_id, start_ts, max_end_ts, duration_micros,
            root_service, root_operation, root_start_ts, span_count, error_count, status,
            services_json, collector, agent_id, host_id, data_id, updated_ts)
         VALUES (?1,?2,?2,1,'order-api','GET /orders',?2,1,0,'ok','[\"order-api\"]','otlp','agent-1','host-1','item-1',?2)",
        &[
            SqlValue::Text(trace_id.to_string()),
            SqlValue::Integer(start_ts),
        ],
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn trace_list_exposes_origin_fields() {
    use crate::query::{search_traces, TraceSearchQuery};

    let dir = tempfile::tempdir().unwrap();
    let sql = store(dir.path());
    bootstrap(sql.as_ref()).await.unwrap();
    seed_summary_full(sql.as_ref(), &"a".repeat(32), NOW - 60_000_000).await;

    let page = search_traces(
        sql.as_ref(),
        &TraceSearchQuery {
            from_ts: Some(NOW - 120_000_000),
            to_ts: Some(NOW),
            ..TraceSearchQuery::default()
        },
    )
    .await
    .unwrap();
    let row = &page.traces[0];
    assert_eq!(row.collector, "otlp");
    assert_eq!(row.agent_id, "agent-1");
    assert_eq!(row.host_id, "host-1");
    assert_eq!(
        row.data_id, "item-1",
        "列表要能看到来源与归属，便于定位采集项"
    );
}

#[tokio::test]
async fn trace_detail_prefers_full_payload_and_derives_duration() {
    use crate::query::get_trace;
    use dataplane_log::TantivyLogStore;

    let dir = tempfile::tempdir().unwrap();
    let sql = store(dir.path());
    bootstrap(sql.as_ref()).await.unwrap();
    let log = TantivyLogStore::new(dir.path().join("logs")).unwrap();
    let trace_id = "a".repeat(32);
    let span_id = "b".repeat(16);
    seed_summary_full(sql.as_ref(), &trace_id, NOW - 60_000_000).await;
    seed_summary_full(sql.as_ref(), &trace_id, NOW - 60_000_000).await;

    // 一条带完整原文的 span 明细。
    let mut labels = std::collections::BTreeMap::new();
    labels.insert("data_type".to_string(), "traces".to_string());
    labels.insert("trace_id".to_string(), trace_id.clone());
    labels.insert("service".to_string(), "order-api".to_string());
    labels.insert("data_id".to_string(), "item-1".to_string());
    let payload = json!({
        "record_id": format!("{trace_id}:{span_id}"),
        "trace_id": trace_id,
        "span_id": span_id,
        "parent_span_id": "",
        "name": "GET /orders",
        "kind": "server",
        "service": "order-api",
        "status_code": "error",
        "status_message": "upstream timeout",
        "start_unix_nano": 1_700_000_000_000_000_000i64,
        "end_unix_nano": 1_700_000_000_012_000_000i64,
        "attributes": {"http.request.method": "GET"},
        "events": [{"name": "exception", "time_unix_nano": 1i64, "attributes": {}}],
        "links": [],
        "resource": {"k8s.pod.name": "order-api-1"},
        "dropped_events": 2
    });
    log.append(dataplane_log::LogRecord {
        id: format!("{trace_id}:{span_id}"),
        timestamp: NOW - 60_000_000,
        level: "error".into(),
        message: "order-api GET /orders 12000us".into(),
        labels,
        payload: Some(payload.to_string()),
    })
    .await
    .unwrap();

    let detail = get_trace(sql.as_ref(), &log, &trace_id, 0).await.unwrap();
    assert!(!detail.partial, "明细齐全时不应 partial");
    let span = &detail.spans[0];
    assert_eq!(
        span["duration_micros"],
        json!(12_000),
        "瀑布图需要直接可用的耗时: {span}"
    );
    assert_eq!(span["attributes"]["http.request.method"], json!("GET"));
    assert_eq!(span["events"][0]["name"], json!("exception"));
    assert_eq!(span["resource"]["k8s.pod.name"], json!("order-api-1"));
    assert_eq!(span["status_message"], json!("upstream timeout"));
    assert_eq!(span["dropped_events"], json!(2));
    assert_eq!(span["parent_span_id"], json!(""), "原文保留父 span 关系");
}

#[tokio::test]
async fn service_list_includes_endpoint_instances() {
    use crate::query::list_services;

    let dir = tempfile::tempdir().unwrap();
    let sql = store(dir.path());
    bootstrap(sql.as_ref()).await.unwrap();
    for (instance, pod, host, port) in [
        ("order-api-1", "order-api-7c9f", "10.0.0.9", 8080i64),
        ("order-api-2", "order-api-8d0a", "10.0.0.10", 8080),
    ] {
        sql.execute(
            "INSERT INTO apm_service_endpoint (service, instance_id, pod_name, node_name, host_ip,
                listen_port, collector, first_seen_ts, last_seen_ts)
             VALUES ('order-api',?1,?2,'node-1',?3,?4,'otlp',?5,?5)",
            &[
                SqlValue::Text(instance.to_string()),
                SqlValue::Text(pod.to_string()),
                SqlValue::Text(host.to_string()),
                SqlValue::Integer(port),
                SqlValue::Integer(NOW),
            ],
        )
        .await
        .unwrap();
    }

    let services = list_services(sql.as_ref()).await.unwrap();
    assert_eq!(services.len(), 1);
    let row = &services[0];
    assert_eq!(row.service, "order-api");
    assert_eq!(row.instance_count, 2);
    assert_eq!(row.instances.len(), 2, "端点实例要能下钻查看");
    let instance = row
        .instances
        .iter()
        .find(|i| i.instance_id == "order-api-1")
        .unwrap();
    assert_eq!(instance.pod_name, "order-api-7c9f");
    assert_eq!(instance.node_name, "node-1");
    assert_eq!(instance.host_ip, "10.0.0.9");
    assert_eq!(instance.listen_port, 8080);
    assert_eq!(instance.collector, "otlp");
    assert_eq!(instance.first_seen_ts, NOW);
}
