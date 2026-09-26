# gse-agent 的 K8s 部署材料（DaemonSet）

每节点一份 gse-agent，用宿主机内核跑 eBPF 采集，并把容器/Pod 反查结果写进边记录。
本版**不引入 helm charts**（排 v1.3），只给一份自包含清单 + 一个造镜像脚本。

```
packaging/deploy/k8s/
├── Dockerfile                  # scratch + 静态链接（musl）二进制，无运行时依赖
├── build-image.sh              # 造镜像；--import <ssh-host> 直接送进 k3s（单机无 registry 时用）
├── gse-agent-daemonset.yaml    # Namespace/SA/RBAC/ConfigMap/Secret/DaemonSet 六件套
└── README.md                   # 本文档
```

## 0. 前提（先核节点，别跳过）

节点要满足 eBPF 的前提：**内核 ≥ 5.8 且带 BTF**，并且 tracepoint `format` 可读
（代码读的就是 `/sys/kernel/tracing/events/**/format`）。在**每个**节点上核一遍：

```bash
uname -r                                    # ≥ 5.8
ls -l /sys/kernel/btf/vmlinux               # 存在（网络/TCP 采集项要它；syscall 项不需要）
ls /sys/kernel/tracing/events/sock/inet_sock_set_state/format   # 存在且可读
```

不想 ssh 逐台核，用一个只读短命 Pod 一次性确认（含 hostPID 可见性）：

```bash
kubectl apply -f - <<'YAML'
apiVersion: v1
kind: Pod
metadata: { name: ebpf-precheck }
spec:
  restartPolicy: Never
  hostPID: true
  containers:
    - name: c
      image: busybox:1.36
      securityContext: { privileged: true }
      command: ["sh","-c","uname -r; ls -l /btf/vmlinux; head -c 50 /tracing/events/sock/inet_sock_set_state/format; ls /proc | grep -c '^[0-9]'"]
      volumeMounts:
        - { name: tr, mountPath: /tracing, readOnly: true }
        - { name: btf, mountPath: /btf, readOnly: true }
  volumes:
    - { name: tr, hostPath: { path: /sys/kernel/tracing } }
    - { name: btf, hostPath: { path: /sys/kernel/btf } }
YAML
kubectl logs ebpf-precheck; kubectl delete pod ebpf-precheck
```

已验证的环境（2026-09-25，cloud3 k3s 单节点）：

| 项 | 实测值 |
| --- | --- |
| 内核 | `6.8.0-48-generic`（Ubuntu 24.04，k3s v1.37） |
| BTF | `/sys/kernel/btf/vmlinux` 存在（6.0 MB） |
| tracefs | `/sys/kernel/tracing/events/sock/inet_sock_set_state/format` 可读 |
| hostPID | 容器内可见 271 个宿主进程（容器/Pod 反查的前提） |
| 特权 | `CapEff` 全量；`ulimit -l` 8192（内核 ≥ 5.11 的 BPF 内存走 memcg 记账，不受该值限制） |

## 1. 造镜像并送进集群

**现在的主路径是从源码树构建**（multi-stage：node 构建前端 dist → rust 构建六件 musl 静态二进制
→ scratch 运行镜像），dist 不再需要手工 scp：

```bash
# server 镜像（五件二进制 + 三份 dist，实测 58.5MB）
docker build --target server -t vectorman-server:<tag> -f packaging/deploy/k8s/Dockerfile .
# agent 镜像（eBPF .o 已入库，不需要 bpf-linker）
docker build --target agent  -t vectorman-gse-agent:<tag> -f packaging/deploy/k8s/Dockerfile .
```

`build-image.sh --pkg <发布包目录>` 仍可用（scratch + 发布包里现成二进制的旧路径），
但从源码构建请直接用上面的 `docker build --target`。

送进单机 k3s（无 registry）：

```bash
docker save vectorman-server:<tag> | ssh cloud3 'sudo -n k3s ctr images import -'
```

集群里有 registry 时（cops CD 接管后是 ghcr.io）：`docker tag` + `docker push`，
清单里的 `image:` 换成仓库地址。

## 2. 台账预登记（每个节点一个 Agent）

Agent 必须先登记才能注册心跳；`agent_id` 用**节点名**（清单里由 Downward API 注入），
`token` 与清单的 Secret 保持一致：

```bash
GSE=https://vectorman.xiaoyxq.top   # 台账 API 走 443（RPC 是 30710）
# 主机（host_id 用节点名即可）
curl -sS -X POST $GSE/api/gse/hosts -H 'Content-Type: application/json' \
  -d '{"host_id":"ser539375215934"}'
# Agent
curl -sS -X POST $GSE/api/gse/agents -H 'Content-Type: application/json' \
  -d '{"agent_id":"ser539375215934","host_id":"ser539375215934","token":"<token>","version":"1.2.0"}'
# 核对
curl -sS $GSE/api/gse/agents | head -c 400
```

## 3. 改清单里的三处占位，然后 apply

```bash
# gse-agent-daemonset.yaml 里替换：
#   ConfigMap  server_addr = "vectorman.xiaoyxq.top:30710"   -> server 的 RPC NodePort
#              （域名只做 DNS A 解析；客户端不要写 :7100，那是集群内端口）
#   Secret     token: "REPLACE_ME"               -> 上一步登记用的 token
#   DaemonSet  image: vectorman-gse-agent:1.2.0  -> 第 1 步造的 tag
kubectl apply --dry-run=server -f packaging/deploy/k8s/gse-agent-daemonset.yaml   # 先干跑
kubectl apply -f packaging/deploy/k8s/gse-agent-daemonset.yaml
```

**灰度**（推荐先一台）：

```bash
kubectl -n vectorman patch ds gse-agent --type merge -p \
  '{"spec":{"template":{"spec":{"nodeSelector":{"kubernetes.io/hostname":"<node>"}}}}}'
# 铺开：去掉 nodeSelector
kubectl -n vectorman patch ds gse-agent --type merge -p \
  '{"spec":{"template":{"spec":{"nodeSelector":null}}}}'
```

## 4. 验证

```bash
kubectl -n vectorman get pods -o wide                     # 每节点 1 个 Ready
kubectl -n vectorman logs ds/gse-agent --tail=50          # 看挂载/降级日志

# 集群侧：纳管与能力
curl -sS https://vectorman.xiaoyxq.top/api/gse/agents | head -c 400  # 状态 online、心跳推进
curl -sS https://vectorman.xiaoyxq.top/api/gse/agents/<node>/collect-items  # 采集项下发了什么

# 数据面：能力、边记录与 Pod 名
curl -sS https://dataserver.xiaoyxq.top/v1/ebpf/capability | head -c 400
curl -sS -X POST https://dataserver.xiaoyxq.top/v1/edges/search \
  -H 'Content-Type: application/json' -d '{"limit":5}' | head -c 800
#   关注 src_container_id 是真实容器 ID、src_pod 是真实 Pod 名（不是 pod<uid>）
```

Prom 侧看四项丢弃计数（见 `observability-hardening/design.md` 的容量口径）：

```
sum(agent_ebpf_buffer_dropped_total)
sum(agent_ebpf_rate_limited_total)
sum(agent_ebpf_map_overflow_dropped_total)
sum(agent_ebpf_read_errors_total)
agent_ebpf_cpu_percent
```

## 5. 回滚

```bash
# 方式一：停用采集项（Agent 侧 detach + 删 map，集群里 Pod 继续跑）
curl -sS -X DELETE "$GSE/api/gse/collect-items/<item_id>"
# 方式二：删 DaemonSet
kubectl -n vectorman delete ds gse-agent
# 方式三：删清单全部对象（含 RBAC/ConfigMap/Secret）
kubectl delete -f packaging/deploy/k8s/gse-agent-daemonset.yaml
```

三种方式都**不需要改内核、不需要重启节点**。卸完可确认无残留：

```bash
sudo bpftool prog list | grep -c gse || echo "无残留"
sudo bpftool map list  | grep -c gse || echo "无残留"
```

## 5b. server 组件上 k3s（gse-server / dataserver / console）

> **已在 cloud3 实测（2026-09-25）**：三 Deployment 全部 Running；`/health`、台账 API、
> GSE/数据面前端（含 SPA 回退）、Prom/SQL 口、`ingest → 落库 → edges/search` 全链路通过；
> dataplane 探活 `online`；**删 Pod 重建后数据仍在**（hostPath 生效）。
> 部署期踩的两个坑已写进下文设计要点（镜像 tag 要一致；dataserver 的 `http_web_dir`
> 是**顶层键**，写进 `[sql_http]` 段会静默失效）。

server 侧也可以整体进集群（与 Agent 的 DaemonSet 同一命名空间 `vectorman`）：

```bash
# 1) 造镜像（从源码树，dist 已在镜像里；见第 1 节）
docker build --target server -t vectorman-server:<tag> -f packaging/deploy/k8s/Dockerfile .
docker save vectorman-server:<tag> | ssh cloud3 'sudo -n k3s ctr images import -'

# 2) 准备节点目录（数据卷；hostPath 不会自动创建父目录）
ssh cloud3 'sudo -n mkdir -p /opt/vectorman-k8s/gse-server /opt/vectorman-k8s/dataserver /opt/vectorman-k8s/console'

# 3) 干跑 + 应用（公网入口是六条 IngressRoute，三个域名各两条）
kubectl apply --dry-run=server -f packaging/deploy/k8s/server-stack.yaml
kubectl apply -f packaging/deploy/k8s/server-stack.yaml
kubectl -n vectorman get pods,svc,ingressroute
```

访问入口（节点 IP + NodePort）：

| 组件 | 用途 | 入口 |
| --- | --- | --- |
| gse-server | Agent RPC（`server_addr = "vectorman.xiaoyxq.top:30710"`，DNS 解析到节点 IP 直打 NodePort，不过 Traefik） | NodePort 30710 |
| gse-server | 台账 API + GSE 前端 | `https://vectorman.xiaoyxq.top` |
| dataserver | 数据面前端 + 查询（同源，相对路径；含 Prom 路由） | `https://dataserver.xiaoyxq.top` |
| console | 桌面门户 | `https://console.xiaoyxq.top` |

> 域名名以腾讯云 DNS 实际添加为准（2026-09-26 实测：`vectorman`/`dataserver`/`console` 三条
> 已生效；早期文档里写的 `gse.`/`data.` 未注册，已按现状改口径）。NodePort 仍保留：
> 30711/30881/30990/30720 是同一服务的另一条直达路径（CI 探活与排障用），
> 对外推荐统一走 HTTPS 域名。

**DNS 与 Agent 连接口径**（2026-09-25 定稿）：server 侧只负责「IP:port 监听」（NodePort），
**客户端配置一律写「域名:port」**——`gse-agent` 的 `server_addr = "vectorman.xiaoyxq.top:30710"`。
三个子域名都解析到节点 IP（`186.244.201.55`）：

| DNS 记录（实际注册名） | 指向 | 用途 |
| --- | --- | --- |
| `vectorman.xiaoyxq.top` | 186.244.201.55 | Traefik → gse-server:7101（GSE 前端/台账 API）+ Agent RPC 30710（直连 NodePort） |
| `dataserver.xiaoyxq.top` | 186.244.201.55 | Traefik → dataserver:8081 |
| `console.xiaoyxq.top` | 186.244.201.55 | Traefik → console:7200 |

Agent RPC 不加 TCPRoute：域名只是 IP 的名字，连 `server_addr` 解析后直打 NodePort。
前置：腾讯云轻量防火墙放行 TCP 30710（80/443 通常已放行）。

关键设计（都有代码事实支撑，不是想当然）：

- **数据落盘**：gse-server 的 `db` 与 dataserver 的 `data_path` 都走
  `hostPath /opt/vectorman-k8s/<组件>`（单节点 k3s，不引入 PVC/local-path）；
  两个 Deployment 都用 `strategy: Recreate` —— sqlite/tsink 是文件型存储，滚动更新会双开进程写同一文件。
- **前端 dist 用 hostPath**（不用 ConfigMap）：dataplane 的单个 js 就有 1.2 MB，
  超 ConfigMap 的 1 MiB 单文件限制；`scp` + 重启即可更新，不用重造镜像。
- **同源部署**：前端是相对路径请求（`/v1/...`、`/api/sql/v1/sql`），dataserver 的
  `http_web_dir` 挂 dist 后同端口托管（SPA 回退 index.html），无需改前端、无需 Ingress。
- **配置全走 env/ConfigMap**：`GSE_SERVER_CONFIG`、`CONSOLE_CONFIG`、`--config`（dataserver）、
  `DP_DATA_PATH`；监听改绑 `0.0.0.0`（Pod 网络里没有 localhost 之外的可达性）。
- **台账联动**：dataserver 的 `gse_admin_url` 指向集群内 Service
  `http://gse-server-internal.vectorman.svc:7101`（清理器按台账 live 采集项决定保留范围）。
- **CLI 不进集群**：`dpc` / `vmctl` 是运维命令行，留在能访问 NodePort 的机器上用即可。
- **镜像 tag 必须与清单一致**：清单 `image: vectorman-server:1.2.0` 而导入的是 `...:1.2.0-dev`
  会 `ErrImagePull`（k3s 不会去 docker.io 找这个不存在的仓库）。
- **dataserver 的 `http_web_dir` 是顶层键**：写进 `[sql_http]` 段会被解析成
  `sql_http.http_web_dir`，顶层仍为 None，静态托管**静默关闭**（日志只有一行 404，无报错）。
  `gse_admin_url` 同理是顶层键。核对方法：`kubectl get cm dataserver-conf -o jsonpath='{.data.config\.toml}'`。
- **删 Pod 后立刻重建可能撞 redb 文件锁**（旧容器还没完全退出 →
  `open redb: Database already open. Cannot acquire lock.` → CrashLoop，等旧容器 Exited 后自动恢复）。
  换镜像/改配置时用 `kubectl rollout restart`（先停旧再起新），别 `delete pod` 后马上再 apply。
- **配置变更要重启 Pod**：ConfigMap 挂载内容变了但 env/pod spec 没变时不会自动滚动；
  `kubectl -n vectorman rollout restart deploy/<name>`。

## 6. 设计要点（为什么这么写）

| 写法 | 原因 |
| --- | --- |
| `hostPID: true` | 容器内 `/proc` 即宿主机视图 → `/proc/<pid>/cgroup` 能拿到 `pod<uid>` 与容器 ID（容器/Pod 反查的前提）。**不需要**再 hostPath 挂 `/proc` |
| hostPath `/sys/kernel/btf`、`/sys/kernel/tracing`（只读） | 内核结构体偏移与 tracepoint 字段偏移的来源；取不到就**该项不采集**（不按猜测值跑）。用 `type: Directory` 而不是 `DirectoryOrCreate`：路径不存在要**明确失败**，而不是造一个空目录让 Agent 悄悄退到兜底布局 |
| `privileged: true` | 加载内核态程序 + 读 tracefs。内核 ≥ 5.8 时等价写法是 `capabilities: { add: ["BPF","PERFMON"] }` |
| `GSE_AGENT_ID` = `spec.nodeName`（Downward API） | 每节点一个稳定身份；Pod 重建后仍是同一个 Agent（否则台账里会堆一堆一次性 Agent） |
| `GSE_AGENT_TOKEN` 走 Secret、不写进清单 | token 不进版本库；`GSE_AGENT_SERVER`/`GSE_AGENT_ID`/`GSE_AGENT_TOKEN` 都是官方支持的环境变量覆盖（`load_config` 里实现） |
| ServiceAccount + ClusterRole(`pods get,list`) | Pod **名**反查要跨命名空间列 Pod（内核只给 cgroup → uid）。凭据走 in-cluster SA：`kubeconfig` 留空时解析顺序是 SA → `~/.kube/config` |
| `emptyDir` 挂 `/tmp` | `scratch` 镜像没有 `/tmp`，而作业功能在未指定 `job_work_dir` 时用系统临时目录 |
| 不开 `hostNetwork` | eBPF 采集靠 tracepoint/kprobe，不抓包，不需要主机网络 |
| `otlp_enabled = false` | 集群里要让应用把 span 发到 Agent 才需要（还要配 Service/NodePort）；本版不做 |
| `tolerations: operator: Exists` | 控制面/有污点的节点同样需要节点级采集 |

**关于集群自签 CA**：k3s/kubeadm 的 apiserver 用的是**集群自签 CA**，而 Agent 的 K8s 客户端默认只信公有根
—— 因此本版让 `K8sCredential` 带上集群 CA（in-cluster 的 `ca.crt`，或 kubeconfig 的
`certificate-authority`/`certificate-authority-data`），并**只对该客户端**生效。没有这一步，Pod 名反查会以
TLS 校验失败告终，`src_pod` 会退化成 uid。

## 7. 已知限制

- 本版**不提供镜像构建流水线**：镜像是本地/节点上手工造并导入的；多架构与签名随 v1.3 的 charts 一起做。
- **OTLP 接收未开**（`otlp_enabled = false`）：集群里要让应用发 span，需要另加 Service/NodePort 并打开开关。
- **作业功能未验证**（`allowed_interpreters` 等默认值未在容器里试过）：本材料面向采集，不面向远程作业。
- Pod 名索引按 TTL 300s 刷新：新扩的 Pod 最多 5 分钟后才出现在 `src_pod` 里，不是 bug。
- 老内核（5.8–5.10）上 BPF map 走 `RLIMIT_MEMLOCK`（容器里 `ulimit -l` 可能只有 8 MiB）：
  若加载报 memlock 相关错误，加 `ulimits.memlock: -1` 或换 ≥ 5.11 的内核。
