# 任务清单：可观测加固与真集群验证

> 对应需求 `.monkeycode/specs/observability-hardening/requirements.md`，
> 设计 `.monkeycode/specs/observability-hardening/design.md`。全部未开工，用户确认后按序执行。
>
> 说明：本 feature 不新增采集能力，因此没有「内核态程序」阶段；主体是**清单 + 验证执行 + 文档对齐**。

## 1. K8s 部署清单（对应需求 1、9）

- [x] 1.1 新增 `packaging/deploy/k8s/gse-agent-daemonset.yaml`：Namespace / ServiceAccount /
      ClusterRole(pods get,list) / ClusterRoleBinding / ConfigMap / DaemonSet 六个对象，
      按设计的字段取值（`hostPID: true`、BTF 与 tracing 只读 hostPath、特权、Downward API 取 `spec.nodeName`
      作 `agent_id`、`token` 走 Secret、`emptyDir` 作上行队列目录）
- [x] 1.2 新增 `packaging/deploy/k8s/README.md`：镜像怎么来（scratch + 静态二进制，实测 10.7 MB）、
      台账里怎么预登记各节点 Agent、如何 `kubectl apply`、如何用 `nodeSelector` 灰度、
      以及「停用采集项/删 DaemonSet」两种回滚方式
- [x] 1.3 已在 cloud3 真集群 `kubectl apply --dry-run=server` 通过（7 个对象全部校验通过；
      命名空间需先存在，dry-run 不落盘 —— 已建 `vectorman` 命名空间）
- [x] 1.4 打包脚本 `packaging/build-package.sh` 把 `deploy/k8s/` 一并打入安装包（保持「随包分发」的一致性）

- **检查点 A**（2026-09-25 完成）：清单在 cloud3 真集群 `--dry-run=server` 通过；
  `deploy/k8s/` 已随包分发；既有 Rust/前端测试保持全绿。

已完成但属于「实施期发现」的两件事，记录在此避免丢失：

- [x] 1.5 **节点前提在真集群实测**：用只读短命 Pod 在 cloud3 确认内核 6.8 / BTF / tracefs 路径 /
      hostPID 可见宿主进程 / 特权能力（结论写进 `design.md` 的「目标平台」表与 `deploy/k8s/README.md`）
- [x] 1.6 **集群自签 CA 的 TLS 信任**（本 feature 的阻塞级发现，随材料一并交付代码修复）：
      `K8sCredential.ca_pem` + `kubeconfig::tls_config` + 本地 TLS 握手测试（带 CA 成功 / 不带被拒）；
      见需求 3.5。**没有它，Pod 名反查在 k3s 上必然失败且只静默退化成 uid**
- [x] 1.7 **镜像交付链路实测**：`build-image.sh --pkg ... --import cloud3` 走
      `docker save | ssh cloud3 'sudo -n k3s ctr images import -'`，6 秒导入、`k3s ctr images ls` 可查
      （集群内无 registry，脚本已处理 containerd socket 的 root 权限）

## 1b. server 组件进集群（用户决定，2026-09-25）

> 用户决定：**server 侧（gse-server / dataserver / console）整体上 k3s（cloud3），
> Agent 暂不部署** —— 原设计「server 留在集群外」的口径作废，随交付更新。

- [x] 1.8 `packaging/deploy/k8s/Dockerfile` 改为**多功能镜像**（五组件二进制共用一个 scratch 镜像，
      入口由 Pod command 指定；实测 56 MB，三个入口 `--version` 正常）
- [x] 1.9 新增 `packaging/deploy/k8s/server-stack.yaml`：三 Deployment（`strategy: Recreate`）+
      4 Service（NodePort 30710/30711/30881/30990 + ClusterIP 7101）+ 3 ConfigMap；
      数据与 web dist 走 hostPath `/opt/vectorman-k8s/*`
- [x] 1.10 cloud3 实测通过：三 Pod Running；`/health`、台账 API、两套前端（含 SPA 回退）、
      Prom/SQL 口、`ingest → 落库 → edges/search` 全链路、dataplane 探活 `online`、
      **删 Pod 重建数据仍在**；过程中发现并修正「镜像 tag 不一致」与
      「`http_web_dir` 写错段导致静态托管静默关闭」两个坑（均记录在 `deploy/k8s/README.md`）
- [ ] 1.11 gse-server 的 web dist 是 **console/dist**、console 的是 **desktop/dist**（已按对应关系铺好，
      前端更新流程「npm build → scp → rollout restart」写进 README；待跑过一次更新流程确认）

## 1c. cops CD 接管（用户已确认口径，2026-09-25 问卷定稿）

> 决策记录：空库起（不搬 cloud2 数据）；cloud2 Agent 改连 cloud3（server_addr 用域名
> `vectorman.xiaoyxq.top:30710`，server 监听维持 NodePort，不加 TCPRoute）；DaemonSet 排下一轮；
> dist 打进镜像（构建上下文改源码树）；metrics 口不建 Service；cops 一次改造到位；
> cloud2 退役 = cops CD 稳定 2 天后。

- [x] 1.12 `Dockerfile` 改 multi-stage：构建上下文改为**源码树**（`npm build:console/dataplane` +
      musl release 产物），dist 打进镜像（实测 server 镜像 58.5MB，三份 dist 从镜像内
      /app/web/{gse,dataplane,console} 就地托管）；`docker build --target server|agent` 双 target，
      `--pkg` 旧路径保留兼容
- [x] 1.13 新增 `.github/workflows/release-image.yml`：tag 触发（`server/v*`、`agent/v*`）+
      workflow_dispatch 重跑；构建多功能镜像 → push `ghcr.io/abrance/vectorman-{server,gse-agent}:<v>-<短sha>`；
      推送后跑容器冒烟（--version 必须含 tag 版本）；发布 summary 给出 cops 侧只 bump tag 的指引
      （与 modelman 的 release-logcluster.yml 同构）。
      **已端到端验证**：`server/v1.2.0` tag → GHCR 推送成功（容器冒烟 --version 一致），
      cloud3 `k3s ctr images pull ghcr.io/abrance/vectorman-server:v1.2.0-6d7131e` 4.1 秒拉完。
      实推踩坑两个：① docker driver 不支持 gha cache export，必须 `docker/setup-buildx-action`；
      ② 冒烟断言要用剥掉 v 的版本（vectorman-version 设计如此）；
      ③ tag 必须打在**包含 workflow 文件的 commit** 上，否则不触发（tag 用 tag 指向 commit 的定义）。
      （cops 的 check-registries 守卫只放行 ghcr.io）；镜像 tag 与 cops 单元 `.env` 的
      `*_IMAGE_TAG` 对齐（只 bump tag 行）
- [x] 1.14 `server-stack.yaml` 增加三条域名 × 两条 IngressRoute（web 跳转 + websecure +
      `tls.certResolver letsencrypt`，沿用 cops whoami/model-logcluster 的已验证写法）；
      ConfigMap/Deployment 不动；**DNS 先生效再 apply**。
      已实测：三域名证书签发成功（verify=0，SAN 正确）、80→443 308 跳转、
      三 UI + API 同域可用、30710 TCP 握手通、幂等 apply 不重启 Pod。
      域名以 DNS 实际注册为准：`vectorman`/`dataserver`/`console`（早期写的 gse./data. 未注册，已改口径）
- [ ] 1.15 删 Pod 重建数据仍在、`rollout restart` 无 CrashLoop、cron 全量 apply 幂等
      （连续两晚 Pod AGE 增长）—— 作为 cops 接管后的验收
- [x] 1.16 **三个 Agent 全部迁移到 cloud3 的 server**（2026-09-26 完成）：
      台账：`hosts/{cloud2,debian12,bkee5}` + `agents/{cloud2-agent,debian12-agent,testbkee}`；
      三者均 `server_addr = "vectorman.xiaoyxq.top:30710"`，`GET /api/gse/agents` 全部 **online**
      （心跳持续推进），server 端日志「agent cloud2-agent authenticated」；
      testbkee 另验证了下发作业通道（`vmctl jobs submit` → succeeded/exit 0）。
      ④ 旧 server（cloud2 的 127.0.0.1:7100）暂未停（不搬运数据，等 cron 幂等验收后一并退役）。
      **三处踩的是同一个坑**：端口写成 `7100`（只在 k3s 集群内监听）而非 `30710`；
      token 用的是旧 server 关鉴权时的占位值（新 server `auth_enabled=true`，必须台账登记后重发）。

- [x] 1.17 **cops CD 接管 server 侧**（2026-09-26 完成，cops PR #61）：
      `apps/vectorman/` 由 native 改为 k8s 单元 —— 新增 `k8s.yaml`（18 个对象：
      1 Namespace + 1 Middleware + 3 ConfigMap + 3 Deployment + 4 Service + 6 IngressRoute），
      `app.conf` 改 `DEPLOY_MODE=k8s / DEPLOY_TARGET=cloud3`，`.env` 改镜像 + 域名 + hostPath；
      删除 `native/` 与 `conf/`（配置进 ConfigMap，二进制进镜像）；
      `deploy.yml` 去掉 `VECTORMAN_SUDO_PASS`。
      **通用机制**：`deploy-k8s.sh` 新增 ConfigMap checksum 注解 —— apply 后把每个 Deployment
      引用的 ConfigMap 内容哈希写进 podTemplate.annotations，内容变则自动滚动。
      没有它时改 ConfigMap 是「部署成功但仍跑旧配置」的静默漂移，健康探测看不出来。
      只处理本单元声明的 ConfigMap；对无 ConfigMap 的单元（model-ocr/model-logcluster）实测 no-op。
      **cloud3 实测**：渲染产物与迁移前手工部署的现网 manifest 同对象集（18 个）、三个 ConfigMap
      逐字一致；跑三次 `deploy-k8s.sh`：首次注入+滚动 → 二次全部「未变」Pod 名不变（幂等）
      → 改一个 ConfigMap **只滚动 dataserver**（精准）；三域名 /health 全 200、Agent RPC 30710 通。
      踩坑（已写进脚本注释）：`kubectl annotate deploy` 改的是 Deployment 自身 metadata、
      **不改 podTemplate 因而不触发滚动**，必须 patch `spec.template.metadata.annotations`；
      jsonpath 含点注解 key 必须转义（否则读回空值 → 每次部署都滚动）；
      `kubectl -o json` 是 4 空格缩进，awk/sed 按 2 空格解析会静默失配（改用 `-o jsonpath={.data}`）；
      k3s 主机**没有 jq**、不保证有 python3（checksum 段只用 kubectl/awk/grep/sha256sum）。

- [x] 1.18 **cloud2 旧 server 退役**（2026-09-26 完成）：
      `vectorman-{gse-server,dataserver,console}` 三个 unit 已 `stop` + `disable`
      （`vectorman-gse-agent` 保留运行 —— 它已改连 cloud3）；旧端口 7100/7101/8081/9090/7200
      全部释放；开机不再自启。退役后 cloud3 全链路复验：三 Agent online、三域名 /health 200、
      Agent RPC 30710 通。**按「空库起」决定，cloud2 的 49MB 数据未迁移**，旧数据留在
      `/opt/vectorman/dataserver/data/`（unit 已停，仅作归档）。
      **退役前的排查发现（值得记）**：cloud2 的 `8081` 上曾有 3 个来自 `115.231.78.4`
      （杭州电信 IDC，非本项目任何主机）的 ESTAB 长连接。排查判定为**端口探测型扫描**而非
      数据源 —— 依据：①连接收发队列恒为 0；②dataserver 的 `/v1/streams` 计数两次采样完全
      不变（`accepted` 停在 27/2）；③旧 server 台账里 `gs`/`testbkee` 均已 offline；
      ④停服后连接立即消失。
- [ ] 1.20 **gse-server 会话生命周期修复 → 已拆为独立规格**（2026-09-26）：
      1.18 升级 testbkee 时发现的第二个真 bug —— 连接断开后会话不被清理、
      心跳又不断刷新 `last_seen`，导致死会话永远停在 `Online`，作业下发拿到它
      必失败（`multiplexer closed` → `lost`，错误措辞还写成「agent offline」）。
      根因：`server.rs:170` 的 `handle_conn` 只注册 handler 就返回，
      **没有任何连接生命周期管理**。
      该修复涉及会话状态机、心跳语义、API/前端展示三层，已拆为独立规格
      **`.monkeycode/specs/gse-session-liveness/`**（沿革：本条只作指向）。
- [ ] 1.19 **dataserver 鉴权加固（用户决定「以后再做」，2026-09-26 记录）**：
      `dataserver` 的 SQL/接入口（cloud3 的 `8081`，公网 `https://dataserver.xiaoyxq.top`）
      当前 `[auth] enabled = false` —— **公网任何人扫到即可 `POST /v1/sql` 查库、
      `POST /v1/ingest` 写数据**。cloud3 侧仅多了 TLS（Traefik），**没有认证**。
      1.18 的扫描事件已证实公网存在主动探测（虽然只探了端口没探路径）。
      加固动作：`dataserver` 开 `[auth] enabled = true` + 客户端带 token（与 gse-server 的
      台账鉴权口径对齐）；同步更新 `k8s.yaml` 的 ConfigMap 与 Agent 侧配置。
      ⚠️ 与 1.18 的扫描事件是同一根因，优先级不低。

## 2. 单机回归清单（对应需求 5、9）

- [ ] 2.1 把设计里的 V12 三条命令固化成可复制的清单（写进 `ebpf-observability/todo.md` 附录或
      `packaging/deploy/k8s/README.md` 的「单机先验」一节）
- [ ] 2.2 执行限流回归：`checkpoint --kind ebpf_syscall` 出现限流丢弃日志且
      `agent_ebpf_rate_limited_total > 0`
- [ ] 2.3 执行缓冲淘汰回归：指向不可达 ingest 地址持续产数据，出现 `drop oldest batch` 日志且
      `agent_ebpf_buffer_dropped_total > 0`
- [ ] 2.4 执行「drain 即增量」回归：专用进程每轮稳定调用数，四轮计数与真实值一致
      （防止少计 bug 回归）

- **检查点 B**：三条回归结论（命令 + 输出要点）落盘到 `ebpf-observability/todo.md`。

## 3. 集群验证执行（对应需求 1–4、9）

- [ ] 3.1 环境准备：确认节点内核 ≥5.8 与 `/sys/kernel/btf/vmlinux` 存在；确认集群外
      gse-server 与 dataserver 可从节点访问；构建并推送验证用镜像
- [ ] 3.2 预登记台账：为每个节点登记 Agent（`agent_id = nodeName`，token 写入 Secret）
- [ ] 3.3 灰度单节点：`kubectl apply` 后只调度到一台，按设计观察 10–30 分钟
      （日志挂载数、`bpftool prog list`、`dmesg` 无 verifier/kprobe/soft lockup、节点 CPU 无异常）
- [ ] 3.4 下发四类 eBPF 采集项（前端「采集链路」页或 API），确认 `/v1/ebpf/capability` 全部 `available=true`
- [ ] 3.5 验证 V5/V6：`src_container_id` 为真实容器 ID、`src_pod` 为**真实 Pod 名**、
      服务名归一能命中按 Pod 名登记的端点
- [ ] 3.6 验证 V7：`/v1/streams` 出现三类流；`/v1/edges/search` 有量；Prom 有 `ebpf_edge_*` 与
      `ebpf_process_*`；`/ebpf` 与 `/topology` 页可见
- [ ] 3.7 容量与丢弃被动观测（V8）：记录实测规模（活跃连接数、Agent 数、事件率）与四项丢弃计数、
      采集侧与接入侧对账差额
- [ ] 3.8 铺开全部节点，重复 3.4–3.7 的抽查项
- [ ] 3.9 验证 V9/V10：Pod 重建后重新挂载且不重复计数；停用采集项/删 DaemonSet 后
      `bpftool prog list` 与 `bpftool map list` 无残留
- [ ] 3.10 验证 V11：在没有 BTF 或无特权的环境启动，确认只降级该项且原因可见

- **检查点 C**：V1–V11 逐条结论（命令 + 期望 vs 实测 + 证据）写入
  `ebpf-observability/todo.md` 的「验证结论」一节；未通过项单列并给出影响面。

## 4. 规格与实现对齐（对应需求 6）

- [x] 4.1 （随设计 PR 一并完成）`ebpf-observability/requirements.md` 的 9.3 改为「每次 drain 的结果即本周期增量；
      内核侧已写零复位」，并注明修正原因（避免再次被照抄成差分实现）
- [x] 4.2 （随设计 PR 一并完成）`ebpf-observability/design.md` 的图表选型由 `echarts` 改为**手绘 SVG**；
      删除或改正「`/ebpf/profile` 本期占位路由」这类与实现不符的描述
- [x] 4.3 （2026-09-25 收口）DNS 冲突已定：**先降级**为「仅 `sendto`/`recvfrom` 形态」；
      需求 7.1 与 `todo.md` 已同步（含真实影响：glibc 2.34+ 的 connect+send/recv 采不到，
      实现前先用数据核对形态比例，必要时再扩挂载点）
- [ ] 4.4 复查 `ebpf-observability/todo.md` 的「已定选型」表与 `tasklist.md` 的勾选状态一致
      （P2 前半已完成、P2 后半与 P3 未开工）

- **检查点 D**：`grep -rn "echarts" .monkeycode/specs/ebpf-observability/` 无残留；
  需求 9.3 与实现口径一致；DNS 冲突有明确结论；全部 Rust/前端测试与 CI 保持绿。

## 5. 明确不做的收口（对应需求 7）

- [x] 5.1 删除 `.github/workflows/helm-ci.yml`（`charts/**` 已不存在，属悬空配置），
      在 `todo.md` 记录「charts 排 v1.3、届时一并恢复该工作流」
- [ ] 5.2 在 `ebpf-observability/todo.md` 记明「前端浏览器 e2e 本版不做」及其覆盖边界
- [ ] 5.3 在 `todo.md` 记明 `ebpf_dns` 与 `ebpf_cpu_profile` 排在本 feature 之后（各自先出设计）

- **检查点 E**：`.github/workflows/` 下无悬空工作流；三项「不做」在文档中有结论与理由；
  CI 全绿且无新增作业耗时异常。

## 6. 文档与运维入口（对应需求 1、4、9）

- [x] 6.1 （随设计 PR 一并完成）README 的 eBPF 章节补三条：**默认不下发采集项就不加载任何 BPF 程序**；
      上线建议（先单节点灰度 → 观察丢弃计数与 `dmesg` → 铺开）；回滚方式（停用采集项/停 Agent）；
      指向 `observability-hardening/design.md` 的风险与回滚清单
- [ ] 6.2 `ebpf-observability/todo.md` 增「验证结论」一节（记录实测规模、四项丢弃计数、对账结果、遗留风险）
- [ ] 6.3 若验证中发现实现缺陷：单开修复 PR（不在本 feature 里夹带功能改动），
      并在 `todo.md` 记录缺陷与修法

- **检查点 F**：文档与实测一致；无未决占位；交付说明（PR 描述）含验证结论摘要与遗留风险。
