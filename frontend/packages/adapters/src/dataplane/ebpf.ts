import type { HttpClient } from "@vectorman/primitives";
import type { LogRecord } from "./ingest";

/// `POST /v1/ebpf/events/search` 的过滤条件。
///
/// 与日志检索同形，但 `data_type` **不需要也不能**传：服务端固定为 `ebpf`。
export type EbpfEventSearchRequest = {
  event_type?: string;
  /// 标签等值过滤，例如 `{ process_name: "java" }`。
  labels?: Record<string, string>;
  message_query?: string;
  from_ts?: number;
  to_ts?: number;
  limit?: number;
};

/// 单个 Agent 的 eBPF 能力状态。
export type CapabilityEntry = {
  agent_id: string;
  item_id: string;
  available: boolean;
  kernel_ok: boolean;
  btf_ok: boolean;
  capability_ok: boolean;
  kernel_release: string;
  reason: string;
};

export type CapabilityReport = {
  agents: CapabilityEntry[];
  /// 有多少 Agent 上报过能力状态：0 表示「没人上报」，而不是「都不可用」。
  reported: number;
};

type EventsReply = { records: LogRecord[] };

/// eBPF 原始事件与能力状态。
export class EbpfAdapter {
  constructor(private readonly http: HttpClient) {}

  /// `POST /v1/ebpf/events/search`
  async searchEvents(req: EbpfEventSearchRequest): Promise<LogRecord[]> {
    const res = await this.http.request<EventsReply>({
      method: "POST",
      url: "/v1/ebpf/events/search",
      body: req,
    });
    return res.body.records ?? [];
  }

  /// `GET /v1/ebpf/capability`
  async capability(): Promise<CapabilityReport> {
    const res = await this.http.request<CapabilityReport>({
      method: "GET",
      url: "/v1/ebpf/capability",
    });
    return { agents: res.body.agents ?? [], reported: res.body.reported ?? 0 };
  }
}
