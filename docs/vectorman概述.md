## what

vectorman， 它将是一个 aiops 平台，集成通用的组件，可以轻易基于此扩展的项目。愿景是无需固定到某一种 aiops 的生态。

## 二进制组件

组件 是 指 编译好的二进制，我希望 在同一个产品下的有些尽量不要设计为多个二进制，而是使用 子命令 这种形式，以免二进制过多。

## vectorman 架构设计

目前已经有了 各种数据存储的脚手架，已经能支持 文件存储，kv 存储， 关系型数据存储，日志型数据存储，时序数据存储。我这里会将 

数据管道 + 数据存储 直接放到一个组件了，暂时还没有打算 接入 influxdb es redis 七牛云s3 mysql 这些成熟组件。

现状我的 v1.1 的目标是先把 vectorman 准备一些通用的 crate ， 把做出一些组件的能力做出来，如 环境变量读取，运行时的设置也做出来，每个组件如果使用了 关系型数据库，因为

使用了 sqlite，每个组件都需要单独的运行目录，目前用统一的目录结构，如 bin/ scripts/ config/config.toml 等，反正就是要有一个统一的合理的目录结构设计

组件也需要整理出基本的一版出来。

cmdb 配置平台

node 节点管理

job 作业平台: 分为 我希望在建立 ssh 执行作业时，用反向隧道，使用 https://github.com/singchia/geminio-rs 这个仓库去实现 agent 与 server 之间的通信。  

```
# 这是一些完整的技术架构设计的组件，可以先只实现 主要的组件即可
job-frontend
job-gateway
job-analysis
job-backup
job-config-watcher
job-crontab
job-execute
job-file-gateway
job-file-worker-headless
job-logsvr
job-manage

```


全局调度引擎实现

```

# 待拆分 组件 ，目前可确定，肯定有 Server 和 Agent 两个二进制组件，但是是否还需要分出其他二进制组件，还不确定，或者说就算 服务端进程分多个，但是还是公用一个二进制执行不同子命令

组件名称	所属模块	角色定位	设计目标
GSE Cluster
（基础平台服务）	GSE 核心	底座服务	为 GSE 所有上层模块提供统一的 Agent 会话管理与信令上下行通道
GSE Task
（任务服务）	GSE 核心	任务执行	提供远程命令的编排、下发与结果回收能力
GSE File
（文件服务）	GSE 核心	文件传输	提供大文件在 Server 与 Agent 之间的高效分发与下载能力
GSE Proc
（进程管理服务）	GSE 核心	进程托管	对 Agent 机器上的进程进行托管式生命周期管理
GSE Data
（数据服务）	GSE 核心	数据传输	提供海量运维采集数据的全链路传输、路由分发与管道管理
GSE Data Server	GSE Data	数据路由引擎	维护 data_id 路由表，将采集数据精准投递到各消费方
Proxy	GSE 管控	非直连区域桥梁	在网络隔离场景中充当 Server 与 Agent 之间的中转节点
Agent
（直连 Agent）	GSE 管控	直连执行端	部署在直连区域目标机器上，主动外连 Server，接收指令并执行
p-agent
（非直连 Agent）	GSE 管控	非直连执行端	部署在非直连区域目标机器上，通过 Proxy 中转与 Server 通信

```


