# 规格索引（.monkeycode/specs/）

每个 feature 一个目录，三件套：`requirements.md`（需求）、`design.md`（设计）、`tasklist.md`（实施清单）。

**文件定位约定（2026-09-26 起）**：

- `design.md` 是长效档案：实现细节、踩坑、容量口径都记在这里，**随代码演进维护**（源码注释大量按
  「`design.md` 某节」「requirements.md Requirement N」引用，编号与文件名保持稳定）。
- 已实现 feature 的 `requirements.md` 压缩为需求索引（编号 + User Story + 验收摘要），
  完整 EARS 条款见 git 历史；`tasklist.md` 的勾选状态是合入时的快照，不再回填。
- 判定「功能现状」以 README「能力清单」与代码为准，不以 spec 勾选状态为准。

## 索引

| Feature | 状态 | 截至日期 | 一句话 |
| --- | --- | --- | --- |
| [gse-session-liveness](gse-session-liveness/) | 🟡 ACTIVE（规格完成，待实施） | 2026-09-26 | 会话生命周期修复：断连后台账/会话状态一致的关闭路径 |
| [ebpf-observability](ebpf-observability/) | 🟡 ACTIVE（主体已实现，P2/P3 遗留见 `todo.md`） | 2026-09-25 | eBPF 四类采集（network/tcp/process/syscall）：内核态程序 + 用户态聚合 |
| [observability-hardening](observability-hardening/) | 🟡 ACTIVE（余 cron 幂等验收、鉴权加固 1.19） | 2026-09-26 | 可观测加固与真集群（k3s）验证：清单 + 验证执行 + 文档对齐 |
| [apm-tracing](apm-tracing/) | ✅ 已实现 | 2026-09-23 | trace/APM 全链路：OTLP 经 Agent 中转 + eBPF 推导、span 查询、拓扑边、RED |
| [console-app-tags](console-app-tags/) | ✅ 已实现 | 2026-09-13 | console 门户 App 台账的 tags 标签与过滤 |
| [console-desktop](console-desktop/) | ✅ 已实现 | 2026-09-11 | 桌面门户 console：聚合各组件入口的 App 目录 + web 托管 |
| [dataplane-layered-storage](dataplane-layered-storage/) | ✅ 已实现 | 2026-09-05 | 数据平面分层存储：五类存储接口 + 本地引擎（文件/时序/日志/KV/sqlite） |
| [dataplane-ts-retention](dataplane-ts-retention/) | ✅ 已实现 | 2026-09-23 | 时序保留：按天清理 + 写入窗口校验 + 基数上限 |
| [deploy-packaging](deploy-packaging/) | ✅ 已实现 | 2026-09-18 | 安装包打包链：musl 静态二进制 + 目录布局 + install.sh/ctl.sh/systemd |
| [frontend-layered-architecture](frontend-layered-architecture/) | ✅ 已实现 | 2026-09-10 | 前端分层：console/dataplane/job/node 的 package 划分与构建产物约定 |
| [gse-cli](gse-cli/) | ✅ 已实现 | 2026-09-11 | vmctl 一次性 CLI：gse-server 台账只读 + 作业提交/等待/重做 |
| [gse-dataplane-ingest](gse-dataplane-ingest/) | ✅ 已实现 | 2026-09-23 | 采集数据接入数据面：Agent 缓冲/直连 dataserver、四类采集信封、流索引 |
| [gse-job-execution](gse-job-execution/) | ✅ 已实现 | 2026-09-10 | 作业脚本下发与执行协议（Agent 拉取/执行/回传） |
| [gse-job-file-transfer](gse-job-file-transfer/) | ✅ 已实现 | 2026-09-20 | 作业文件传输：vmctl 本地文件 → Agent 目标机 |
| [gse-job-rerun](gse-job-rerun/) | ✅ 已实现 | 2026-09-10 | 历史作业重做（rerun API 与覆盖字段） |
| [gse-job-templates](gse-job-templates/) | ✅ 已实现 | 2026-09-10 | 作业模板：参数化模板存台账、按模板提交 |
| [gse-node-app](gse-node-app/) | ✅ 已实现 | 2026-09-10 | 节点管理前端（@vectorman/node：主机/接入点/Agent 页面） |
| [gse-server-agent](gse-server-agent/) | ✅ 已实现 | 2026-09-06 | GSE Server↔Agent 最小闭环：会话/认证/心跳/信令 |
| [gse-server-cmdb](gse-server-cmdb/) | ✅ 已实现 | 2026-09-06 | 资产台账 CMDB 化：Host/Agent/接入点持久化 sqlite + 查库认证 |
| [observability-data-model](observability-data-model/) | ✅ 已实现（本目录仅 design） | 2026-09-24 | 可观测共享数据模型：span/边/服务标识/指标命名 + LogStore v2/sqlite 观测表 |
| [server-self-monitoring](server-self-monitoring/) | ✅ 已实现 | 2026-09-21 | 组件自监控指标（vectorman-* metrics 口） |
| [vmctl-file-transfer](vmctl-file-transfer/) | ✅ 已实现 | 2026-09-21 | vmctl 文件传输子命令 |

## 相关测试用例

GSE 节点管理与作业平台的测试用例库在 [`../docs/testcases/`](../docs/testcases/README.md)，
按业务模块分文件维护，与上表 feature 相互引用。
