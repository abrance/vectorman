# Grill Me Results

Generated: 2026-09-28T00:12:16.037Z

## Plan

(state was created by grill_record_turns; no plan recorded)

## Shared Understanding

v1.2 范围 = observability-hardening 真集群验证主线（tasklist 3.1–3.10），环境 cloud3 k3s（单节点，kernel 6.8 + BTF），服务端复用线上栈，镜像走 ghcr.io，直接从 DaemonSet 部署验证启动。回归三件套（2.2–2.4）与规格一致性（5.x）顺延。

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

## Agreed Decisions

- 验证环境：cloud3 远程 k3s（SSH cloud3，节点 ser539375215934，kernel 6.8.0-48，BTF OK）
- 服务端：复用 cloud3 现有线上栈（gse-server:30710 / dataserver:30881）
- 范围：只做真集群验证主线（tasklist 3.1–3.10）
- 镜像分发：ghcr.io（agent/v<semver> tag 触发 release-image.yml）
- 启动项：直接开搞 DaemonSet 部署验证

## Open Risks

- cloud3 是单节点：tasklist 里「灰度单节点→铺开全节点」「内核版本矩阵」做不了，多 Agent 场景需用现有 3 个 native agent（cloud2/debian12/testbkee）+ 新容器 agent 同 server 对账替代
- cloud3 上现有 3 个 native agent 打的是 30710 线上 server：新容器 agent 也注册到同一 server，观测口径要按 agent_id 区分
- DaemonSet 清单 image 现为本地 tag + IfNotPresent，需改为 ghcr 地址 + Always/具体版本
- 需要台账预登记 ser539375215934 的 token 并写入 Secret（README 第 2 节），token 不进版本库
- 线上栈开了鉴权（auth_enabled=true），老占位 token 不可用
- 公网 dataserver 鉴权仍未开（1.19 记录「以后再做」），验证期间注意不要误用公网入口写数据

## Next Decision Needed

镜像版本号与 tag 时机（先合清单改动打 agent/v1.2.0，还是先本地 build-image 验证一轮再正式打 tag）
