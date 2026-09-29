import { Alert, Button, Space, Table, Tag, Tooltip, Typography } from "antd";
import { useEffect } from "react";
import { useNavigate } from "react-router-dom";
import { COLLECT_KINDS, type AgentSpecView } from "@vectorman/adapters";
import { formatTimestamp } from "@vectorman/primitives";
import {
  summarizeStreams,
  useSpecCatalog,
  type AgentItem,
} from "../features/use-collect";

const kindLabel = (kind: string) => COLLECT_KINDS.find((k) => k.value === kind)?.label ?? kind;

/// 配置编辑在 console（直连 gse-server）。两个前端由不同进程托管，所以用可配置外链；
/// 没配就只提示位置，不假装能跳过去。
const CONSOLE_URL = (import.meta.env.VITE_CONSOLE_URL as string | undefined)?.replace(/\/$/, "") ?? "";

function reportingState(lastSeenMicros: number | undefined, intervalSecs: number) {
  if (!lastSeenMicros) {
    return { label: "未上报", color: "red" as const };
  }
  const staleAfterMicros = Math.max(3 * intervalSecs, 60) * 1_000_000;
  if (Date.now() * 1000 - lastSeenMicros > staleAfterMicros) {
    return { label: "已停滞", color: "orange" as const };
  }
  return { label: "上报中", color: "green" as const };
}

function syncTag(status: AgentSpecView["sync_status"]) {
  switch (status) {
    case "synced":
      return <Tag color="green">已同步</Tag>;
    case "stale":
      return <Tag color="orange">未下发</Tag>;
    case "rejected":
      return <Tag color="red">被拒绝</Tag>;
    case "unspecified":
      return <Tag color="blue">无期望</Tag>;
    default:
      return <Tag>未知</Tag>;
  }
}

function editHint(agentId: string) {
  if (!CONSOLE_URL) {
    return (
      <Typography.Text type="secondary">在 console「Agent 配置」页编辑</Typography.Text>
    );
  }
  return (
    <a href={`${CONSOLE_URL}/agent-configs/${encodeURIComponent(agentId)}`} target="_blank" rel="noreferrer">
      去配置
    </a>
  );
}

/// 采集链路：跨 Agent 的**只读总览**。
///
/// 采集项已经是各台 Agent 的 spec 的一部分，这里只回答「谁在采什么、报上来没有」；
/// 编辑入口统一在 console 的 Agent 配置页（避免两个写入口）。
export function CollectPage() {
  const navigate = useNavigate();
  const { specs, agents, streams, rows, usage, refresh, loadAgents, loadStreams } =
    useSpecCatalog();

  useEffect(() => {
    void refresh();
    void loadAgents();
    void loadStreams();
  }, [refresh, loadAgents, loadStreams]);

  const summary = summarizeStreams(streams.data ?? []);
  const views: AgentSpecView[] = specs.data ?? [];
  const syncByAgent = new Map(views.map((v) => [v.agent_id, v.sync_status]));
  const agentOnline = new Map((agents.data ?? []).map((a) => [a.agent_id, a.status]));

  const dataSource = rows.map((row: AgentItem) => {
    const summaryRow = summary.get(row.item.item_id);
    const interval = Number(
      (row.item.collector as { interval_secs?: number })?.interval_secs ?? 15,
    );
    return {
      key: `${row.agentId}/${row.item.item_id}`,
      ...row,
      lastSeenMicros: summaryRow?.lastSeenMicros,
      accepted: summaryRow?.accepted ?? 0,
      reporting: reportingState(summaryRow?.lastSeenMicros, interval),
    };
  });

  return (
    <Space direction="vertical" style={{ width: "100%" }} size="middle">
      <Space>
        <Button type="primary" onClick={() => void refresh()}>
          刷新
        </Button>
        <Button onClick={() => void loadStreams()}>刷新上报状态</Button>
        <Typography.Text type="secondary">
          只读总览：采集项属于各台 Agent 的 spec，配置编辑在 console「Agent 配置」页。
        </Typography.Text>
      </Space>
      {specs.error ? <Alert type="error" showIcon message={specs.error.message} /> : null}
      <Table
        rowKey="key"
        loading={specs.status === "loading"}
        dataSource={dataSource}
        pagination={false}
        locale={{ emptyText: "没有任何 Agent 配置采集项" }}
        columns={[
          { title: "agent_id", dataIndex: "agentId" },
          {
            title: "会话",
            render: (_, row) => {
              const status = agentOnline.get(row.agentId);
              return status === "online" ? <Tag color="green">online</Tag> : <Tag>{status ?? "—"}</Tag>;
            },
          },
          {
            title: "同步",
            render: (_, row) => syncTag(syncByAgent.get(row.agentId) ?? "unknown"),
          },
          {
            title: "item_id",
            render: (_, row) => (
              <Tooltip title={`作用于 ${usage.get(row.item.item_id)?.length ?? 1} 台 Agent`}>
                <Typography.Text code>{row.item.item_id}</Typography.Text>
              </Tooltip>
            ),
          },
          { title: "名称", render: (_, row) => row.item.name },
          {
            title: "类型",
            render: (_, row) => <Tag>{kindLabel(row.item.kind)}</Tag>,
          },
          {
            title: "启用",
            render: (_, row) =>
              row.item.enabled ? <Tag color="green">启用</Tag> : <Tag>停用</Tag>,
          },
          {
            title: "最近上报",
            // 流索引里是微秒；`formatTimestamp` 认字符串微秒。
            render: (_, row) => (
              <Typography.Text>
                {row.lastSeenMicros
                  ? formatTimestamp(String(Math.floor(row.lastSeenMicros / 1000) * 1000))
                  : "—"}
              </Typography.Text>
            ),
          },
          { title: "累计条数", dataIndex: "accepted" },
          {
            title: "上报状态",
            render: (_, row) => <Tag color={row.reporting.color}>{row.reporting.label}</Tag>,
          },
          {
            title: "操作",
            render: (_, row) => (
              <Space>
                <Button
                  type="link"
                  onClick={() =>
                    navigate(
                      `/metrics?agent_id=${encodeURIComponent(row.agentId)}&data_id=${encodeURIComponent(row.item.item_id)}`,
                    )
                  }
                >
                  数据检索
                </Button>
                {editHint(row.agentId)}
              </Space>
            ),
          },
        ]}
      />
    </Space>
  );
}
