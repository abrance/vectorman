import type { HttpClient } from "@vectorman/primitives";
import type { Agent, AgentSpecView, SpecItem } from "../gse/admin";
import type { PromEnvelope } from "./prom";

/// 采集项类型。
///
/// `ebpf_*` 系列由内核态程序采集，前置条件是 Linux + 内核 ≥5.8 + BTF + root
/// （不满足时 Agent 只降级该项并上报 `agent_ebpf_capability` 说明原因）。
export type CollectItemKind =
  | "metrics_host"
  | "log_file"
  | "log_k8s_stdout"
  | "apm_otlp"
  | "ebpf_network"
  | "ebpf_tcp"
  | "ebpf_process"
  | "ebpf_syscall";

/// 清洗提取规则。
export type ExtractRule = {
  kind: string;
  expr: string;
  label: string;
};

/// 新建/编辑采集项入参；`item_id` 缺省由服务端生成，编辑既有项时要原样带回。
///
/// `SpecItem` 本身定义在 `gse/admin.ts`（spec 是控制面概念），这里只补一个「入参」形状。
export type SpecItemInput = Omit<SpecItem, "item_id"> & { item_id?: string };

/// `/v1/streams` 单条流：`data_id` 即 `item_id`。
export type StreamEntry = {
  agent_id: string;
  data_type: string;
  data_id: string;
  last_seen_micros: number;
  accepted: number;
};

/// `POST /v1/logs/search` 过滤条件；缺省字段不过滤。
export type LogSearchRequest = {
  data_type?: string;
  agent_id?: string;
  host_id?: string;
  data_id?: string;
  from_ts?: number;
  to_ts?: number;
  level?: string;
  message_query?: string;
  trace_id?: string;
  event_type?: string;
  labels?: Record<string, string>;
  limit?: number;
};

/// 日志命中的统一记录形状。
export type LogRecord = {
  id: string;
  timestamp: number;
  level: string;
  message: string;
  labels: Record<string, string>;
};

type StreamsReply = { streams: StreamEntry[] };
type LogSearchReply = { records: LogRecord[] };

function enc(id: string): string {
  return encodeURIComponent(id);
}

/// dataserver SQL 口同源客户端：采集链路总览、指标与日志查询。
export class DataplaneAdapter {
  constructor(private readonly http: HttpClient) {}

  /// 只读：per-Agent spec 列表（含期望采集项与生效状态）。
  ///
  /// 配置编辑走 console 直连 gse-server（`PUT /api/gse/agents/{id}/spec`），
  /// 数据面只提供读，避免出现两个写入口。
  listAgentSpecs(): Promise<AgentSpecView[]> {
    return this.http
      .request<AgentSpecView[]>({ method: "GET", url: "/v1/agent-specs" })
      .then((r) => r.body);
  }

  listStreams(): Promise<StreamEntry[]> {
    return this.http
      .request<StreamsReply>({ method: "GET", url: "/v1/streams" })
      .then((r) => r.body.streams);
  }

  listAgents(): Promise<Agent[]> {
    return this.http.request<Agent[]>({ method: "GET", url: "/v1/agents" }).then((r) => r.body);
  }

  queryRange(expr: string, start: string, end: string, step: string): Promise<PromEnvelope> {
    const q = new URLSearchParams({ query: expr, start, end, step });
    return this.http
      .request<PromEnvelope>({ method: "GET", url: `/api/v1/query_range?${q.toString()}` })
      .then((r) => r.body);
  }

  queryInstant(expr: string, time?: string): Promise<PromEnvelope> {
    const q = new URLSearchParams({ query: expr });
    if (time) {
      q.set("time", time);
    }
    return this.http
      .request<PromEnvelope>({ method: "GET", url: `/api/v1/query?${q.toString()}` })
      .then((r) => r.body);
  }

  searchLogs(query: LogSearchRequest): Promise<LogRecord[]> {
    return this.http
      .request<LogSearchReply>({ method: "POST", url: "/v1/logs/search", body: query })
      .then((r) => r.body.records);
  }
}
