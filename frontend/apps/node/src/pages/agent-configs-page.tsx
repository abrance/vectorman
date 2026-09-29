import { Alert, Button, Modal, Space, Table, Tag, Typography } from "antd";
import { useEffect } from "react";
import { useNavigate } from "react-router-dom";
import {
  COLLECT_KINDS,
  type AgentSpecView,
} from "@vectorman/adapters";
import { formatTimestamp } from "@vectorman/primitives";
import { useRuntime } from "../app/runtime";
import { toAppError } from "../features/ledger/errors";
import { syncStatusMeta } from "../features/ledger/spec-diff";
import { useAgentSpecs } from "../features/ledger/use-agent-specs";

const kindLabel = (kind: string) => COLLECT_KINDS.find((k) => k.value === kind)?.label ?? kind;

/// 「Agent 配置」列表：一台 Agent 一行，一眼看是否同步。
export function AgentConfigsPage() {
  const { notifier } = useRuntime();
  const { list, refresh, apply } = useAgentSpecs();
  const navigate = useNavigate();

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const confirmApply = (row: AgentSpecView) => {
    Modal.confirm({
      title: `下发 ${row.agent_id} 的配置？`,
      content:
        row.session_state === "online"
          ? "会把该 Agent 的期望配置（运行参数 + 采集项）整份推送，并取回生效回执。"
          : "该 Agent 当前没有在线会话，下发会被拒绝（配置仍可先保存）。",
      okText: "下发",
      cancelText: "取消",
      onOk: async () => {
        try {
          await apply(row.agent_id);
        } catch (e) {
          notifier.error(toAppError(e));
          throw e;
        }
      },
    });
  };

  return (
    <Space direction="vertical" style={{ width: "100%" }} size="middle">
      <Space>
        <Button type="primary" onClick={() => void refresh()}>
          刷新
        </Button>
        <Typography.Text type="secondary">
          一行一台 Agent：期望配置（参数 + 采集项）与生效值不一致时标「未同步」，点「下发」才推送。
        </Typography.Text>
      </Space>
      <Table
        rowKey="agent_id"
        loading={list.status === "loading"}
        dataSource={list.data ?? []}
        locale={{ emptyText: list.error?.message ?? "暂无数据" }}
        columns={[
          { title: "agent_id", dataIndex: "agent_id" },
          { title: "host_id", dataIndex: "host_id" },
          {
            title: "会话",
            dataIndex: "session_state",
            render: (state: string) =>
              state === "online" ? <Tag color="green">online</Tag> : <Tag>{state || "—"}</Tag>,
          },
          {
            title: "同步状态",
            dataIndex: "sync_status",
            render: (status: AgentSpecView["sync_status"]) => {
              const meta = syncStatusMeta(status);
              return <Tag color={meta.color}>{meta.label}</Tag>;
            },
          },
          {
            title: "revision",
            dataIndex: ["desired", "revision"],
            render: (rev: string | undefined) => (
              <Typography.Text code>{rev ? rev.slice(0, 8) : "—"}</Typography.Text>
            ),
          },
          {
            title: "更新时间",
            dataIndex: "updated_at",
            render: (v: string | null) => formatTimestamp(v),
          },
          {
            title: "上报时间",
            dataIndex: "reported_at",
            render: (v: string | null) => formatTimestamp(v),
          },
          {
            title: "采集项",
            dataIndex: ["desired", "spec", "items"],
            render: (items: unknown[] | undefined) =>
              items?.length ? (
                <Space size={4} wrap>
                  {items.slice(0, 3).map((raw) => {
                    const item = raw as { item_id: string; kind: string };
                    return (
                      <Tag key={item.item_id}>{kindLabel(item.kind)}</Tag>
                    );
                  })}
                  {(items?.length ?? 0) > 3 ? <Tag>+{(items?.length ?? 0) - 3}</Tag> : null}
                </Space>
              ) : (
                <Typography.Text type="secondary">无</Typography.Text>
              ),
          },
          {
            title: "操作",
            render: (_, row) => (
              <Space>
                <Button type="link" onClick={() => navigate(`/agent-configs/${encodeURIComponent(row.agent_id)}`)}>
                  查看 / 编辑
                </Button>
                <Button type="link" disabled={!row.desired} onClick={() => confirmApply(row)}>
                  下发
                </Button>
              </Space>
            ),
          },
        ]}
        expandable={{
          expandedRowRender: (row) => {
            const meta = syncStatusMeta(row.sync_status);
            const enforced = row.applied?.not_enforced ?? [];
            return (
              <Space direction="vertical" style={{ width: "100%" }} size={4}>
                <Alert type="info" showIcon message={meta.hint} />
                {enforced.length > 0 ? (
                  <Alert
                    type="warning"
                    showIcon
                    message={`未实现字段（仅记录，不下发生效）：${enforced.join("、")}`}
                  />
                ) : null}
              </Space>
            );
          },
        }}
      />
    </Space>
  );
}
