# gse-server / dataserver / console 的 K8s 镜像与部署
#
# 二进制都是静态链接（musl），一个 scratch 镜像装下全部 server 侧组件；
# 入口由 Pod 的 command 指定，web dist 用 hostPath/emptyDir 挂载（不烘进镜像）。
#
# 构建上下文 = 发布包解压后的根目录（内含 <component>/bin/<bin> 与 <component>/web/）：
#   docker build -f packaging/deploy/k8s/Dockerfile -t vectorman-server:1.2.0 /tmp/vectorman-1.2.0
FROM scratch

COPY dataserver/bin/dataserver   /usr/local/bin/dataserver
COPY gse-server/bin/gse-server   /usr/local/bin/gse-server
COPY console/bin/console         /usr/local/bin/console
COPY dpc/bin/dpc                 /usr/local/bin/dpc
COPY vmctl/bin/vmctl             /usr/local/bin/vmctl
# 配置示例随镜像（实际配置用 ConfigMap 挂载）。
COPY dataserver/conf/config.toml.example           /usr/local/share/dataserver/config.toml.example
COPY gse-server/conf/gse-server.toml.example       /usr/local/share/gse-server/gse-server.toml.example
COPY console/conf/console.toml.example             /usr/local/share/console/console.toml.example

# 入口由 Pod spec 的 command 决定；这里给个能自证的默认值。
ENTRYPOINT ["/usr/local/bin/gse-server"]
