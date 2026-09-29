# Grill Me Results

Generated: 2026-09-29T09:01:37.317Z

## Plan

(state was created by grill_record_turns; no plan recorded)

## Shared Understanding

vectorman Agent 配置中心：一台 Agent 一份 spec（params + items），Server 只有取/存/下发三个动作，手动单台下发 + Agent 重连自动收敛 + 热加载（含 SIGHUP 本地重读）+ 逐字段生效核验与可视化；采集项取消一等资源并迁为 per-Agent；cpu/mem/log_level 本期仅标注 not_enforced。规格三件套已按此模型重写，7 份既有 spec 完成交叉回改。

## Questions and Answers

### 1. cloud2 上的存量数据怎么处理？（dataserver 49MB + 台账 1.2MB；台账里只有 3 个测试 agent，边记录为空，时序库里只有自监控指标）

**Recommended answer:** 空库起（推荐）

**User answer:** 空库起

**Status:** resolved

**Notes:** 云端现网数据很少（无业务数据），避免 v1.1.0 schema → 1.2.0 代码的兼容风险大于数据价值；若以后要历史曲线需要重新积累。

### 2. cloud2 现在跑着的 gse-agent（systemd，连着旧 server）怎么处理？

**Recommended answer:** 改连 cloud3（推荐）

**User answer:** 改连 cloud3，但是使用域名代替 IP 地址

**Status:** resolved

**Notes:** 用户答案与预置选项不同点：Agent 的 server_addr 用域名而非 IP。这与 q6 选了 NodePort+IP 有冲突，需要收口：要么给 RPC 加 TCPRoute 域名（如 gse-rpc.xiaoyxq.top），要么改选 TCPRoute。未收口前不能写 Agent 的迁移步骤。

### 3. 要不要在 cloud3 上以 DaemonSet 跑 gse-agent（eBPF 采集 k3s 自身）？

**Recommended answer:** 本轮不上，排下一轮（推荐）

**User answer:** 本轮不上，排下一轮

**Status:** resolved

**Notes:** Agent DaemonSet 材料已就绪（PR #81），下一轮一次部署动作即可；本轮 v1.2 验证矩阵中涉及 agent 的项顺延。

### 4. 前端 dist 的分发方式？

**Recommended answer:** 打进镜像（推荐）

**User answer:** 打进镜像

**Status:** resolved

**Notes:** 构建上下文从「发布包」改为「源码树」，multi-stage npm build；镜像 +~3MB、CI +约 2 分钟。build-image.sh 要加 --src 用法。

### 5. 对外 HTTP 入口的域名规划？

**Recommended answer:** 用建议的三个子域名（推荐）

**User answer:** 用建议的三个子域名

**Status:** resolved

**Notes:** gse.xiaoyxq.top → gse-server:7101；data.xiaoyxq.top → dataserver:8081（同源，含 Prom 路由）；console.xiaoyxq.top → console:7200。DNS A 记录由用户手工加，先解析生效再 apply（否则 ACME 签发失败）。

### 6. Agent RPC 7100 的对外地址走哪种？（HTTP Ingress 不能代理 TCP 长连接）

**Recommended answer:** NodePort 30710 + IP 直连（推荐）

**User answer:** NodePort 30710 + IP 直连

**Status:** open

**Notes:** ⚠️ 与 q2 的自定义答案冲突：用户要 Agent 用域名连，但这里选了 NodePort+IP。两个收口方案：① 保持 30710 直连但同时也加一条 TCPRoute 域名（如 gse-rpc.xiaoyxq.top→30710/7100），Agent 配置写域名；② 改选 TCPRoute-only。未收口前不写 cloud2 Agent 迁移步骤。

### 7. metrics 自监控口（7102/9091/7201）要不要建 Service？

**Recommended answer:** 不建 Service（推荐）

**User answer:** 不建 Service

**Status:** resolved

**Notes:** 前端已核实不调用 metrics 口；无 Prometheus 抓取方。将来接入时再加 Service（改一个文件）。

### 8. cops 的 CD 接管与镜像改造的合并节奏？

**Recommended answer:** cops 侧一次改造到位（推荐）

**User answer:** cops 侧一次改造到位

**Status:** resolved

**Notes:** cops 仓库 apps/vectorman 单元一次改造：E1 k8s.yaml/app.conf + B5 dist 进镜像 + C1 ghcr 流水线 + C3 checksum annotation，避免「CD 接管了但 dist 仍手工」的半部署状态。

### 9. cloud2 的旧 systemd 四件套什么时候退役？

**Recommended answer:** cops CD 稳定跑 2 天后（推荐）

**User answer:** cops CD 稳定跑 2 天后

**Status:** resolved

**Notes:** 迁移 PR 合入后让 cloud3 跑稳（cron 全量 apply 无漂移）再停 cloud2 systemd 四件套；/opt/vectorman 数据按 q1 结论（空库起）归档或删除。

### 10. Agent RPC 的客户端地址用域名还是 IP？（server 监听与客户端配置的收口）

**Recommended answer:** server 维持 NodePort 30710 监听；客户端 server_addr = "gse.xiaoyxq.top:30710"（DNS A → 186.244.201.55，不走 TCPRoute）。

**User answer:** server 监听只做 ip port（NodePort 30710）；客户端（gse-agent 的 server_addr）用域名+port 指定，域名在腾讯云 DNS 上解析到节点 IP。

**Status:** resolved

**Notes:** 收口 q2/q6 冲突：server 侧维持 NodePort 监听（IP:port），客户端 server_addr 写「域名:30710」。域名是 IP 的名字（DNS A → 186.244.201.55），流量直打 NodePort，不需要 TCPRoute、不过 Traefik。复用 gse.xiaoyxq.top（与 GSE 前端同域名不同端口），DNS 记录共三条：gse/data/console → 186.244.201.55。前置：腾讯云轻量防火墙放行 TCP 30710。

### 11. 核心修复：连接断开时必须清理会话。根因是 `handle_conn` 返回后（server.rs:170 spawn 丢弃 JoinHandle）没有生命周期清理，且 `run_liveness` 只按 last_seen 推进、心跳又不断 touch 刷新它，死会话永远停在 Online。修到哪一层？

**Recommended answer:** B. 正解：在 handle_conn 里等待连接结束，结束后 remove + close + mark_offline

**User answer:** B. 正解：在 handle_conn 里等待连接结束，结束后 remove + close + mark_offline

**Status:** resolved

**Notes:** 根因修复：让 spawn 的 future 活到连接结束，断开即清理；同时修 run_liveness 让心跳不再无脑保活死会话

### 12. 心跳语义：当前 registry.touch() 只更新时间戳、不建会话，所以「心跳活」≠「作业通道活」，这正是台账 online 却下发失败的原因。怎么处理？

**Recommended answer:** A. 心跳能「复活/重建」会话（心跳到达说明连接是活的）

**User answer:** A. 心跳能「复活/重建」会话（心跳到达说明连接是活的）

**Status:** resolved

**Notes:** 心跳与连接本就走同一条 gRPC 通道，心跳到达即证明连接活着，用它反推会话状态最自然

### 13. 这次修复的范围做到哪里？

**Recommended answer:** A. 只修 server 侧会话生命周期 + 补齐回归测试

**User answer:** C. 修 server 生命周期与心跳语义 + 前端/API 暴露会话真实状态 + 查 agent 侧重连为什么慢

**Status:** resolved

**Notes:** 一次做到观测与根因两层；agent 侧指数退避到 60s 后重置、dial 一直失败也一并查

### 14. testbkee 升级后连接仍是「建立后很快 multiplexer closed」，靠两次重启 gse-server 才做完升级。是否一并处理？

**Recommended answer:** A. 先在规格文档记录现象与影响，修完 server 侧后回来验证

**User answer:** B. 现在就深挖 testbkee 为什么连接不持久（可能要查 bkee5 的网络中间设备）

**Status:** resolved

**Notes:** 同步排查环境侧：可能是 bkee5 到公网的连接被中间设备掐断/超时，与 server bug 是两件事

### 15. testbkee（10.10.28.10）到 cloud3（186.201.55:30710）之间有过什么网络设备？观察到 TCP 连接半死（ESTAB 但 Send-Q 积压 122 字节），怀疑中间设备对空闲连接超时清理。

**Recommended answer:** A. 知道是什么设备，可以去查配置

**User answer:** C. 先不管中间设备，只修软件层

**Status:** resolved

**Notes:** 中间设备配置不查了，只在软件层解决；避免把修复依赖在环境侧

### 16. 既然确认了连接会「半死」，怎么让 agent 尽早发现？

**Recommended answer:** A. 给 agent 的 TCP 连接设置 keepalive（如 60s 探测）—— 让 OS 尽早发现死连接

**User answer:** A. 给 agent 的 TCP 连接设置 keepalive（如 60s 探测）—— 让 OS 尽早发现死连接

**Status:** resolved

**Notes:** 当前 TCP keepalive 是 600s（10 分钟），对中间设备的常见 5-10 分钟空闲超时太慢；目标降到 60s 级

### 17. 实现 PR 的流程怎么走？

**Recommended answer:** A. 先合 #89（规格），再开实现 PR

**User answer:** A. 先合 #89（规格），我再开实现 PR

**Status:** resolved

**Notes:** 先合 PR #89（规格）再开实现 PR，符合「先评审设计再实现」偏好

### 18. 错误措辞（design 决策 5）：要区分 agent offline 与 session unavailable。当前 mark_lost_by_agent 硬编码英文，其它错误码是中英混用。新增措辞用什么形式？

**Recommended answer:** A. 保持现有中英混用（错误码英文、原因英文短句）

**User answer:** A. 保持现有中英混用（错误码英文、原因英文短句）

**Status:** resolved

**Notes:** 错误码与原因都用英文短句，与现有 mark_lost_by_agent 的 "agent offline" 风格一致；新增 "session unavailable" 及其它分支措辞同理

### 19. 前端改动（Requirement 3.3：Agent 列表展示会话状态）是否包含在本 feature 的实现里？

**Recommended answer:** A. 包含前端改动

**User answer:** A. 包含前端改动

**Status:** resolved

**Notes:** 会话状态要前端可见，否则运维看不出「心跳在线但作业不可用」；Requirement 3.3 与本 feature 同 PR 完成

### 20. 不想每次人工重启 agent，希望做到什么程度？

**Recommended answer:** B. 写升级脚本 + skill（脚本走 vmctl 自动下发）

**User answer:** 在本仓库加入 一台一台升级的脚本即可

**Status:** resolved

**Notes:** 不做自动遍历台账；脚本接受目标 agent 参数，一次升一台，运维可控

### 21. 升级时 agent 会被重启，下发通道会断。怎么避开这个死锁？

**Recommended answer:** A. 靠现有作业通道（agent 自己执行升级脚本）

**User answer:** A. 靠现有作业通道（agent 自己执行升级脚本）

**Status:** resolved

**Notes:** setsid nohup 让升级脚本脱离作业进程组，避免【停 agent 连带杀掉作业子进程】导致升级半途而废

### 22. 方案的覆盖范围？

**Recommended answer:** A. 三台都支持（debian12 / cloud2 systemd；testbkee ctl.sh）

**User answer:** 解决两种形式的

**Status:** resolved

**Notes:** 脚本需自动识别 systemd / ctl.sh direct 两种部署形式（前者用 systemctl，后者用 ctl.sh + PID 文件）

### 23. 实测发现：走作业通道升级时，「停 agent」这一步总踩坑（setsid 不脱 cgroup、作业卡 running）。你想走哪条路？

**Recommended answer:** A. 脚本只换文件，不停服

**User answer:** B. 给 agent 加自更新能力（改代码 + 出规格）

**Status:** resolved

**Notes:** 实测证明「作业通道里停 agent 换二进制」不可靠：setsid 不脱 cgroup（systemd 按 cgroup 杀）、外层作业卡 running、时间线混乱。改为 agent 自身提供升级能力：接收升级作业 → fork 独立进程（脱离 cgroup）→ 由它替换二进制并触发 systemd/ctl.sh 重启 → 新进程接管。需先出规格再实现

### 24. 真集群验证（DaemonSet 部署 Agent + Pod 名反查 + 容量观测）在哪个环境做？

**Recommended answer:** debian12 先灰度 + cloud3/testbkee 铺开（规格灰度口径）

**User answer:** cloud3 远程 k3s

**Status:** resolved

**Notes:** 用户选 cloud3；server 栈所在地，验证后 cloud3 节点直接跑 agent 最接近生产

### 25. 验证时 dataserver/gse-server 用哪套？

**Recommended answer:** 先本地后打 cloud3（scratch 验证+线上对账）

**User answer:** cloud3 现有线上栈

**Status:** resolved

**Notes:** 真链路验证；agent 需配 server 地址与 token

### 26. v1.2 这轮范围怎么切？

**Recommended answer:** 全量 hardening（验证+回归+规格对齐）

**User answer:** 先只做真集群验证主线（tasklist 3.1–3.10）

**Status:** resolved

**Notes:** 回归三件套与规格一致性顺延

### 27. Agent 容器镜像怎么分发到集群？

**Recommended answer:** 由目标环境网络决定

**User answer:** ghcr.io 镜像

**Status:** resolved

**Notes:** 走 release-image.yml 打新 tag，集群拉 ghcr

### 28. 先从哪块启动？

**Recommended answer:** 直接开搞 DaemonSet 部署验证

**User answer:** 直接开搞 DaemonSet 部署验证

**Status:** resolved

**Notes:** 按 tasklist 3.1 起

### 29. 镜像策略：先打 agent/v1.2.0 tag 走 ghcr，还是先 build-image --import 快速验证？

**Recommended answer:** 先 build-image.sh --import 快速验证一轮，清单改好后再正式打 tag

**User answer:** 先快速验证一轮、改好后再打 tag

**Status:** resolved

**Notes:** 避免清单错误→重打 tag 的往返；快速验证用 build-image.sh --import 送镜像进 cloud3 k3s containerd

### 30. vmctl 与 dpc 的关系怎么定？

**Recommended answer:** vmctl 吸收 dpc 查询子命令，同时持有 gse URL + dataserver URL

**User answer:** vmctl 新增 `vmctl data ...` 子命名空间承载查询，dpc 保留待废弃

**Status:** resolved

### 31. 采集项下发的命令形态？

**Recommended answer:** 强类型子命令 + --json 逃生口

**User answer:** 只接受 raw JSON 文件/字符串（-f spec.json / --json）

**Status:** resolved

### 32. 是否需要声明式批量下发（幂等 apply + diff 预览）？

**Recommended answer:** 底层 CRUD 原语 + collect apply -f spec.json 幂等声明式

**User answer:** 底层 CRUD 原语 + collect apply -f spec.json 幂等声明式（both）

**Status:** resolved

### 33. 声明式 spec 文件格式？

**Recommended answer:** JSON（零新依赖）

**User answer:** JSON

**Status:** resolved

### 34. 一条采集项怎么选中目标机器？

**Recommended answer:** --agent-id 可重复，仅本地展开不做选择器

**User answer:** --agent-id 可重复，仅本地展开不做选择器

**Status:** resolved

### 35. 下完配置怎么确认「真的采上了」？

**Recommended answer:** collect status <item_id>：agent 是否拉取 + 最近上报时间 + 数据点计数

**User answer:** collect status <item_id>（推荐项）

**Status:** resolved

### 36. 「能查数据」覆盖到哪些面？

**Recommended answer:** 反代 dpc 现有全部：health/sql/query/logs/ebpf/traces/edges/apm/ts/streams

**User answer:** 反代 dpc 现有全部（all）

**Status:** resolved

### 37. 查询结果输出形态？

**Recommended answer:** 默认原样透传服务端 JSON，--table 可选表格

**User answer:** 默认原样透传 JSON，--table 可选表格

**Status:** resolved

### 38. 多环境（本机/test/dev/uat/线上）怎么切？

**Recommended answer:** ~/.vmctl/config.json profiles + --profile

**User answer:** 只命令行 flag

**Status:** resolved

### 39. 凭证怎么带？

**Recommended answer:** 预留 --token/VECTORMAN_TOKEN 注入 Authorization，本期不做认证逻辑

**User answer:** 预留 --token/VECTORMAN_TOKEN，本期不做认证逻辑

**Status:** resolved

### 40. 哪些写操作算「链路采集能力」范围内？

**Recommended answer:** 排除 /v1/ingest；包含采集项 CRUD、agent-config、时序删除、服务名别名 CRUD

**User answer:** 排除 /v1/ingest，包含采集项 CRUD、agent-config、时序删除、服务名别名 CRUD

**Status:** resolved

### 41. 是否要 vmctl collect doctor --agent <id> 链路聚合诊断？

**Recommended answer:** 要

**User answer:** 要（推荐项）

**Status:** resolved

### 42. 8 类采集项字段怎么让用户知道？

**Recommended answer:** collect kinds 列出每类必填/可选字段 + 示例 + 前置条件

**User answer:** collect kinds（推荐项），不做预置模板

**Status:** resolved

### 43. 现有 vmctl 子命令/输出是否必须保持不变？

**Recommended answer:** 必须完全向后兼容

**User answer:** 允许重组命名空间（如统一到 vmctl gse ... / vmctl data ...），破坏性变更可接受

**Status:** resolved

### 44. 这一轮交付什么？

**Recommended answer:** 只写 requirements.md

**User answer:** 写清楚需求提交 PR 即可

**Status:** resolved

### 45. token 轮换的锁死窗口怎么解？（保存期望配置时若同时更新 agents.token，而下发是手动的，Agent 在窗口内重连会被拒且永久失联）

**Recommended answer:** 服务端接受「新 + 上一个」双凭据宽限（新增 prev_token 列，Agent 用新 token 认证成功后清除）——零窗口、无需回滚

**User answer:** 服务端接受「新 + 上一个」双凭据宽限

**Status:** resolved

**Notes:** 需在 design 增 agents.prev_token 列 + auth 双凭据校验 + 用新 token 认证成功后清 prev_token；影响 requirements R7/R11 与 tasklist 阶段 3。

### 46. 下发的心跳周期可能超过服务端全局判活窗口（heartbeat_timeout_secs，默认 90s），导致 Agent 周期性闪断，怎么处理？

**Recommended answer:** 服务端校验：下发值必须 ≤ heartbeat_timeout_secs / 3，否则 400 拒绝

**User answer:** 服务端校验：下发值必须 ≤ heartbeat_timeout_secs / 3，否则拒绝

**Status:** resolved

**Notes:** 需在 requirements 增一条校验（下发值 > timeout/3 → 400）并在 design 的 error handling 表里补该行；同时文档写明「想把心跳放大必须先调服务端 heartbeat_timeout_secs」。

### 47. 删掉期望配置之后 Agent 应该变成什么？（Agent 不重读本地 TOML，删除实际等于永久停在最后一次下发的值）

**Recommended answer:** 删除时推送空 revision，Agent 重读本地 gse-agent.toml 并热应用

**User answer:** 不提供删除（避免「删了到底发生了什么」的歧义）

**Status:** resolved

**Notes:** 从 API 里去掉 DELETE /agent-configs/{id}；连带影响：tasklist 3.1、Correctness Property 8、前端「删除期望配置」入口、以及我刚改过的 gse-node-app/design.md 里「补上删除动作」那句要回退。接受代价：一旦有过期望配置，Agent 就再也回不到本地 TOML 基线（需在 design 里写成显式已知限制）。

### 48. 采集项手动下发时，一个 Agent 把所有采集项都移除后就不再成为下发目标，手上那几项会一直采下去，怎么补？

**Recommended answer:** 三种模式都支持：item_ids / agent_ids / all

**User answer:** 不太清晰你的问题是什么？我希望下发的信息很明确清晰，不做成批量下发，比如一次不能操作多个 agent 下的多个采集项，而是应该操作同一个 agent 下的多个采集项

**Status:** resolved

**Notes:** 用户否掉了批量语义：下发粒度是「单个 Agent 下的多个采集项/多项配置」，不做跨 Agent 批量。这推翻了第一轮 questionnaire 里「手动下发按钮（可批量/重试）」的批量部分——需回改 requirements R6/R8/R9、design 的接口表、tasklist 3.1/5.5，并删掉 `POST /agent-configs/apply` 与 `POST /collect-items/apply` 的批量形态。

### 49. 「一次下发」到底包含什么？是不是把 Agent 参数与采集项合并成一台 Agent 一次操作、一份回执？

**Recommended answer:** 一次下发 = 一台 Agent 的完整期望状态（Agent 参数 + 采集项整表），一份回执

**User answer:** 是：一次下发 = 一台 Agent 的完整期望状态（Agent 参数 + 采集项整表）

**Status:** resolved

**Notes:** 下发单位从「字段/采集项」改成「一台 Agent 的完整期望状态」：Agent 参数 + 该 Agent 命中的采集项整表合并为一次调用、一份 revision、一份回执。推翻上一轮「批量下发」的结论，需重写 requirements R6/R8/R9 与 design 的服务端接口、协议（AgentConfigPush 增 items）、Correctness Properties，并删掉 POST /agent-configs/apply 与 /collect-items/apply 的批量形态。核心新问题：采集项台账是共享表还是 per-agent 副本。

### 50. 采集项台账属于谁？全局共享表（item 带 agent_ids）与「一台 Agent 的完整期望状态」会打架：在 A 页改采集项会同时改掉 B

**Recommended answer:** 保留共享台账表（item 带 agent_ids），某 Agent 的「整表」= 过滤结果

**User answer:** 迁到 per-agent：每个 Agent 拥有自己一份采集项列表

**Status:** resolved

**Notes:** 采集项从全局共享表（item.agent_ids 关联多台 Agent）迁为 per-agent：每台 Agent 拥有自己一份采集项列表，改 A 不影响 B。连带影响大：collect_items 表主键改 (agent_id, item_id)、gse-proto::CollectItem 去掉 agent_ids、HTTP 路由改 /agents/{id}/collect-items、dataplane 采集链路页按 Agent 视角重做、现有全局台账按 agent_ids 展开成 per-agent 行做一次性搬运、vmctl-collect-chain spec 的 R4/R5/R6/R13 口径整体改写。新产生的能力缺口：跨 Agent 复制采集项（模板能力没了）。

### 51. 采集项的删除要不要自动推送（破掉「改动都要手动下发」规则）？

**Recommended answer:** 台账写操作一律不推送；删除后该 Agent 显示 pending，运维自己点下发

**User answer:** 对于某一个 agent 采集项的删除，就在 spec 中剔除掉它就好了

**Status:** resolved

**Notes:** 用户把问题本身消掉了：采集项不是独立资源，而是「该 Agent 的期望状态 spec」的一部分；删除某台 Agent 的采集项 = 编辑这份 spec 把该项剔除，再下发整份 spec。因此不存在「DELETE 采集项要不要自动推送」这个问题，也没有独立的 PUT/DELETE /collect-items 写路由。

### 52. 是不是可以把「采集项的增删改查」整个去掉，只留「某台 Agent 的期望状态 spec」这一份文档？

**Recommended answer:** 是：采集项只是「该 Agent 的 spec」里的一段；只有 spec 级读/写/下发三个动作

**User answer:** 是：采集项只是「该 Agent 的 spec」里的一段；只有 spec 级读/写/下发三个动作

**Status:** resolved

**Notes:** 去掉采集项的独立资源模型与全部写路由（POST/PUT/DELETE /collect-items*）。新模型：一台 Agent 一行 spec（JSON：Agent 参数 + 采集项数组），只有三个动作 GET /agents/{id}/spec、PUT /agents/{id}/spec（只写期望）、POST /agents/{id}/spec/apply（推送整份 + 回执）。连带：gse-proto::CollectItem 的 agent_ids 字段删除；/api/gse/agent-configs 与 /api/gse/collect-items 的既有写路由都是破坏性变更。

### 53. 采集项变成 per-agent spec 后，dataplane 的「采集链路」页（全局采集项列表）怎么办？

**Recommended answer:** 保留为跨 Agent 的只读总览（从所有 spec 派生 + stream 上报状态），编辑跳到 Agent 配置页

**User answer:** 采集链路页保留为跨 Agent 的只读总览（从所有 spec 派生 + stream 上报状态），编辑跳到 Agent 配置页

**Status:** resolved

**Notes:** 采集链路页保留为跨 Agent 只读总览（从所有 spec 派生 + stream 上报状态），编辑入口统一在 Agent 配置页。需改 dataplane 的 collect-page.tsx：从「可编辑全局列表」改为「只读总览 + 跳转编辑」。

### 54. 命令行（vmctl）要不要本期一起做？vmctl-collect-chain spec 的 per-item CRUD 设计与 per-agent spec 模型对不上

**Recommended answer:** 本期不做 CLI

**User answer:** 本期不做 CLI

**Status:** resolved

**Notes:** 本期不做 CLI。vmctl-collect-chain spec（规格完成、未实施）需重新定范围：其 per-item CRUD + agent_ids 的设计与 per-agent spec 模型不符，collect 部分基本重写；data 子命令（dataserver 查询）与新模型无关，可保留。

### 55. 「手动下发」是不是唯一生效路径？Agent 重连后要不要自动拉取最新期望配置？

**Recommended answer:** 会：Agent 认证成功后主动拉取最新期望 spec 并应用

**User answer:** 应该需要允许 agent 配置手动修改和应用，但是，可能会被 server 下发的配置冲掉（→ 允许本地手改 + 应用，且下发 > 本地）

**Status:** resolved

**Notes:** 本地手动改配置 + 应用也要支持：触发方式为 SIGHUP（systemd `systemctl reload` / ctl.sh direct 部署 `kill -HUP <pid>`），Agent 重读 gse-agent.toml 并热应用。优先级仍为 下发 > 本地（用户明确接受本地改动会被 server 下发冲掉）→ 因此 Agent 认证成功后仍要拉取最新期望 spec 并应用，即自动收敛，手动下发按钮只是「不必等重连、立即生效」。这推翻了第一轮「不做文件监听、只做 Server 下发通道」的口径（监听仍不做，SIGHUP 不算监听）。需在 requirements R1/R4 增一条本地 reload 需求，design 增 signal handling 与 gse-agent 依赖（tokio signal feature 或 libc::signal）。

## Agreed Decisions

- 配置面：Agent 本地运行参数 + 本地 TOML 全量参数 + 采集项，统一为一个「Agent 配置中心」
- 粒度：一台 Agent 一份 spec（params + items），一次下发 = 该 Agent 的完整期望状态，一份 revision、一份回执；不做跨 Agent 批量下发
- 采集项取消一等资源：没有独立增删改查、没有 agent_ids；删除某台 Agent 的采集项 = 编辑该 Agent 的 spec 剔除它
- 采集项归属：迁到 per-agent（每台 Agent 拥有自己一份采集项列表），迁移时把全局 item 按 agent_ids 展开成 N 份拷贝，此后互不影响
- Server API 只有三个动作：GET spec / PUT spec（只写期望，不推送）/ POST spec/apply（单台下发）
- 触发方式：保存与下发解耦，手动下发单台 Agent；Agent 认证后自动拉取期望 spec（重连即收敛），所以手动下发是「立即生效」而非「唯一生效」
- 心跳周期下发校验：必须 ≤ heartbeat_timeout_secs / 3，否则拒绝
- token 轮换：服务端接受「新 + 上一个」双凭据宽限（agents.prev_token），用新 token 认证成功后清除
- server_addr / agent_id 不可下发（只能改本地文件），但作为只读字段随生效上报回来展示
- 本地手改：支持 SIGHUP 重读 gse-agent.toml 并热应用；优先级为 下发 > 本地重读 > env > TOML > 默认（本地改动会被下发冲掉，已接受）
- 热加载：心跳周期、作业四项、OTLP 参数、采集项整表全部不重启进程；仅 token 变更触发一次主动重连；OTLP 变更只重对齐 apm_otlp 项以保住 log_file 的 tail 位置
- cpu_limit_percent / mem_limit_percent / log_level：本期不做真实现，只存期望值并由 Agent 上报 not_enforced 显式标注
- 审计：只记 updated_at / reported_at，不建历史表、不做审批
- 脱敏：token / otlp_token 在响应里输出 "***"，写回哨兵或空串表示保持原值，null 表示清空
- 不提供删除期望 spec（一旦有过期望 spec 就回不到纯本地 TOML 基线，已接受）
- 不做：跨 Agent 复制采集项、全局模板库、CLI（vmctl 本期不动）、文件监听、跨 Agent 批量
- 可视化：/agent-configs 列表 + /agent-configs/:id 独立整页（表单 / 原始 JSON / 逐字段差异 / 采集项）；dataplane 采集链路页改为跨 Agent 只读总览
- 存储：新表 agent_specs / agent_spec_states 各一行整份 JSON（避开 sqlite 无迁移框架）+ ALTER agents 加 prev_token
- 协议：RPC agent_spec 双侧同名注册取代 collect_items 通道；心跳带一次性补报 + HeartbeatReply{spec_synced} 确认重传

## Open Risks

- gse-server 管理口（默认 127.0.0.1:7101）无鉴权，而配置下发是「能改所有 Agent 行为」的敏感写操作 —— 已把 observability-hardening 1.19 从待办升级为前置风险（只提这一处，不再重复建议）
- 5 条破坏性变更：/api/gse/collect-items* 与 /api/gse/agent-configs* 整组删除、collect_items RPC 通道删除、CollectItem.agent_ids 删除、dataplane 采集链路页由可编辑改只读、dataserver /v1/collect-items* 转发改只读 /v1/agent-specs 且 retention 清理数据源改 agent-specs（同一 item_id 跨 Agent 的 retention_days 冲突取最大值）
- 迁移语义不可逆：全局采集项按 agent_ids 展开成 N 份独立拷贝后不再联动（刻意取舍，必须写进发布说明，否则会被当 bug）
- 「下发赢 + 重连自动收敛」意味着本地手改只在下次下发/重连前有效（刻意的取舍）
- 一旦某台 Agent 有过期望 spec，就没有回到纯本地 TOML 基线的路径（本 feature 不提供删除）
- 整份 spec 存一行 JSON → 「哪些 Agent 采了 X」是内存聚合、无索引；规模上来需要拆 agent_spec_items 表或把聚合下推
- cpu_limit_percent / mem_limit_percent / log_level 仍是未实现字段，只由 not_enforced 标注；升级路径＝作业子进程 setrlimit + Agent 日志级别门控
- vmctl-collect-chain spec 的 collect 部分（per-item CRUD + agent_ids）与新模型不符，已标为「需重新定范围」，CLI 形态应为 vmctl agent spec get|put|apply

## Next Decision Needed

无（已到 95% 把握）；下一步是实施，建议从阶段 1（协议）与阶段 2（台账 + 一次性迁移）开始
