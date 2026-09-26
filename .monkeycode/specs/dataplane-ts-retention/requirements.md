# Requirements Document

> **已归档**（2026-09-26 文档整理）：本 feature 已实现并合入 main，本文件压缩为需求索引——完整 EARS 条款见 git 历史（本文件重写前的最后一个版本），design.md 全文保留为实施细节档案。

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

- AS 运维人员, I want 聚合指标按保留期自动过期, so that 磁盘占用不会无限增长。
- 验收：THE `TimeSeriesStore` 实现 SHALL 接受保留配置：`retention_days`（天）、`enforced`（是否执行）；WHEN `enforced` 为 true，THE 存储 SHALL 通过 tsink 的保留执行机制拒绝或过滤超出保留窗口的点。
### Requirement 2: 按序列选择删除

- AS 运维人员, I want 能精确删掉某个采集项或某个指标的历史点, so that 单条链路的保留期可以独立控制。
- 验收：THE `TimeSeriesStore` trait SHALL 新增 `delete_series(selection) -> Result<TsDeleteReport, DataplaneError>`；THE `selection` SHALL 包含：`measurement`（可选）、label matchers（可多个，支持 `equal`、`not_equal`、`regex_match`、`regex_no_match`）、`from_ts`、`to_ts`（Unix 微秒）。
### Requirement 3: 按采集项保留期清理

- AS 运维人员, I want 每个采集项的保留期都被尊重, so that APM trace 明细与聚合指标的保留期可以不同。
- 验收：THE `dataserver` SHALL 每小时执行一次时序清理任务；WHEN 某采集项（`metrics` 类）配置了 `retention_days`，THE 清理任务 SHALL 调用 `delete_series`，selection 为：该采集项写入的 measurement 集合 + matcher `item_id == <item_id>` + 时间范围 `[0, now - retention_days)`。
### Requirement 4: 基数与资源保护

- AS 运维人员, I want 指标基数有硬上限, so that 维度爆炸不会把 dataserver 拖垮。
- 验收：THE `dataserver` SHALL 提供配置项 `ts_cardinality_limit`（默认 2_000_000）、`ts_memory_limit_bytes`（默认 0 表示不限）、`ts_wal_size_limit_bytes`（默认 0 表示不限）；WHEN 非 0，THE 配置 SHALL 通过 tsink 的 `with_cardinality_limit` / `with_memory_limit` / `with_wal_size_limit` 传入。
### Requirement 5: 统计暴露

- AS 运维人员, I want 看到时序存储的保留与容量状态, so that 能提前发现基数或内存问题。
- 验收：THE `TimeSeriesStore` trait SHALL 新增 `storage_stats() -> Result<TsStorageStats, DataplaneError>`；THE `TsStorageStats` SHALL 至少包含：`series_count`、`memory_used_bytes`、`memory_budget_bytes`、`wal_size_bytes`、`retention_days`、`retention_enforced`、`expired_segments_total`、`future_skew_points_total`、`background_errors_total`、`degraded`。
### Requirement 6: 兼容性

- AS 开发者, I want 存储层变更不破坏现有调用方, so that 上线不需要改其它 feature。
- 验收：THE 现有 `write`、`query_instant`、`query_range` 的行为在 `enforced=false` 时 SHALL 与变更前一致；THE `TsPoint` 结构 SHALL 不变，一个点仍只有一个数值 field。
### Requirement 7: 范围边界

- AS 开发者, I want 明确本期不做什么, so that 实现范围可控。
- 验收：THE 下列能力 SHALL 列入后续范围：降采样（tsink `RollupPolicy`）、冷热分层与对象存储（`with_tiered_retention_policy`、`with_object_store_path`）、快照与恢复（`snapshot` / `restore`）、按序列的差异化保留窗口（当前只有全局窗口 + 删除补齐）；THE 本 feature SHALL 不改写 tsink 的数据格式与文件布局，不引入新的存储引擎。
