//! 外置存储 vs 本地存储的回放/写入对比基准（`#[ignore]`，需要真实 cloud3 组件）。
//!
//! 前置：源 tsink 数据目录必须是**拷贝**（打开它会重放 WAL 并改写目录）。
//!
//! ```text
//! RUN_ID=$(date +%H%M%S)
//! scp -r cloud3:/opt/vectorman-k8s/dataserver/ts /tmp/ts-bench-$RUN_ID
//! DP_BENCH_TS_SRC=/tmp/ts-bench-$RUN_ID \
//! DP_BENCH_VM_INSERT_URL=http://127.0.0.1:18480 \
//! DP_BENCH_VM_SELECT_URL=http://127.0.0.1:18481 \
//! DP_BENCH_REDIS_URL=redis://:pw@127.0.0.1:16379/0 \
//! cargo test --release -p dataserver --test replay_bench -- --ignored --nocapture --test-threads=1
//! ```
//!
//! 指标口径：
//! - **本地回放耗时**：打开 tsink 目录（WAL 重放）的墙钟时间 —— 就是上一轮 347s 那个数。
//! - **外置回放耗时**：VM 里数据已在，dataserver 启动不做本地回放，仅建连接 ≈ 0；
//!   所以外置的代价体现在下面的**写入吞吐**，不在这项。
//! - **写入吞吐**：同一批点，分别写入新建的本地 tsink 与 VM，取写入循环的净时间。
//! - **VM 侧**：跑前/跑后读 `vm_rows_inserted_total` 差值（vminsert 指标），用于核对
//!   实际入库点数与丢弃/合并情况。

use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dataplane_adapter_redis::RedisKvStore;
use dataplane_adapter_victoriametrics::{VictoriaMetricsStore, VmConfig};
use dataplane_log::TantivyLogStore;
use dataplane_ts::{TimeSeriesStore, TsPoint, TsRetentionConfig, TsinkTimeSeriesStore};
use tsink::{Storage, StorageBuilder, TimestampPrecision, Value};

fn now_micros() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn open_source(dir: &str) -> (std::sync::Arc<dyn Storage>, Duration) {
    let started = Instant::now();
    let storage = StorageBuilder::new()
        .with_data_path(dir)
        .with_timestamp_precision(TimestampPrecision::Microseconds)
        .with_background_fail_fast(false)
        .build()
        .expect("open source tsink (must be a copy, not the live dir)");
    let elapsed = started.elapsed();
    // 触发一次全量序列枚举，把「重放 + 索引重建」都算进本地回放耗时。
    let series = storage.list_metrics_with_wal().expect("list series");
    eprintln!(
        "local open+replay: {:.3}s, series={}",
        started.elapsed().as_secs_f64(),
        series.len()
    );
    (storage, elapsed)
}

fn to_ts_point(series: &tsink::MetricSeries, dp: &tsink::DataPoint) -> Option<TsPoint> {
    let value = match dp.value {
        Value::F64(v) => v,
        Value::I64(v) => v as f64,
        Value::U64(v) => v as f64,
        Value::Bool(v) => {
            if v {
                1.0
            } else {
                0.0
            }
        }
        _ => return None,
    };
    let tags: BTreeMap<String, String> = series
        .labels
        .iter()
        .map(|l| (l.name.clone(), l.value.clone()))
        .collect();
    Some(TsPoint {
        measurement: series.name.clone(),
        tags,
        field_name: "value".to_string(),
        field_value: value,
        timestamp: dp.timestamp,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 cloud3 VM/Redis 与 tsink 数据目录拷贝"]
async fn replay_local_vs_victoriametrics() {
    let src = std::env::var("DP_BENCH_TS_SRC").expect("DP_BENCH_TS_SRC");
    let vm_insert = std::env::var("DP_BENCH_VM_INSERT_URL").expect("DP_BENCH_VM_INSERT_URL");
    let vm_select = std::env::var("DP_BENCH_VM_SELECT_URL").unwrap_or_else(|_| vm_insert.clone());
    let chunk_series = env_usize("DP_BENCH_SERIES_CHUNK", 200);

    let (source, local_replay) = open_source(&src);
    let series = source.list_metrics_with_wal().expect("list series");
    eprintln!("series total: {}", series.len());
    let start = 1i64;
    let end = now_micros() + 86_400_000_000;

    // --- 统计源点集规模（不计入写入计时） ---
    let mut points_total = 0u64;
    for chunk in series.chunks(chunk_series) {
        let rows = source
            .select_many(chunk, start, end)
            .expect("read source points");
        points_total += rows.iter().map(|r| r.points.len() as u64).sum::<u64>();
    }
    eprintln!("points total: {points_total}");

    // --- VM 侧：批次计数 ---
    let vm = VictoriaMetricsStore::new(VmConfig {
        insert_url: vm_insert.clone(),
        select_url: vm_select.clone(),
        tenant: "0".to_string(),
        single_node: std::env::var("DP_BENCH_VM_SINGLE_NODE").is_ok(),
        ..VmConfig::default()
    })
    .expect("vm store");

    // 只要外置数字时跳过本地写入（cloud3 上本地基线会占满整轮时间）。
    let skip_local = std::env::var("DP_BENCH_SKIP_LOCAL").is_ok();

    let mut local_points = 0u64;
    let mut local_elapsed = Duration::ZERO;
    if !skip_local {
        let local_dir = std::env::temp_dir().join(format!("ts-bench-local-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&local_dir);
        let local = TsinkTimeSeriesStore::new(
            &local_dir,
            TsRetentionConfig {
                retention_days: 0,
                enforced: false,
                ..TsRetentionConfig::default()
            },
        )
        .expect("local target");
        let local_started = Instant::now();
        for chunk in series.chunks(chunk_series) {
            let rows = source.select_many(chunk, start, end).expect("read");
            for row in &rows {
                for dp in &row.points {
                    if let Some(p) = to_ts_point(&row.series, dp) {
                        local.write(p).await.expect("local write");
                        local_points += 1;
                    }
                }
            }
        }
        local_elapsed = local_started.elapsed();
        eprintln!(
            "local write: {} points in {:.3}s = {:.0} pts/s",
            local_points,
            local_elapsed.as_secs_f64(),
            local_points as f64 / local_elapsed.as_secs_f64()
        );
    } else {
        eprintln!("local write: skipped (DP_BENCH_SKIP_LOCAL)");
    }

    // --- 写 VM（同一批点，同一顺序） ---
    let mut vm_points = 0u64;
    let mut queue_full = 0u64;
    let vm_started = Instant::now();
    for chunk in series.chunks(chunk_series) {
        let rows = source.select_many(chunk, start, end).expect("read");
        for row in &rows {
            for dp in &row.points {
                if let Some(p) = to_ts_point(&row.series, dp) {
                    match vm.write(p).await {
                        Ok(()) => vm_points += 1,
                        Err(e) if e.code.as_str() == "unavailable" => {
                            // 背压：这一轮就是把队列容量调准的依据。
                            queue_full += 1;
                            tokio::time::sleep(Duration::from_millis(1)).await;
                        }
                        Err(e) => panic!("vm write: {}: {}", e.code.as_str(), e.message),
                    }
                }
            }
        }
    }
    let enqueue_elapsed = vm_started.elapsed();
    let flush_started = Instant::now();
    vm.flush().await.expect("vm flush");
    let flush_elapsed = flush_started.elapsed();
    let vm_total = enqueue_elapsed + flush_elapsed;
    eprintln!(
        "vm write: {} points, enqueue {:.3}s + flush {:.3}s = {:.3}s = {:.0} pts/s (queue-full retries={})",
        vm_points,
        enqueue_elapsed.as_secs_f64(),
        flush_elapsed.as_secs_f64(),
        vm_total.as_secs_f64(),
        vm_points as f64 / vm_total.as_secs_f64(),
        queue_full
    );

    // --- 留存量核对：VM 侧序列数 ---
    let stats = vm.storage_stats().await.expect("vm stats");
    eprintln!(
        "vm stats: series={} background_errors={} degraded={}",
        stats.series_count, stats.background_errors_total, stats.degraded
    );
    eprintln!(
        "SUMMARY local_replay={:.3}s points={} local_write={:.3}s vm_write={:.3}s vm_pts_per_s={:.0}",
        local_replay.as_secs_f64(),
        points_total,
        local_elapsed.as_secs_f64(),
        vm_total.as_secs_f64(),
        vm_points as f64 / vm_total.as_secs_f64()
    );
    if !skip_local {
        assert_eq!(local_points, points_total, "本地写入点数与源不一致");
    }
    assert!(!stats.degraded, "VM 写入有失败批次");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 cloud3 Redis"]
async fn bench_redis_kv_ops() {
    use dataplane_adapter_redis::{RedisConfig, RedisKvStore};
    use dataplane_kv::KvStore;

    let url = std::env::var("DP_BENCH_REDIS_URL").expect("DP_BENCH_REDIS_URL");
    let n = env_usize("DP_BENCH_REDIS_KEYS", 100_000);
    let store = RedisKvStore::new(RedisConfig {
        url,
        password: None,
        prefix: format!("bench-{}-", std::process::id()),
        ..RedisConfig::default()
    })
    .await
    .expect("redis");

    let ts = now_micros();
    let started = Instant::now();
    for i in 0..n {
        store
            .set(
                format!("ingest/{i:08}").as_bytes(),
                &(ts - 86_400_000_000i64).to_le_bytes(),
            )
            .await
            .expect("set");
    }
    let write_elapsed = started.elapsed();

    let started = Instant::now();
    for i in 0..n {
        assert!(store
            .exists(format!("ingest/{i:08}").as_bytes())
            .await
            .expect("exists"));
    }
    let exists_elapsed = started.elapsed();

    let started = Instant::now();
    let removed = store
        .prune_prefix_before(b"ingest/", ts)
        .await
        .expect("prune");
    let prune_elapsed = started.elapsed();

    eprintln!(
        "redis: set {} keys {:.3}s ({:.0} ops/s); exists {:.3}s ({:.0} ops/s); prune {} keys {:.3}s ({:.0} keys/s)",
        n,
        write_elapsed.as_secs_f64(),
        n as f64 / write_elapsed.as_secs_f64(),
        exists_elapsed.as_secs_f64(),
        n as f64 / exists_elapsed.as_secs_f64(),
        removed,
        prune_elapsed.as_secs_f64(),
        removed as f64 / prune_elapsed.as_secs_f64()
    );
    assert_eq!(removed, n as u64);
}

/// 一条 `metrics` 接入记录（字段与 `dataplane_ingest::MetricsRecord` 对齐）。
fn record_json(p: &TsPoint, idx: usize) -> serde_json::Value {
    serde_json::json!({
        "record_id": format!("rp{idx:08}"),
        "timestamp": p.timestamp,
        "measurement": p.measurement,
        "tags": p.tags,
        "field_name": p.field_name,
        "field_value": p.field_value,
    })
}

/// 跑一个信封（最多 `chunk` 条记录），返回 (accepted, 该批耗时)。
async fn apply_batch(
    vm: &VictoriaMetricsStore,
    log: &TantivyLogStore,
    kv: &RedisKvStore,
    records: Vec<serde_json::Value>,
    seq: usize,
) -> Result<(u64, Duration), dataplane_core::DataplaneError> {
    use dataplane_ingest::{apply_with_sinks, DataEnvelope};
    let envelope: DataEnvelope = serde_json::from_value(serde_json::json!({
        "batch_id": format!("b{seq}"),
        "data_type": "metrics",
        "data_id": "bench",
        "agent_id": "bench",
        "host_id": "bench",
        "sent_at_micros": now_micros(),
        "records": records,
    }))
    .expect("envelope");
    let started = Instant::now();
    let reply = apply_with_sinks(envelope, vm, log, kv, None, None, None).await?;
    Ok((u64::from(reply.accepted), started.elapsed()))
}

/// 进程峰值 RSS（kB），用来证明跑法是有界的。
fn peak_rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .and_then(|l| l.split_whitespace().nth(1).map(|v| v.to_string()))
        })
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// 端到端补数回放：源 `ts/` 拷贝里的每个点 → 一条接入记录 → `apply_with_sinks`
/// （Redis 去重 + VM 写入），量**接入墙钟**与 records/s。
///
/// 这是「去重键拖死迁移」的验收场景：改前每记录两次 Redis 往返。同一轮里还会用
/// 逐条 `exists`+`set` 跑一个小样本，作为**同一实例上的改前天花板**对照。
///
/// **内存有界**：记录按 `chunk` 流式生成、逐批 apply，不把 539 万条记录攒在内存里
/// （2026-11-10 第一版就是这么把 cloud3 打到 11.7GB / OOM 的）。生产节点上跑必须加
/// 上限，peak RSS 会打在 SUMMARY 里：
///
/// ```text
/// systemd-run --scope -p MemoryMax=6G -p MemorySwapMax=0 -- ./replay_bench \
///   bench_ingest_replay_throughput --ignored --nocapture --test-threads=1
/// ```
///
/// 环境变量：`DP_BENCH_INGEST_CHUNK`（信封大小，默认 1000）、
/// `DP_BENCH_INGEST_LIMIT`（只跑前 N 条，0 = 全部）、
/// `DP_BENCH_DEDUP_SINGLE_N`（改前口径样本，默认 20000）、
/// `DP_BENCH_INGEST_KEEP=1`（跳过收尾清理）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 cloud3 Redis/VM 与 tsink 数据目录拷贝"]
async fn bench_ingest_replay_throughput() {
    use dataplane_adapter_redis::{RedisConfig, RedisKvStore};
    use dataplane_kv::KvStore;
    use dataplane_log::TantivyLogStore;

    let src = std::env::var("DP_BENCH_TS_SRC").expect("DP_BENCH_TS_SRC");
    let redis_url = std::env::var("DP_BENCH_REDIS_URL").expect("DP_BENCH_REDIS_URL");
    let insert = std::env::var("DP_BENCH_VM_INSERT_URL").expect("DP_BENCH_VM_INSERT_URL");
    let select = std::env::var("DP_BENCH_VM_SELECT_URL").unwrap_or_else(|_| insert.clone());
    let chunk = env_usize("DP_BENCH_INGEST_CHUNK", 1000).max(1);
    let limit = env_usize("DP_BENCH_INGEST_LIMIT", 0);
    let queue_capacity = env_usize("DP_BENCH_VM_QUEUE_CAPACITY", 500_000).max(chunk);
    let prefix = format!("bench-in-{}-", std::process::id());

    // 打开源 dir 会重放 WAL 并改写目录 —— 必须是拷贝，且这段不计入接入计时。
    let (source, _) = open_source(&src);
    let series = source.list_metrics_with_wal().expect("list series");
    eprintln!("series total: {}", series.len());
    let start = 1i64;
    let end = now_micros() + 86_400_000_000;

    let kv = RedisKvStore::new(RedisConfig {
        url: redis_url,
        password: None,
        prefix: prefix.clone(),
        ..RedisConfig::default()
    })
    .await
    .expect("redis");
    let vm = VictoriaMetricsStore::new(VmConfig {
        insert_url: insert,
        select_url: select,
        tenant: "0".to_string(),
        single_node: std::env::var("DP_BENCH_VM_SINGLE_NODE").is_ok(),
        batch_size: 5000,
        flush_interval: Duration::from_millis(50),
        queue_capacity,
        ..VmConfig::default()
    })
    .expect("vm store");
    let log_dir = std::env::temp_dir().join(format!("bench-ingest-log-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&log_dir);
    let log = TantivyLogStore::new(&log_dir).expect("log store");

    // --- 流式：边读源点边凑信封，一次只有一个信封（chunk 条）在内存里 ---
    let mut accepted = 0u64;
    let mut apply_total = Duration::ZERO;
    let mut seq = 0usize;
    let mut idx = 0usize;
    let mut buf: Vec<serde_json::Value> = Vec::with_capacity(chunk);
    let mut error: Option<dataplane_core::DataplaneError> = None;

    'outer: for chunk_series in series.chunks(200) {
        let rows = source.select_many(chunk_series, start, end).expect("read");
        for row in &rows {
            for dp in &row.points {
                let Some(p) = to_ts_point(&row.series, dp) else {
                    continue;
                };
                if limit > 0 && idx >= limit {
                    break 'outer;
                }
                buf.push(record_json(&p, idx));
                idx += 1;
                if buf.len() >= chunk {
                    let batch = std::mem::take(&mut buf);
                    match apply_batch(&vm, &log, &kv, batch, seq).await {
                        Ok((n, elapsed)) => {
                            accepted += n;
                            apply_total += elapsed;
                        }
                        Err(e) => {
                            error = Some(e);
                            break 'outer;
                        }
                    }
                    seq += 1;
                }
            }
        }
    }
    if error.is_none() && !buf.is_empty() {
        match apply_batch(&vm, &log, &kv, std::mem::take(&mut buf), seq).await {
            Ok((n, elapsed)) => {
                accepted += n;
                apply_total += elapsed;
            }
            Err(e) => error = Some(e),
        }
    }
    eprintln!("records read from source: {idx}, envelopes: {}", seq + 1);

    let flush_started = Instant::now();
    let flush_result = vm.flush().await;
    let flush_elapsed = flush_started.elapsed();
    let total = apply_total + flush_elapsed;
    let rate = accepted as f64 / total.as_secs_f64();
    eprintln!(
        "ingest batch: {} records, apply {:.3}s + flush {:.3}s = {:.3}s = {:.0} records/s",
        accepted,
        apply_total.as_secs_f64(),
        flush_elapsed.as_secs_f64(),
        total.as_secs_f64(),
        rate
    );
    let stats = vm.storage_stats().await;
    if let Ok(stats) = &stats {
        eprintln!(
            "vm stats: background_errors={} degraded={}",
            stats.background_errors_total, stats.degraded
        );
    }

    // --- 改前口径：每记录两次往返，只打 Redis（同一实例、同一前缀） ---
    let single_n = env_usize("DP_BENCH_DEDUP_SINGLE_N", 20_000).min(idx.max(1));
    let single_started = Instant::now();
    for i in 0..single_n {
        let key = format!("single/{i:08}");
        if let Ok(false) = kv.exists(key.as_bytes()).await {
            let _ = kv.set(key.as_bytes(), &now_micros().to_le_bytes()).await;
        }
    }
    let single_elapsed = single_started.elapsed();
    eprintln!(
        "dedup single-roundtrip (改前口径): {} records in {:.3}s = {:.0} records/s",
        single_n,
        single_elapsed.as_secs_f64(),
        single_n as f64 / single_elapsed.as_secs_f64()
    );
    eprintln!(
        "SUMMARY records={} batch_total={:.3}s batch_records_per_s={:.0} single_roundtrip_records_per_s={:.0} peak_rss_kb={}",
        accepted,
        total.as_secs_f64(),
        rate,
        single_n as f64 / single_elapsed.as_secs_f64(),
        peak_rss_kb()
    );

    // --- 收尾：不管成功还是失败都清（第一版失败时跳过清理，在 Redis 里留了 2002 个键） ---
    if std::env::var("DP_BENCH_INGEST_KEEP").is_err() {
        let cleanup = Instant::now();
        let removed = kv
            .prune_prefix_before(b"ingest/", i64::MAX)
            .await
            .unwrap_or_else(|e| {
                eprintln!("cleanup ingest failed: {}: {}", e.code.as_str(), e.message);
                0
            });
        let _ = kv.prune_prefix_before(b"single/", i64::MAX).await;
        eprintln!(
            "cleanup: {} ingest keys in {:.3}s",
            removed,
            cleanup.elapsed().as_secs_f64()
        );
    }

    if let Some(e) = error {
        panic!("ingest batch failed: {}: {}", e.code.as_str(), e.message);
    }
    flush_result.expect("vm flush");
    let stats = stats.expect("vm stats");
    assert!(!stats.degraded, "VM 写入有失败批次");
    let _ = std::fs::remove_dir_all(&log_dir);
}

/// 全量规模的**合成**接入回放：不打开任何源目录，内存 ~百 MB，可在受限 cgroup 里跑。
///
/// 与 `bench_ingest_replay_throughput` 的口径一致（同一条 `apply_with_sinks` 路径、
/// 同样的去重与 VM 写入），只是点集是生成的：`DP_BENCH_RECORDS` 条、`DP_BENCH_SERIES`
/// 条序列、1s 间隔。用来在**不能碰 tsink 大目录**的机器上拿全量墙钟（539 万条）。
///
/// ```text
/// systemd-run --scope -p MemoryMax=2G -p MemorySwapMax=0 -- ./replay_bench \
///   bench_ingest_synthetic --ignored --nocapture --test-threads=1
/// ```
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "需要真实 Redis/VM"]
async fn bench_ingest_synthetic() {
    use dataplane_adapter_redis::{RedisConfig, RedisKvStore};
    use dataplane_kv::KvStore;
    use dataplane_log::TantivyLogStore;

    let redis_url = std::env::var("DP_BENCH_REDIS_URL").expect("DP_BENCH_REDIS_URL");
    let insert = std::env::var("DP_BENCH_VM_INSERT_URL").expect("DP_BENCH_VM_INSERT_URL");
    let select = std::env::var("DP_BENCH_VM_SELECT_URL").unwrap_or_else(|_| insert.clone());
    let total_n = env_usize("DP_BENCH_RECORDS", 5_391_255);
    let series_n = env_usize("DP_BENCH_SERIES", 3_342).max(1);
    let chunk = env_usize("DP_BENCH_INGEST_CHUNK", 1000).max(1);
    let prefix = format!("bench-in-{}-", std::process::id());

    let kv = RedisKvStore::new(RedisConfig {
        url: redis_url,
        password: None,
        prefix: prefix.clone(),
        ..RedisConfig::default()
    })
    .await
    .expect("redis");
    let vm = VictoriaMetricsStore::new(VmConfig {
        insert_url: insert,
        select_url: select,
        tenant: "0".to_string(),
        single_node: std::env::var("DP_BENCH_VM_SINGLE_NODE").is_ok(),
        batch_size: 5000,
        flush_interval: Duration::from_millis(50),
        queue_capacity: env_usize("DP_BENCH_VM_QUEUE_CAPACITY", 500_000).max(chunk),
        ..VmConfig::default()
    })
    .expect("vm store");
    let log_dir = std::env::temp_dir().join(format!("bench-ingest-log-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&log_dir);
    let log = TantivyLogStore::new(&log_dir).expect("log store");

    let base_ts = now_micros() - (total_n as i64 / series_n as i64).max(1) * 1_000_000;
    let mut accepted = 0u64;
    let mut apply_total = Duration::ZERO;
    let mut seq = 0usize;
    let mut error: Option<dataplane_core::DataplaneError> = None;
    let mut buf: Vec<serde_json::Value> = Vec::with_capacity(chunk);

    for i in 0..total_n {
        let s = i % series_n;
        buf.push(serde_json::json!({
            "record_id": format!("rp{i:09}"),
            "timestamp": base_ts + (i as i64 / series_n as i64) * 1_000_000,
            "measurement": format!("bench_syn_{:03}", s % 64),
            "tags": { "series": format!("s{s}"), "agent_id": "bench", "host_id": "bench", "item_id": "bench" },
            "field_name": "value",
            "field_value": (i % 1000) as f64,
        }));
        if buf.len() >= chunk {
            match apply_batch(&vm, &log, &kv, std::mem::take(&mut buf), seq).await {
                Ok((n, elapsed)) => {
                    accepted += n;
                    apply_total += elapsed;
                }
                Err(e) => {
                    error = Some(e);
                    break;
                }
            }
            seq += 1;
        }
    }
    if error.is_none() && !buf.is_empty() {
        if let Ok((n, elapsed)) = apply_batch(&vm, &log, &kv, std::mem::take(&mut buf), seq).await {
            accepted += n;
            apply_total += elapsed;
        }
    }

    let flush_started = Instant::now();
    let flush_result = vm.flush().await;
    let flush_elapsed = flush_started.elapsed();
    let total = apply_total + flush_elapsed;
    let rate = accepted as f64 / total.as_secs_f64();
    let stats = vm.storage_stats().await;
    eprintln!(
        "ingest synthetic: {} records over {} series, apply {:.3}s + flush {:.3}s = {:.3}s = {:.0} records/s",
        accepted,
        series_n,
        apply_total.as_secs_f64(),
        flush_elapsed.as_secs_f64(),
        total.as_secs_f64(),
        rate
    );
    if let Ok(stats) = &stats {
        eprintln!(
            "vm stats: series={} background_errors={} degraded={}",
            stats.series_count, stats.background_errors_total, stats.degraded
        );
    }
    eprintln!(
        "SUMMARY records={} total={:.3}s records_per_s={:.0} peak_rss_kb={}",
        accepted,
        total.as_secs_f64(),
        rate,
        peak_rss_kb()
    );

    if std::env::var("DP_BENCH_INGEST_KEEP").is_err() {
        let cleanup = Instant::now();
        let removed = kv
            .prune_prefix_before(b"ingest/", i64::MAX)
            .await
            .unwrap_or(0);
        eprintln!(
            "cleanup: {} keys in {:.3}s",
            removed,
            cleanup.elapsed().as_secs_f64()
        );
    }
    if let Some(e) = error {
        panic!("ingest batch failed: {}: {}", e.code.as_str(), e.message);
    }
    flush_result.expect("vm flush");
    let _ = std::fs::remove_dir_all(&log_dir);
}
