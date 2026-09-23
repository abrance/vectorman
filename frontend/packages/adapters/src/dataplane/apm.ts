import type { HttpClient } from "@vectorman/primitives";

/// trace 列表的一行（对应 dataserver `apm_trace_summary`）。
export type TraceSummary = {
  trace_id: string;
  start_ts: number;
  duration_micros: number;
  root_service: string;
  root_operation: string;
  span_count: number;
  error_count: number;
  status: string;
  services: string[];
  collector: string;
  agent_id: string;
  host_id: string;
  data_id: string;
};

/// trace 列表过滤条件；未提供的时间范围由服务端取最近 1 小时。
export type TraceSearchRequest = {
  from_ts?: number;
  to_ts?: number;
  service?: string;
  operation?: string;
  status?: string;
  min_duration_micros?: number;
  agent_id?: string;
  host_id?: string;
  data_id?: string;
  sort?: "start_ts" | "duration_micros";
  order?: "asc" | "desc";
  limit?: number;
  offset?: number;
};

export type TraceSearchPage = {
  total: number;
  traces: TraceSummary[];
};

/// span 内的事件。
export type SpanEvent = {
  name: string;
  time_unix_nano: number;
  attributes: Record<string, string>;
};

/// span 的关联链接。
export type SpanLink = {
  trace_id: string;
  span_id: string;
  attributes: Record<string, string>;
};

/// span 详情：服务端返回写入时的完整 OTel 原文 + 派生 `duration_micros`。
/// 旧索引数据没有原文时字段可能缺失，前端按缺省处理。
export type SpanDetail = {
  record_id: string;
  trace_id: string;
  span_id: string;
  parent_span_id: string;
  name: string;
  kind: string;
  service: string;
  status_code: string;
  status_message?: string;
  start_unix_nano: number;
  end_unix_nano: number;
  duration_micros: number;
  attributes?: Record<string, string>;
  resource?: Record<string, string>;
  events?: SpanEvent[];
  links?: SpanLink[];
  dropped_events?: number;
  dropped_attributes?: number;
  dropped_links?: number;
  collector?: string;
};

export type TraceDetail = {
  summary: TraceSummary;
  spans: SpanDetail[];
  partial: boolean;
  reason?: string | null;
  expected_span_count: number;
};

export type EdgeSearchRequest = {
  from_ts?: number;
  to_ts?: number;
  src_service?: string;
  dst_service?: string;
  source?: "otlp" | "ebpf";
  agent_id?: string;
  min_requests?: number;
  limit?: number;
  offset?: number;
};

export type EdgeRow = {
  bucket_ts: number;
  src_service: string;
  dst_service: string;
  span_kind: string;
  calls: number;
  errors: number;
  duration_sum: number;
  duration_max: number;
  source: string;
  agent_id: string;
};

export type EdgeSearchPage = {
  total: number;
  edges: EdgeRow[];
};

/// 端点实例（服务清单下钻用）。
export type ServiceInstance = {
  instance_id: string;
  pod_name: string;
  node_name: string;
  host_ip: string;
  listen_port: number;
  collector: string;
  first_seen_ts: number;
  last_seen_ts: number;
};

export type ServiceRow = {
  service: string;
  instance_count: number;
  last_seen_ts: number;
  instances: ServiceInstance[];
};

/// 静态服务名映射。
export type AliasRecord = {
  alias_id: string;
  match_kind: string;
  match_value: string;
  service: string;
  enabled: boolean;
  note: string;
  updated_ts: number;
};

export type AliasInput = {
  match_kind: string;
  match_value: string;
  service: string;
  enabled?: boolean;
  note?: string;
};

/// 时序存储运行状态（容量/保留/降级）。
export type TsStorageStats = {
  series_count: number;
  memory_used_bytes: number;
  memory_budget_bytes: number;
  wal_size_bytes: number;
  retention_days: number;
  retention_enforced: boolean;
  expired_segments_total: number;
  future_skew_points_total: number;
  background_errors_total: number;
  degraded: boolean;
  last_background_error?: string | null;
  sampled_at_ts: number;
};

/// APM 查询与配置接口。
export class ApmAdapter {
  constructor(private readonly http: HttpClient) {}

  /// `POST /v1/traces/search`
  async searchTraces(req: TraceSearchRequest): Promise<TraceSearchPage> {
    const res = await this.http.request<TraceSearchPage>({
      method: "POST",
      url: "/v1/traces/search",
      body: req,
    });
    return res.body;
  }

  /// `GET /v1/traces/{trace_id}`
  async getTrace(traceId: string): Promise<TraceDetail> {
    const res = await this.http.request<TraceDetail>({
      method: "GET",
      url: `/v1/traces/${encodeURIComponent(traceId)}`,
    });
    return res.body;
  }

  /// `POST /v1/edges/search`
  async searchEdges(req: EdgeSearchRequest): Promise<EdgeSearchPage> {
    const res = await this.http.request<EdgeSearchPage>({
      method: "POST",
      url: "/v1/edges/search",
      body: req,
    });
    return res.body;
  }

  /// `GET /v1/apm/services`
  async listServices(): Promise<ServiceRow[]> {
    const res = await this.http.request<{ services: ServiceRow[] }>({
      method: "GET",
      url: "/v1/apm/services",
    });
    return res.body.services ?? [];
  }

  /// `GET /v1/apm/service-aliases`
  async listAliases(query: { match_kind?: string; enabled?: boolean } = {}): Promise<AliasRecord[]> {
    const params = new URLSearchParams();
    if (query.match_kind) {
      params.set("match_kind", query.match_kind);
    }
    if (query.enabled !== undefined) {
      params.set("enabled", String(query.enabled));
    }
    const suffix = params.toString();
    const res = await this.http.request<{ aliases: AliasRecord[] }>({
      method: "GET",
      url: `/v1/apm/service-aliases${suffix ? `?${suffix}` : ""}`,
    });
    return res.body.aliases ?? [];
  }

  /// `POST /v1/apm/service-aliases`
  async createAlias(input: AliasInput): Promise<AliasRecord> {
    const res = await this.http.request<AliasRecord>({
      method: "POST",
      url: "/v1/apm/service-aliases",
      body: input,
    });
    return res.body;
  }

  /// `PUT /v1/apm/service-aliases/{alias_id}`（只能改 service/enabled/note）
  async updateAlias(aliasId: string, input: AliasInput): Promise<AliasRecord> {
    const res = await this.http.request<AliasRecord>({
      method: "PUT",
      url: `/v1/apm/service-aliases/${encodeURIComponent(aliasId)}`,
      body: input,
    });
    return res.body;
  }

  /// `DELETE /v1/apm/service-aliases/{alias_id}`
  async deleteAlias(aliasId: string): Promise<void> {
    await this.http.request<unknown>({
      method: "DELETE",
      url: `/v1/apm/service-aliases/${encodeURIComponent(aliasId)}`,
    });
  }

  /// `GET /v1/ts/stats`
  async storageStats(): Promise<TsStorageStats> {
    const res = await this.http.request<TsStorageStats>({
      method: "GET",
      url: "/v1/ts/stats",
    });
    return res.body;
  }
}
