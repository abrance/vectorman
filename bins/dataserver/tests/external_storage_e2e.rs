//! 外置存储后端的 e2e：对着真实 Redis / VictoriaMetrics 跑。
//!
//! **缺环境变量一律失败**（P0 修复：原先是 `eprintln!("skip: …"); return;` → 静默假绿，
//! 离线跑 `cargo test` 永远是绿的，没人知道真实链路到底通没通）。缺哪个变量就在 panic
//! 信息里点名。按 `test_plan/IT-EXT-REDIS-VM-01.md` 执行：
//!
//! ```text
//! DP_E2E_REDIS_URL=redis://:pw@127.0.0.1:16379/0 \
//! DP_E2E_VM_INSERT_URL=http://127.0.0.1:18480 \
//! DP_E2E_VM_SELECT_URL=http://127.0.0.1:18481 \
//! cargo test --release -p dataserver --test external_storage_e2e -- --nocapture
//! ```
//!
//! 单机版 VM 加 `DP_E2E_VM_SINGLE_NODE=1`。测试自己清理写入的键/序列前缀
//! （`e2e-{pid}-`），不依赖空库。

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use dataplane_adapter_redis::{RedisConfig, RedisKvStore};
use dataplane_adapter_victoriametrics::{VictoriaMetricsStore, VmConfig};
use dataplane_kv::{KvStore, RedbKvStore};
use dataplane_ts::{
    TimeSeriesStore, TsMatcher, TsMatcherOp, TsPoint, TsRetentionConfig, TsSeriesSelection,
    TsinkTimeSeriesStore,
};

fn now_micros() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

fn point(name: &str, tags: &[(&str, &str)], ts: i64, value: f64) -> TsPoint {
    TsPoint {
        measurement: name.to_string(),
        tags: tags
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect(),
        field_name: "value".to_string(),
        field_value: value,
        timestamp: ts,
    }
}

/// 缺变量 = 失败，不是跳过。返回的 panic 信息必须让执行者一眼看出该补哪个变量。
fn require_env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        panic!(
            "缺少环境变量 {name}：本用例要求真实外置后端，不能静默跳过。\
             按 test_plan/IT-EXT-REDIS-VM-01.md 起 Redis/VictoriaMetrics 后重跑"
        )
    })
}

async fn redis_store() -> RedisKvStore {
    let url = require_env("DP_E2E_REDIS_URL");
    let store = RedisKvStore::new(RedisConfig {
        url,
        password: None,
        prefix: format!("e2e-{}-", std::process::id()),
        ..RedisConfig::default()
    })
    .await
    .expect("redis connect");
    store
}

fn vm_store() -> VictoriaMetricsStore {
    let insert = require_env("DP_E2E_VM_INSERT_URL");
    // 单机版 VM 只需一个基址；集群版要两个。不单独强求 SELECT 变量，
    // 但连不上 VM 时下面的写入/查询会直接失败，仍然是「红」不是「绿」。
    let select = std::env::var("DP_E2E_VM_SELECT_URL").unwrap_or_else(|_| insert.clone());
    VictoriaMetricsStore::new(VmConfig {
        insert_url: insert,
        select_url: select,
        tenant: "0".to_string(),
        single_node: std::env::var("DP_E2E_VM_SINGLE_NODE").is_ok(),
        batch_size: 1_000,
        flush_interval: std::time::Duration::from_millis(50),
        delete_auth_key: std::env::var("DP_E2E_VM_DELETE_AUTH_KEY").ok(),
        whole_series_delete: std::env::var("DP_E2E_VM_DELETE_ENABLED").is_ok(),
        ..VmConfig::default()
    })
    .expect("vm store")
}

#[tokio::test]
async fn redis_kv_semantics_match_local_engine() {
    let store = redis_store().await;

    // 基本 CRUD + 错误口径
    assert!(store
        .get(b"e2e/missing")
        .await
        .unwrap_err()
        .code
        .as_str()
        .eq("not_found"));
    store.set(b"e2e/a", b"1").await.unwrap();
    assert_eq!(store.get(b"e2e/a").await.unwrap(), b"1".to_vec());
    assert!(store.exists(b"e2e/a").await.unwrap());
    store.set(b"e2e/a", b"2").await.unwrap();
    assert_eq!(store.get(b"e2e/a").await.unwrap(), b"2".to_vec());
    store.delete(b"e2e/a").await.unwrap();
    assert!(!store.exists(b"e2e/a").await.unwrap());
    assert!(
        store.delete(b"e2e/a").await.is_err(),
        "删不存在的键要报 not_found"
    );

    // 二进制键值
    let raw_key = b"e2e/raw/\xff\x00\x01".to_vec();
    let raw_val = vec![0u8, 255, 7, 9];
    store.set(&raw_key, &raw_val).await.unwrap();
    assert_eq!(store.get(&raw_key).await.unwrap(), raw_val);

    // scan_prefix 只返回本前缀，键还原成调用方看到的形状
    store.set(b"e2e/s1", b"a").await.unwrap();
    store.set(b"e2e/s2", b"b").await.unwrap();
    store.set(b"other/x", b"c").await.unwrap();
    let rows = store.scan_prefix(b"e2e/").await.unwrap();
    let keys: Vec<String> = rows
        .iter()
        .map(|(k, _)| String::from_utf8_lossy(k).to_string())
        .collect();
    assert!(keys.contains(&"e2e/s1".to_string()), "{keys:?}");
    assert!(keys.contains(&"e2e/s2".to_string()), "{keys:?}");
    assert!(!keys.iter().any(|k| k.starts_with("other/")), "{keys:?}");

    // prune_prefix_before：8 字节小端微秒，旧的删、新的留、坏值也删；跨 SCAN 分批
    let cutoff = now_micros();
    let mut total = 0;
    for i in 0..1_200i64 {
        let ts = if i % 2 == 0 {
            cutoff - 3_600_000_000
        } else {
            cutoff + 3_600_000_000
        };
        store
            .set(format!("e2e/prune/{i:05}").as_bytes(), &ts.to_le_bytes())
            .await
            .unwrap();
        total += 1;
    }
    store.set(b"e2e/prune/bad", b"").await.unwrap();
    let removed = store
        .prune_prefix_before(b"e2e/prune/", cutoff)
        .await
        .unwrap();
    assert_eq!(removed, 601, "600 旧的 + 1 个坏值（共 {total} 键）");
    assert!(!store.exists(b"e2e/prune/00000").await.unwrap());
    assert!(store.exists(b"e2e/prune/00001").await.unwrap());
    assert!(!store.exists(b"e2e/prune/bad").await.unwrap());
}

#[tokio::test]
async fn redis_batch_ops_match_single_ops() {
    let store = redis_store().await;

    // 跨多个 chunk（默认 500）的批量：MSET/MGET 与逐条结果必须一致。
    let pairs: Vec<(Vec<u8>, Vec<u8>)> = (0..1_200u32)
        .map(|i| {
            (
                format!("e2e/batch/{i:05}").into_bytes(),
                (i as i64).to_le_bytes().to_vec(),
            )
        })
        .collect();
    store.set_many(&pairs).await.unwrap();
    for (key, value) in &pairs {
        assert_eq!(&store.get(key).await.unwrap(), value);
    }

    let mut keys: Vec<Vec<u8>> = pairs.iter().map(|(k, _)| k.clone()).collect();
    keys.push(b"e2e/batch/missing".to_vec());
    let hits = store.exists_many(&keys).await.unwrap();
    assert_eq!(hits.len(), keys.len());
    assert!(hits[..1_200].iter().all(|h| *h), "写入的键都应命中");
    assert!(!hits[1_200], "缺失键必须报 false，不是报错");

    // 空批是 no-op（MGET/MSET 不接受零参数，适配器要自己挡）。
    assert!(store.set_many(&[]).await.is_ok());
    assert!(store.exists_many(&[]).await.unwrap().is_empty());

    let removed = store
        .prune_prefix_before(b"e2e/batch/", i64::MAX)
        .await
        .unwrap();
    assert_eq!(removed, 1_200);
}

#[tokio::test]
async fn vm_write_query_delete_roundtrip() {
    let store = vm_store();
    // cloud3 的 vmstorage 带 `-dedup.minScrapeInterval=30s`：同序列 30s 内的点会在查询时
    // 合并成一个（后写胜出）。所以这里按 60s 间距写，验证的是「写入→可见→删除」链路，
    // 而不是分辨率。dedup 对 1s 级数据是**数据可见性**问题，见 docs/external-storage.md §5。
    const STEP_S: i64 = 60;
    const POINTS: i64 = 10;
    let base_ms = (now_micros() / 1_000) - 15 * 60_000;
    let metric = "vectorman_e2e_probe_total";
    // 每次运行一个唯一 `run` label：VM 里的数据保留 30 天，不隔离就会读到上一轮的样本。
    let run = format!("{}-{}", std::process::id(), now_micros());
    let tags_a = [
        ("item_id", "item-e2e-a"),
        ("agent_id", "agent-e2e"),
        ("run", run.as_str()),
    ];
    let tags_b = [
        ("item_id", "item-e2e-b"),
        ("agent_id", "agent-e2e"),
        ("run", run.as_str()),
    ];

    // 两个序列各 10 点，60 秒一个，时间戳带微秒尾巴（VM 会截到毫秒）。
    let mut expected_a = Vec::new();
    for i in 0..POINTS {
        let ts = (base_ms + i * STEP_S * 1_000) * 1_000 + 456;
        let v = i as f64 * 1.5;
        store.write(point(metric, &tags_a, ts, v)).await.unwrap();
        expected_a.push((ts / 1_000 * 1_000, v));
    }
    for i in 0..POINTS {
        let ts = (base_ms + i * STEP_S * 1_000) * 1_000 + 456;
        store
            .write(point(metric, &tags_b, ts, i as f64))
            .await
            .unwrap();
    }
    store.flush().await.expect("flush to victoriametrics");

    // VM 集群是「写 vminsert → 落 vmstorage」的最终一致：刚 flush 完的窗口内查不到是正常的，
    // 等它追平再断言（实测 1s 内可见）。
    let last_ts = (base_ms + (POINTS - 1) * STEP_S * 1_000) * 1_000;
    let expr_a = format!("{metric}{{item_id=\"item-e2e-a\",run=\"{run}\"}}");
    let mut instant = None;
    for _ in 0..60 {
        let r = store
            .query_instant(&expr_a, Some(last_ts))
            .await
            .expect("instant query");
        if !r.result.is_empty() {
            instant = Some(r);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    let instant = instant.unwrap_or_else(|| panic!("5s 后仍查不到刚写入的点：{expr_a}"));
    let series = instant
        .result
        .iter()
        .find(|s| s.metric.get("item_id").map(String::as_str) == Some("item-e2e-a"))
        .expect("item-e2e-a series");
    let (ts, v) = series.value.expect("instant value");
    // VM 的 instant 返回的是**求值时刻**，且精度只到毫秒：写入时 +456µs 被截到毫秒，
    // 采样点与求值时刻同毫秒，因此 ts 就是毫秒对齐的写入时刻。
    assert_eq!(ts, last_ts, "instant 时间戳应还原成毫秒对齐的微秒");
    assert!((v - (POINTS - 1) as f64 * 1.5).abs() < 1e-9, "value={v}");
    assert_eq!(
        series.metric.get("__name__").map(String::as_str),
        Some(metric)
    );

    // range：点集与本地写入逐点一致（求值网格与采样点重合，step = 采样间距）
    let range = store
        .query_range(
            &format!("{metric}{{item_id=\"item-e2e-a\",run=\"{run}\"}}"),
            base_ms * 1_000,
            (base_ms + (POINTS - 1) * STEP_S * 1_000) * 1_000,
            STEP_S,
        )
        .await
        .expect("range query");
    let got: BTreeMap<i64, f64> = range
        .result
        .iter()
        .flat_map(|s| s.values.clone().unwrap_or_default())
        .collect();
    let want: BTreeMap<i64, f64> = expected_a.iter().copied().collect();
    assert_eq!(got, want, "range 结果与写入点集不一致");

    let stats = store.storage_stats().await.expect("stats");
    assert!(stats.series_count > 0, "VM tsdb 应报出序列数");
    assert!(!stats.degraded, "写入未失败不应 degraded");

    // 删除：VM 只能整序列删（不接受 start/end），所以分两条路径：
    // 配了 deleteAuthKey 就验整序列删除生效；没配就验「明确拒绝 + 指到位」。
    let delete_result = store
        .delete_series(TsSeriesSelection {
            measurement: Some(metric.to_string()),
            matchers: vec![
                TsMatcher {
                    name: "item_id".to_string(),
                    op: TsMatcherOp::Equal,
                    value: "item-e2e-a".to_string(),
                },
                TsMatcher {
                    name: "run".to_string(),
                    op: TsMatcherOp::Equal,
                    value: run.clone(),
                },
            ],
            from_ts: (base_ms - STEP_S * 1_000) * 1_000,
            to_ts: (base_ms + POINTS * STEP_S * 1_000) * 1_000,
        })
        .await;
    if std::env::var("DP_E2E_VM_DELETE_AUTH_KEY").is_ok()
        && std::env::var("DP_E2E_VM_DELETE_ENABLED").is_ok()
    {
        let report = delete_result.expect("delete_series（deleteAuthKey 已配）");
        assert!(report.matched_series >= 1, "{report:?}");
        let after = store
            .query_instant(
                &format!("{metric}{{item_id=\"item-e2e-a\",run=\"{run}\"}}"),
                Some(last_ts),
            )
            .await
            .expect("instant after delete");
        assert!(after.result.is_empty(), "删除后不该再查到：{after:?}");
        let kept = store
            .query_instant(
                &format!("{metric}{{item_id=\"item-e2e-b\",run=\"{run}\"}}"),
                Some(last_ts),
            )
            .await
            .expect("instant b");
        assert_eq!(kept.result.len(), 1, "b 序列被误删：{kept:?}");
    } else {
        let err = delete_result
            .expect_err("没有 deleteAuthKey / 未允许整序列删除时必须报错，不能伪装成功");
        assert!(
            err.message.contains("delete_auth_key") || err.message.contains("整序列删除"),
            "错误信息没指出原因: {}",
            err.message
        );
        eprintln!("delete 未启用，已验错误路径：{}", err.message);
    }
}

/// 同一批点写本地 tsink 与 VM，同 PromQL 的结果集一致（毫秒对齐后）。
#[tokio::test]
async fn local_tsink_and_vm_agree_on_same_points() {
    let vm = vm_store();
    let dir = tempfile::tempdir().expect("tempdir");
    let local = TsinkTimeSeriesStore::new(
        dir.path(),
        TsRetentionConfig {
            retention_days: 0,
            enforced: false,
            ..TsRetentionConfig::default()
        },
    )
    .expect("tsink init");

    // 同 probe：60s 间距，避开 cloud3 vmstorage 的 30s dedup。
    let step_s: i64 = 60;
    let points: i64 = 10;
    let base_ms = (now_micros() / 1_000) - 15 * 60_000;
    let metric = "vectorman_e2e_parity_total";
    let run = format!("{}-{}", std::process::id(), now_micros());
    let tag_sets = [
        [
            ("item_id", "item-parity-a"),
            ("agent_id", "agent-parity"),
            ("run", run.as_str()),
        ],
        [
            ("item_id", "item-parity-b"),
            ("agent_id", "agent-parity"),
            ("run", run.as_str()),
        ],
    ];
    // 时间戳对齐到整秒（毫秒对齐、步长对齐）：range 查询返回的是**求值网格**上的时间戳，
    // 网格与采样点重合时两边才能逐点比。
    let mut expected: BTreeMap<(String, i64), f64> = BTreeMap::new();
    for (idx, tags) in tag_sets.iter().enumerate() {
        for i in 0..points {
            let ts = (base_ms + i * step_s * 1_000) * 1_000;
            let v = (i as f64) * (idx as f64 + 1.0);
            local.write(point(metric, tags, ts, v)).await.unwrap();
            vm.write(point(metric, tags, ts, v)).await.unwrap();
            expected.insert((tags[0].1.to_string(), ts), v);
        }
    }
    vm.flush().await.expect("flush vm");

    let expr = format!("{metric}{{agent_id=\"agent-parity\",run=\"{run}\"}}");
    let start = base_ms * 1_000;
    let end = (base_ms + (points - 1) * step_s * 1_000) * 1_000;
    // 等 VM 追平（最终一致）。
    let mut vm_range = None;
    for _ in 0..60 {
        let r = vm
            .query_range(&expr, start, end, step_s)
            .await
            .expect("vm range");
        let got: usize = r
            .result
            .iter()
            .map(|s| s.values.as_ref().map(Vec::len).unwrap_or(0))
            .sum();
        if got >= expected.len() {
            vm_range = Some(r);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    let vm_range = vm_range.expect("VM 15s 内未追平");
    let local_range = local
        .query_range(&expr, start, end, step_s)
        .await
        .expect("local range");

    let flatten = |r: &dataplane_ts::PromResult| -> BTreeMap<(String, i64), f64> {
        r.result
            .iter()
            .flat_map(|s| {
                let item = s.metric.get("item_id").cloned().unwrap_or_default();
                s.values
                    .clone()
                    .unwrap_or_default()
                    .into_iter()
                    .map(move |(ts, v)| ((item.clone(), ts), v))
            })
            .collect()
    };
    let local_points = flatten(&local_range);
    let vm_points = flatten(&vm_range);
    assert_eq!(local_points, expected, "本地 tsink 结果与写入不一致");
    assert_eq!(vm_points, expected, "VM 结果与本地结果不一致");
}

#[tokio::test]
async fn redb_baseline_still_works() {
    // 对照组：同一套语义测试跑本地 redb，保证适配器没把调用口径带偏。
    let dir = tempfile::tempdir().unwrap();
    let store = RedbKvStore::new(dir.path().join("kv.redb")).unwrap();
    store.set(b"ingest/a", &1i64.to_le_bytes()).await.unwrap();
    assert_eq!(
        store.get(b"ingest/a").await.unwrap(),
        1i64.to_le_bytes().to_vec()
    );
    let removed = store.prune_prefix_before(b"ingest/", 10).await.unwrap();
    assert_eq!(removed, 1);
}

/// 真 HTTP 链路：`POST /v1/ingest` → Redis（流索引/去重键）+ VM（时序）→ `/api/v1/query`。
///
/// 需要三个 env：`DP_E2E_REDIS_URL`、`DP_E2E_VM_INSERT_URL`、`DP_E2E_VM_SELECT_URL`。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ingest_over_http_lands_in_redis_and_victoriametrics() {
    use dataplane_core::NoopAuth;
    use dataplane_file::{DirFileStore, FileStore};
    use dataplane_log::{LogStore, TantivyLogStore};
    use dataplane_sql::{RelationalStore, SqliteRelationalStore};
    use dataserver::{prom_router, sql_router, AppState};

    let kv_store = redis_store().await;
    let vm = vm_store();
    let kv: std::sync::Arc<dyn KvStore> = std::sync::Arc::new(kv_store);
    let ts: std::sync::Arc<dyn TimeSeriesStore> = std::sync::Arc::new(vm);

    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("data");
    for sub in ["files", "logs"] {
        std::fs::create_dir_all(root.join(sub)).expect("mkdir");
    }
    let file: std::sync::Arc<dyn FileStore> =
        std::sync::Arc::new(DirFileStore::new(root.join("files")));
    let sql: std::sync::Arc<dyn RelationalStore> =
        std::sync::Arc::new(SqliteRelationalStore::new(root.join("sql.sqlite")).expect("sql"));
    let log: std::sync::Arc<dyn LogStore> =
        std::sync::Arc::new(TantivyLogStore::new(root.join("logs")).expect("log"));
    dataplane_apm::bootstrap(sql.as_ref())
        .await
        .expect("apm schema");

    let state = AppState {
        file,
        kv: std::sync::Arc::clone(&kv),
        sql: std::sync::Arc::clone(&sql),
        ts: std::sync::Arc::clone(&ts),
        log,
        auth: std::sync::Arc::new(NoopAuth),
        gse_admin_url: None,
        gse_admin_password: None,
        metrics: None,
        apm: None,
        apm_limiter: None,
        apm_detail_min_duration_micros: 0,
    };

    async fn spawn(app: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}")
    }
    let ds_url = spawn(sql_router(state.clone(), None)).await;
    let prom_url = spawn(prom_router(state)).await;

    // VM 的 `-search.latencyOffset`（默认 30s）：用 2 分钟前的时间戳，避免「刚写入查不到」。
    let ts_us = ((now_micros() / 1_000) - 120_000) / 60_000 * 60_000 * 1_000;
    let item_id = format!("item-http-{}", now_micros());
    let agent_id = format!("agent-http-{}", std::process::id());
    let envelope = serde_json::json!({
        "batch_id": format!("b-{item_id}"),
        "data_type": "metrics",
        "data_id": item_id,
        "agent_id": agent_id,
        "host_id": "host-http",
        "sent_at_micros": ts_us,
        "records": [{
            "record_id": format!("r-{item_id}"),
            "timestamp": ts_us,
            "measurement": "vectorman_http_smoke_total",
            "tags": {},
            "field_name": "value",
            "field_value": 42.5
        }]
    })
    .to_string();

    let resp = reqwest_like_post(&ds_url, "/v1/ingest", &envelope).await;
    assert!(resp.0 == 200, "ingest failed: {} {}", resp.0, resp.1);
    assert!(resp.1.contains("\"status\":\"ok\""), "{}", resp.1);

    // KV 侧：`/v1/streams` 读的是 Redis 里的 `stream/` 索引。
    let streams = reqwest_like_get(&ds_url, "/v1/streams").await;
    assert_eq!(streams.0, 200, "{}", streams.1);
    assert!(
        streams.1.contains(&agent_id) && streams.1.contains(&item_id),
        "流索引没写进 Redis：{}",
        streams.1
    );

    // 时序侧：等 VM 追平后按 Prom 查询命中。
    let expr = format!("vectorman_http_smoke_total{{item_id=\"{item_id}\"}}");
    let query = format!(
        "/api/v1/query?query={}&time={}",
        urlencode(&expr),
        ts_us / 1_000_000
    );
    let mut hit = None;
    for _ in 0..60 {
        let r = reqwest_like_get(&prom_url, &query).await;
        if r.0 == 200 && r.1.contains(&item_id) {
            hit = Some(r.1);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    let body = hit.unwrap_or_else(|| panic!("15s 后 Prom 仍查不到 {expr}"));
    assert!(body.contains("\"value\""), "{body}");

    // 去重键确实落在 Redis（`ingest/{record_id}`），用 KV 接口直读。
    let record_key = format!("ingest/r-{item_id}");
    let seen = kv.exists(record_key.as_bytes()).await.expect("kv exists");
    assert!(seen, "去重键没写进 Redis: {record_key}");
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

async fn reqwest_like_post(base: &str, path: &str, body: &str) -> (u16, String) {
    use std::io::{Read, Write};
    let url = format!("{base}{path}");
    let (host_port, _) = url
        .trim_start_matches("http://")
        .split_once('/')
        .expect("url");
    let mut stream = std::net::TcpStream::connect(host_port).expect("connect");
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: {host_port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    stream.read_to_string(&mut resp).expect("read");
    parse_http(&resp)
}

async fn reqwest_like_get(base: &str, path: &str) -> (u16, String) {
    use std::io::{Read, Write};
    let url = format!("{base}{path}");
    let (host_port, _) = url
        .trim_start_matches("http://")
        .split_once('/')
        .expect("url");
    let mut stream = std::net::TcpStream::connect(host_port).expect("connect");
    let req = format!("GET {path} HTTP/1.1\r\nHost: {host_port}\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).expect("write");
    let mut resp = String::new();
    stream.read_to_string(&mut resp).expect("read");
    parse_http(&resp)
}

fn parse_http(raw: &str) -> (u16, String) {
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((raw, ""));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    (status, body.to_string())
}
