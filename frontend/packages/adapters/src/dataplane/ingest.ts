import type { HttpClient } from "@vectorman/primitives";
import type { Agent } from "../gse/admin";
import type { PromEnvelope } from "./prom";

/// 采集项类型。
export type CollectItemKind = "metrics_host" | "log_file" | "log_k8s_stdout" | "apm_otlp";

/// 清洗提取规则。
export type ExtractRule = {
  kind: string;
  expr: string;
  label: string;
};

/// 采集项：GSE 持久化并由 dataserver 反代读写。
export type CollectItem = {
  item_id: string;
  agent_ids: string[];
  name: string;
  kind: CollectItemKind | string;
  enabled: boolean;
  collector: Record<string, unknown>;
  storage: { retention_days: number };
  updated_at?: string;
};

/// 新建/编辑采集项入参（不含 item_id）。
export type CollectItemInput = Omit<CollectItem, "item_id" | "updated_at">;

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

/// dataserver SQL 口同源客户端：采集链路、采集项、指标与日志查询。
export class DataplaneAdapter {
  constructor(private readonly http: HttpClient) {}

  listCollectItems(agentId?: string): Promise<CollectItem[]> {
    const query = agentId ? `?agent_id=${enc(agentId)}` : "";
    return this.http
      .request<CollectItem[]>({ method: "GET", url: `/v1/collect-items${query}` })
      .then((r) => r.body);
  }

  getCollectItem(itemId: string): Promise<CollectItem> {
    return this.http
      .request<CollectItem>({ method: "GET", url: `/v1/collect-items/${enc(itemId)}` })
      .then((r) => r.body);
  }

  createCollectItem(input: CollectItemInput): Promise<CollectItem> {
    return this.http
      .request<CollectItem>({ method: "POST", url: "/v1/collect-items", body: input })
      .then((r) => r.body);
  }

  updateCollectItem(itemId: string, input: CollectItemInput): Promise<CollectItem> {
    return this.http
      .request<CollectItem>({ method: "PUT", url: `/v1/collect-items/${enc(itemId)}`, body: input })
      .then((r) => r.body);
  }

  deleteCollectItem(itemId: string): Promise<void> {
    return this.http
      .request({ method: "DELETE", url: `/v1/collect-items/${enc(itemId)}` })
      .then(() => undefined);
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
