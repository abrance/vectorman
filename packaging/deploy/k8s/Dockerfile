# gse-agent 的容器镜像：scratch + 静态链接（musl）二进制，无运行时依赖。
#
# 构建上下文必须是**发布包解压后的根目录**（内含 gse-agent/bin/gse-agent 与 gse-agent/conf/）：
#   docker build -f packaging/deploy/k8s/Dockerfile -t vectorman-gse-agent:<version> /tmp/vectorman-1.2.0
# 或直接用 build-image.sh（并把镜像送进 k3s）。
FROM scratch

COPY gse-agent/bin/gse-agent /usr/local/bin/gse-agent
# 配置由 ConfigMap 挂到 /etc/vectorman/gse-agent.toml（见 gse-agent-daemonset.yaml）；
# 这里只放一份示例，方便进容器对照字段。
COPY gse-agent/conf/gse-agent.toml.example /usr/local/share/gse-agent.toml.example

# 需要的外部依赖都由 Pod 提供：
#   /proc            <- hostPID: true（宿主视图）
#   /sys/kernel/btf  <- hostPath 只读
#   /sys/kernel/tracing <- hostPath 只读
#   /tmp             <- emptyDir
ENTRYPOINT ["/usr/local/bin/gse-agent"]
