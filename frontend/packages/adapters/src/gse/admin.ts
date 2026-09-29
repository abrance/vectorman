import type { HttpClient } from "@vectorman/primitives";

export type Host = {
  host_id: string;
  inner_ip: string;
  hostname?: string;
  os_type?: string;
  os_version?: string;
  cpu_spec?: string;
  mem_spec?: string;
  created_at?: string;
};

export type AccessPoint = {
  id: string;
  name: string;
  server_ip: string;
  rpc_port: number;
  file_port?: number | null;
  data_port?: number | null;
  created_at?: string;
};

export type Agent = {
  agent_id: string;
  host_id: string;
  access_point_id?: string | null;
  token: string;
  version?: string;
  install_path?: string;
  status?: string;
  last_heartbeat_at?: string | null;
  registered_at?: string;
  /// 会话口径（由 gse-server 的内存会话注册表实时投影，非台账字段）。
  /// `status` 是心跳口径，两者可以不一致 —— 「心跳在线但作业通道已死」
  /// 就是 `status = online` 而 `session_state = absent`。
  session_state?: "online" | "checking" | "offline" | "closed" | "absent";
  job_channel_available?: boolean;
};

/// Agent 运行参数（spec 的一半）。敏感字段读出是 `"***"`，写回该值表示「保持不变」。
export type SpecParams = {
  heartbeat_interval_secs: number;
  allowed_interpreters: string[];
  job_default_interpreter: string;
  max_concurrent_jobs: number;
  job_work_dir?: string | null;
  otlp_enabled: boolean;
  otlp_listen: string;
  otlp_max_body_bytes: number;
  otlp_token?: string | null;
  otlp_allowed_cidrs: string[];
  token?: string | null;
  cpu_limit_percent?: number | null;
  mem_limit_percent?: number | null;
  log_level: string;
};

/// 一台 Agent 的完整期望状态：`params` + 这台的采集项数组。
export type AgentSpecWire = {
  params: SpecParams;
  items: SpecItem[];
};

/// 采集项（收口到 spec 之后不再带 `agent_ids`）。
export type SpecItem = {
  item_id: string;
  name: string;
  kind: string;
  enabled: boolean;
  collector: Record<string, unknown>;
  /// 至少含 `retention_days`（dataserver 的保留清理按它算清理窗口）。
  storage: { retention_days?: number } & Record<string, unknown>;
};

export type FieldPair = { desired: unknown; applied: unknown };
export type ItemDiff = { added: string[]; removed: string[]; changed: string[] };
export type SpecDiff = { params: Record<string, FieldPair>; items: ItemDiff };

/// 期望值与生效值是否一致。
///
/// `unspecified` = 没有期望 spec（Agent 跑本地文件基线）；`unknown` = 从未上报。
export type SyncStatus = "synced" | "stale" | "rejected" | "unspecified" | "unknown";

export type AgentSpecView = {
  agent_id: string;
  host_id: string;
  /// 会话口径（内存会话注册表），与台账 `status` 可能不一致。
  session_state: string;
  sync_status: SyncStatus;
  updated_at?: string | null;
  reported_at?: string | null;
  desired?: { revision: string; spec: AgentSpecWire } | null;
  applied?: {
    revision: string;
    outcome: string;
    spec: AgentSpecWire;
    not_enforced: string[];
    detail: string;
  } | null;
  diff?: SpecDiff | null;
};

/// PUT spec 的请求体。非敏感字段整体覆盖，`token`/`otlp_token` 缺省或空串表示保持原值。
export type AgentSpecPutBody = {
  params?: Partial<SpecParams>;
  items?: (Partial<SpecItem> & { item_id?: string })[];
};

export type AgentSpecAck = {
  revision: string;
  outcome: string;
  applied: AgentSpecWire;
  not_enforced: string[];
  detail: string;
};

const PREFIX = "/api/gse";

function enc(id: string): string {
  return encodeURIComponent(id);
}

export class GseAdminAdapter {
  constructor(private readonly http: HttpClient) {}

  listHosts(): Promise<Host[]> {
    return this.http.request<Host[]>({ method: "GET", url: `${PREFIX}/hosts` }).then((r) => r.body);
  }

  upsertHost(host: Host): Promise<Host> {
    return this.http.request<Host>({ method: "POST", url: `${PREFIX}/hosts`, body: host }).then((r) => r.body);
  }

  getHost(hostId: string): Promise<Host> {
    return this.http.request<Host>({ method: "GET", url: `${PREFIX}/hosts/${enc(hostId)}` }).then((r) => r.body);
  }

  deleteHost(hostId: string): Promise<void> {
    return this.http.request({ method: "DELETE", url: `${PREFIX}/hosts/${enc(hostId)}` }).then(() => undefined);
  }

  listAccessPoints(): Promise<AccessPoint[]> {
    return this.http.request<AccessPoint[]>({ method: "GET", url: `${PREFIX}/access-points` }).then((r) => r.body);
  }

  upsertAccessPoint(ap: AccessPoint): Promise<AccessPoint> {
    return this.http
      .request<AccessPoint>({ method: "POST", url: `${PREFIX}/access-points`, body: ap })
      .then((r) => r.body);
  }

  getAccessPoint(id: string): Promise<AccessPoint> {
    return this.http
      .request<AccessPoint>({ method: "GET", url: `${PREFIX}/access-points/${enc(id)}` })
      .then((r) => r.body);
  }

  deleteAccessPoint(id: string): Promise<void> {
    return this.http.request({ method: "DELETE", url: `${PREFIX}/access-points/${enc(id)}` }).then(() => undefined);
  }

  listAgents(): Promise<Agent[]> {
    return this.http.request<Agent[]>({ method: "GET", url: `${PREFIX}/agents` }).then((r) => r.body);
  }

  upsertAgent(agent: Agent): Promise<Agent> {
    return this.http.request<Agent>({ method: "POST", url: `${PREFIX}/agents`, body: agent }).then((r) => r.body);
  }

  getAgent(agentId: string): Promise<Agent> {
    return this.http.request<Agent>({ method: "GET", url: `${PREFIX}/agents/${enc(agentId)}` }).then((r) => r.body);
  }

  deleteAgent(agentId: string): Promise<void> {
    return this.http.request({ method: "DELETE", url: `${PREFIX}/agents/${enc(agentId)}` }).then(() => undefined);
  }

  listAgentSpecs(): Promise<AgentSpecView[]> {
    return this.http
      .request<AgentSpecView[]>({ method: "GET", url: `${PREFIX}/agent-specs` })
      .then((r) => r.body);
  }

  getAgentSpec(agentId: string): Promise<AgentSpecView> {
    return this.http
      .request<AgentSpecView>({ method: "GET", url: `${PREFIX}/agents/${enc(agentId)}/spec` })
      .then((r) => r.body);
  }

  /// 只写期望，**不触发下发**（下发是独立的 apply）。
  putAgentSpec(agentId: string, body: AgentSpecPutBody): Promise<AgentSpecView> {
    return this.http
      .request<AgentSpecView>({
        method: "PUT",
        url: `${PREFIX}/agents/${enc(agentId)}/spec`,
        body,
      })
      .then((r) => r.body);
  }

  /// 下发该 Agent 的期望 spec（单台，不批量）。
  applyAgentSpec(agentId: string): Promise<{ ok: boolean; ack: AgentSpecAck }> {
    return this.http
      .request<{ ok: boolean; ack: AgentSpecAck }>({
        method: "POST",
        url: `${PREFIX}/agents/${enc(agentId)}/spec/apply`,
      })
      .then((r) => r.body);
  }
}
