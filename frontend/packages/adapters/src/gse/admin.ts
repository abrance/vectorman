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
};

export type AgentConfig = {
  agent_id: string;
  host_id: string;
  cpu_limit_percent?: number | null;
  mem_limit_percent?: number | null;
  log_level?: string;
  updated_at?: string;
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

  listAgentConfigs(): Promise<AgentConfig[]> {
    return this.http.request<AgentConfig[]>({ method: "GET", url: `${PREFIX}/agent-configs` }).then((r) => r.body);
  }

  upsertAgentConfig(cfg: AgentConfig): Promise<AgentConfig> {
    return this.http
      .request<AgentConfig>({ method: "POST", url: `${PREFIX}/agent-configs`, body: cfg })
      .then((r) => r.body);
  }

  getAgentConfig(agentId: string): Promise<AgentConfig> {
    return this.http
      .request<AgentConfig>({ method: "GET", url: `${PREFIX}/agent-configs/${enc(agentId)}` })
      .then((r) => r.body);
  }
}
