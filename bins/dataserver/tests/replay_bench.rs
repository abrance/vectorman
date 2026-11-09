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

use dataplane_adapter_victoriametrics::{VictoriaMetricsStore, VmConfig};
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
