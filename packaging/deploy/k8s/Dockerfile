# Vectorman 的容器镜像（multi-stage，构建上下文 = **源码树根目录**）。
#
# 产出两个镜像（按 --target 选择）：
#   --target agent   -> vectorman-gse-agent:<tag>    （内核态程序 + 用户态采集）
#   --target server  -> vectorman-server:<tag>       （gse-server/dataserver/console/dpc/vmctl + web dist）
#
# web dist 在 server 镜像里就地构建（npm ci + vite build），不再依赖手工 scp：
#   docker build --target server -t vectorman-server:<tag> .
#   docker build --target agent  -t vectorman-gse-agent:<tag> .
#
# Rust 侧对齐 packaging/build-package.sh 的既有姿势（musl-gcc + musl target，
# 不设 LINKER：rust-lld 会 ring/ureq SIGSEGV）。eBPF .o 已入库（packaging/ebpf/*.o），
# build.rs 直接 include_bytes! 内嵌 —— 镜像里**不需要** bpf-linker/LLVM。
# ── 1) 前端 dist ──────────────────────────────────────────────────────────
FROM node:24-bookworm-slim AS web
WORKDIR /src/frontend
COPY frontend/package.json frontend/package-lock.json ./
COPY frontend/apps ./apps
COPY frontend/packages ./packages
RUN npm ci --no-audit --no-fund
RUN npm run build:console && npm run build:dataplane
# desktop 是 console 桌面门户的产物
RUN if [ -d apps/desktop ]; then npm run build:desktop; fi

# ── 2) Rust 静态二进制（musl）────────────────────────────────────────────
FROM rust:1-bookworm AS rust
# 版本注入：build-push-action 的 build-args 传 VECTORMAN_VERSION（与 tag 一致）；
# 本地 docker build 不传时回退 git describe（仓库语义不变）。
ARG VECTORMAN_VERSION
WORKDIR /src
RUN apt-get update && apt-get install -y --no-install-recommends musl-tools && rm -rf /var/lib/apt/lists/* \
    && rustup target add x86_64-unknown-linux-musl
COPY . .
# 与 build-package.sh 相同的环境语义；一次构建全部六个静态二进制。
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    CC_x86_64_unknown_linux_musl=musl-gcc \
    VECTORMAN_VERSION="${VECTORMAN_VERSION}" \
    cargo build --release --workspace --target x86_64-unknown-linux-musl

# ── 3a) server 运行镜像：scratch + 五件二进制 + web dist ────────────────
FROM scratch AS server
COPY --from=rust /src/target/x86_64-unknown-linux-musl/release/dataserver   /usr/local/bin/dataserver
COPY --from=rust /src/target/x86_64-unknown-linux-musl/release/gse-server   /usr/local/bin/gse-server
COPY --from=rust /src/target/x86_64-unknown-linux-musl/release/console      /usr/local/bin/console
COPY --from=rust /src/target/x86_64-unknown-linux-musl/release/dpc          /usr/local/bin/dpc
COPY --from=rust /src/target/x86_64-unknown-linux-musl/release/vmctl        /usr/local/bin/vmctl
COPY --from=web /src/frontend/apps/console/dist/   /app/web/gse/
COPY --from=web /src/frontend/apps/dataplane/dist/ /app/web/dataplane/
COPY --from=web /src/frontend/apps/desktop/dist/   /app/web/console/
# 运行期布局由 Pod 的 workingDir + subPath 决定（见 server-stack.yaml）：
#   /app/web/gse -> gse-server 的 http_web_dir（console/dist 产物，GSE 前端）
#   /app/web/dataplane -> dataserver 的 http_web_dir
#   /app/web/console -> console 的 web_dir（desktop/dist 产物，桌面门户）
ENTRYPOINT ["/usr/local/bin/gse-server"]

# ── 3b) agent 运行镜像：scratch + 静态二进制 ─────────────────────────────
FROM scratch AS agent
COPY --from=rust /src/target/x86_64-unknown-linux-musl/release/gse-agent /usr/local/bin/gse-agent
ENTRYPOINT ["/usr/local/bin/gse-agent"]
