# Requirements Document

## Introduction

本 feature 给时序存储补上保留与删除能力：开启保留窗口执行、按序列选择删除历史点、暴露基数与内存/WAL 统计。它是 `apm-tracing` 与 `ebpf-observability` 聚合指标可清理的前置依赖，因此与两份观测设计同期交付。

现状（2026-09-23）：`crates/dataplane-ts` 的 `TimeSeriesStore` trait 只有 `write` / `query_instant` / `query_range`，`TsinkTimeSeriesStore::new` 只设置了 `data_path` 与 `TimestampPrecision::Microseconds`。底层 tsink 0.10.2 已具备保留与删除能力，但既未启用也未透出：`StorageBuilder` 的 `retention` 缺省 14 天但 `retention_enforced` 缺省 `false`（不拒绝也不过滤超窗点）；`Storage::delete_series` 在 trait 里是返回 `InvalidConfiguration` 的默认方法（本地引擎由 engine 的 `deletion` 模块实现）。

范围：只补「保留执行 + 按选择删除 + 统计暴露 + 保护性上限」，不改写写入与查询语义，不引入降采样与冷热分层。

## Glossary

- **保留窗口（retention window）**：允许写入与查询的最新时间跨度；超出窗口的点在保留执行开启时被拒绝或过滤。
- **保留执行（retention enforcement）**：是否真正按保留窗口拒绝/过滤点。tsink 缺省关闭，开启后写入超窗点会失败。
- **序列（series）**：一个 measurement（指标名）与一组 labels 的唯一组合。
- **序列选择（series selection）**：`measurement` + label matchers + 时间范围的组合条件，用于定位要删除的序列或点。
- **墓碑（tombstone）**：删除标记。写入墓碑后查询立即看不到被删点，磁盘空间由后续 compaction 回收。
- **基数（cardinality）**：序列总数。基数爆炸会同时放大内存与查询成本。
- **降采样（rollup）**：tsink 的 `RollupPolicy` 能力，把细粒度点物化为粗粒度点；本期不使用。
- **冷热分层（tiered retention）**：tsink 的 hot/warm/cold 保留策略，配合对象存储使用；本期不使用。

## Requirements

### Requirement 1: 保留窗口可配置并生效

**User Story:** AS 运维人员, I want 聚合指标按保留期自动过期, so that 磁盘占用不会无限增长。

#### Acceptance Criteria

1. THE `TimeSeriesStore` 实现 SHALL 接受保留配置：`retention_days`（天）、`enforced`（是否执行）。
2. WHEN `enforced` 为 true，THE 存储 SHALL 通过 tsink 的保留执行机制拒绝或过滤超出保留窗口的点。
3. WHEN `enforced` 为 false，THE 存储 SHALL 保持现状语义：超窗点照常写入与查询，不做过滤。
4. THE `dataserver` SHALL 提供配置项 `ts_retention_days`（默认 30）与 `ts_retention_enforced`（默认 true），并支持 `DATASERVER_` 前缀环境变量覆盖。
5. WHEN 保留执行开启且写入点的时间早于 `now - retention_days`，THE `write` SHALL 返回 `DataplaneError`，`code=invalid_argument`，消息含实测时间戳与窗口下界。
6. THE 保留窗口 SHALL 为全局窗口（对全部 measurement 生效）；按采集项区分的保留期由 Requirement 3 的删除任务实现。
7. THE 保留配置变更 SHALL 在进程重启后生效，不在运行期热更新。

### Requirement 2: 按序列选择删除

**User Story:** AS 运维人员, I want 能精确删掉某个采集项或某个指标的历史点, so that 单条链路的保留期可以独立控制。

#### Acceptance Criteria

1. THE `TimeSeriesStore` trait SHALL 新增 `delete_series(selection) -> Result<TsDeleteReport, DataplaneError>`。
2. THE `selection` SHALL 包含：`measurement`（可选）、label matchers（可多个，支持 `equal`、`not_equal`、`regex_match`、`regex_no_match`）、`from_ts`、`to_ts`（Unix 微秒）。
3. THE `TsDeleteReport` SHALL 包含 `matched_series`（命中的序列数）与 `tombstones_applied`（墓碑状态发生变化的序列数）。
4. THE 实现 SHALL 调用 tsink 的 `Storage::delete_series`，不自行扫描并重写数据。
5. WHEN `from_ts >= to_ts`，THE `delete_series` SHALL 返回 `code=invalid_argument`（对齐底层 `InvalidTimeRange`）。
6. WHEN 底层存储不支持删除（trait 默认实现或 compute-only 后端），THE `delete_series` SHALL 返回 `code=query_failed`，消息含 `delete_series is not implemented`，不报假成功。
7. WHEN 墓碑落盘失败，THE `delete_series` SHALL 返回 `code=query_failed`，不返回部分成功。
8. WHEN 对同一 selection 重复删除，THE 第二次调用的 `tombstones_applied` SHALL 为 0，`matched_series` 保持稳定。
9. THE 删除后查询 SHALL 立即看不到被删时间范围内的点；磁盘空间回收由底层 compaction 决定，接口不承诺删除后磁盘立刻下降。
10. THE `dataserver` SHALL 暴露 `POST /v1/ts/delete`（同源 SQL 口），请求体为 `selection`，响应为 `TsDeleteReport`；仅在 `apm_enabled` 或调试场景下由运维手动调用，不开放给前端常规操作。

### Requirement 3: 按采集项保留期清理

**User Story:** AS 运维人员, I want 每个采集项的保留期都被尊重, so that APM trace 明细与聚合指标的保留期可以不同。

#### Acceptance Criteria

1. THE `dataserver` SHALL 每小时执行一次时序清理任务。
2. WHEN 某采集项（`metrics` 类）配置了 `retention_days`，THE 清理任务 SHALL 调用 `delete_series`，selection 为：该采集项写入的 measurement 集合 + matcher `item_id == <item_id>` + 时间范围 `[0, now - retention_days)`。
3. WHEN 采集项被删除，THE 清理任务 SHALL 复用既有 `retain/{item_id}` 机制，以 `until_micros` 为上界删除，到期后删除 `retain/` 键。
4. THE 清理任务 SHALL 优先使用全局保留窗口；对已配置更短保留期的采集项，用 `delete_series` 补齐。
5. THE 清理任务 SHALL 记录一行标准输出，含 `item_id`、`matched_series`、`tombstones_applied` 与耗时。
6. WHEN 某采集项没有产生过时序数据，THE 清理任务 SHALL 跳过并继续，不报错。

### Requirement 4: 基数与资源保护

**User Story:** AS 运维人员, I want 指标基数有硬上限, so that 维度爆炸不会把 dataserver 拖垮。

#### Acceptance Criteria

1. THE `dataserver` SHALL 提供配置项 `ts_cardinality_limit`（默认 2_000_000）、`ts_memory_limit_bytes`（默认 0 表示不限）、`ts_wal_size_limit_bytes`（默认 0 表示不限）。
2. WHEN 非 0，THE 配置 SHALL 通过 tsink 的 `with_cardinality_limit` / `with_memory_limit` / `with_wal_size_limit` 传入。
3. WHEN 写入会产生超过基线上限的新序列，THE tsink 层 SHALL 拒绝该写入；THE `write` SHALL 返回 `code=query_failed`，消息含 `cardinality`。
4. WHEN 已存在的序列在上限内，THE 上限 SHALL 不影响其写入与查询。
5. THE 清理任务 SHALL 在发现 `matched_series` 连续 3 轮为 0 且有待删时间范围时，输出一条标准错误提示（疑似 matcher 写错），继续运行。

### Requirement 5: 统计暴露

**User Story:** AS 运维人员, I want 看到时序存储的保留与容量状态, so that 能提前发现基数或内存问题。

#### Acceptance Criteria

1. THE `TimeSeriesStore` trait SHALL 新增 `storage_stats() -> Result<TsStorageStats, DataplaneError>`。
2. THE `TsStorageStats` SHALL 至少包含：`series_count`、`memory_used_bytes`、`memory_budget_bytes`、`wal_size_bytes`、`retention_days`、`retention_enforced`、`expired_segments_total`、`future_skew_points_total`、`background_errors_total`、`degraded`。
3. THE `dataserver` SHALL 把上述值以 Prom 形指标暴露在既有自监控口，指标名为 `dataserver_ts_*`。
4. THE `dataserver` SHALL 提供 `GET /v1/ts/stats` 返回 `TsStorageStats` 的 JSON 形，供前端与 `dpc` 读取。
5. WHEN `degraded` 为 true，THE `dataserver` SHALL 在自监控页与健康检查响应中标记异常原因（取 `last_background_error`）。
6. THE `dpc` SHALL 新增 `ts stats` 与 `ts delete --metric <m> --matcher k=v --from-ts <us> --to-ts <us>` 两个子命令。

### Requirement 6: 兼容性

**User Story:** AS 开发者, I want 存储层变更不破坏现有调用方, so that 上线不需要改其它 feature。

#### Acceptance Criteria

1. THE 现有 `write`、`query_instant`、`query_range` 的行为在 `enforced=false` 时 SHALL 与变更前一致。
2. THE `TsPoint` 结构 SHALL 不变，一个点仍只有一个数值 field。
3. THE 新增 trait 方法 SHALL 有默认实现（`delete_series` 返回 `query_failed`、`storage_stats` 返回空统计），使其它实现（含测试桩）无需改动即可编译。
4. THE `dataserver` 的既有 SQL、Prom 查询、日志检索、接入行为 SHALL 不因本 feature 改变。
5. THE 打开保留执行后，需要历史回填的调用方 SHALL 收到明确错误（Requirement 1 第 5 条），不静默丢点。

### Requirement 7: 范围边界

**User Story:** AS 开发者, I want 明确本期不做什么, so that 实现范围可控。

#### Acceptance Criteria

1. THE 下列能力 SHALL 列入后续范围：降采样（tsink `RollupPolicy`）、冷热分层与对象存储（`with_tiered_retention_policy`、`with_object_store_path`）、快照与恢复（`snapshot` / `restore`）、按序列的差异化保留窗口（当前只有全局窗口 + 删除补齐）。
2. THE 本 feature SHALL 不改写 tsink 的数据格式与文件布局，不引入新的存储引擎。
3. THE 本 feature SHALL 不改变 `data_path` 布局（时序数据仍在 `{data_path}/ts/`）。
4. THE `ts_retention_enforced` 缺省为 true 的变更 SHALL 在发布说明中显式标注：升级后超窗点写入会失败。
