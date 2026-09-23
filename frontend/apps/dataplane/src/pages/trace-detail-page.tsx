import { Alert, Button, Card, Descriptions, Drawer, Space, Table, Tabs, Tag } from "antd";
import { useCallback, useEffect, useState } from "react";
import { Link, useParams } from "react-router-dom";
import type { SpanDetail, SpanEvent, SpanLink } from "@vectorman/adapters";
import { formatTimestamp } from "@vectorman/primitives";
import { formatDuration } from "../features/apm/layout";
import { useTraceDetail } from "../features/apm/use-apm";
import { Waterfall } from "../ui/waterfall";

const PARTIAL_REASON: Record<string, string> = {
  retention_expired: "span 明细已过保留期（摘要仍在），可查询窗口请参考保留期配置",
  detail_filtered: "部分 span 未写入明细（低于明细耗时阈值或被索引重建窗口影响）",
  index_rebuild: "日志索引正在重建，历史明细暂不可见",
};

/// trace 详情：摘要 + span 瀑布图 + span 明细抽屉。
export function TraceDetailPage() {
  const { traceId = "" } = useParams();
  const { result, run } = useTraceDetail();
  const [selected, setSelected] = useState<SpanDetail | null>(null);

  const load = useCallback(() => {
    if (traceId) {
      void run(traceId);
    }
  }, [run, traceId]);

  useEffect(load, [load]);

  const detail = result.data;

  return (
    <Space direction="vertical" style={{ width: "100%" }} size="middle">
      <Space>
        <Link to="/traces">
          <Button size="small">← 返回列表</Button>
        </Link>
        {detail && (
          <Link
            to={`/logs?service=${encodeURIComponent(detail.summary.root_service)}&from_ts=${
              detail.summary.start_ts
            }&to_ts=${detail.summary.start_ts + Math.max(detail.summary.duration_micros, 1)}`}
          >
            <Button size="small">查看该服务日志</Button>
          </Link>
        )}
        <Button size="small" onClick={load} loading={result.status === "loading"}>
          刷新
        </Button>
      </Space>

      {detail?.partial && (
        <Alert
          type="warning"
          showIcon
          message={`明细不完整（${detail.spans.length} / ${detail.expected_span_count} 个 span）`}
          description={PARTIAL_REASON[detail.reason ?? ""] ?? detail.reason ?? undefined}
        />
      )}

      <Card size="small" title="trace 摘要">
        {detail ? (
          <Descriptions size="small" column={3} bordered>
            <Descriptions.Item label="trace_id" span={2}>
              <span style={{ fontFamily: "monospace" }}>{detail.summary.trace_id}</span>
            </Descriptions.Item>
            <Descriptions.Item label="状态">
              {detail.summary.status === "error" ? (
                <Tag color="red">error</Tag>
              ) : (
                <Tag color="green">ok</Tag>
              )}
            </Descriptions.Item>
            <Descriptions.Item label="开始时间">
              {formatTimestamp(String(detail.summary.start_ts))}
            </Descriptions.Item>
            <Descriptions.Item label="总耗时">
              {formatDuration(detail.summary.duration_micros)}
            </Descriptions.Item>
            <Descriptions.Item label="span / 错误">
              {detail.summary.span_count} / {detail.summary.error_count}
            </Descriptions.Item>
            <Descriptions.Item label="根服务">{detail.summary.root_service}</Descriptions.Item>
            <Descriptions.Item label="根操作">{detail.summary.root_operation}</Descriptions.Item>
            <Descriptions.Item label="来源">{detail.summary.collector}</Descriptions.Item>
            <Descriptions.Item label="服务" span={2}>
              {(detail.summary.services ?? []).join(" → ")}
            </Descriptions.Item>
            <Descriptions.Item label="Agent">{detail.summary.agent_id || "-"}</Descriptions.Item>
            <Descriptions.Item label="主机">{detail.summary.host_id || "-"}</Descriptions.Item>
            <Descriptions.Item label="采集项">{detail.summary.data_id || "-"}</Descriptions.Item>
          </Descriptions>
        ) : (
          <div>加载中…</div>
        )}
      </Card>

      <Card size="small" title="span 瀑布图">
        <Waterfall spans={detail?.spans ?? []} onSelect={setSelected} />
      </Card>

      <Drawer
        width={680}
        open={Boolean(selected)}
        onClose={() => setSelected(null)}
        title={selected ? `${selected.service} · ${selected.name}` : ""}
      >
        {selected && <SpanDetailView span={selected} />}
      </Drawer>
    </Space>
  );
}

function SpanDetailView({ span }: { span: SpanDetail }) {
  return (
    <Tabs
      items={[
        {
          key: "overview",
          label: "概览",
          children: (
            <Descriptions size="small" column={1} bordered>
              <Descriptions.Item label="span_id">
                <span style={{ fontFamily: "monospace" }}>{span.span_id}</span>
              </Descriptions.Item>
              <Descriptions.Item label="parent_span_id">
                <span style={{ fontFamily: "monospace" }}>{span.parent_span_id || "（根）"}</span>
              </Descriptions.Item>
              <Descriptions.Item label="kind / 状态">
                {span.kind} / {span.status_code}
              </Descriptions.Item>
              <Descriptions.Item label="开始 / 结束">
                {formatTimestamp(String(Math.round(span.start_unix_nano / 1000)))} ·{" "}
                {formatDuration(span.duration_micros)}
              </Descriptions.Item>
              {span.status_message ? (
                <Descriptions.Item label="status_message">{span.status_message}</Descriptions.Item>
              ) : null}
              <Descriptions.Item label="丢弃计数">
                attributes {span.dropped_attributes ?? 0} · events {span.dropped_events ?? 0} · links{" "}
                {span.dropped_links ?? 0}
              </Descriptions.Item>
            </Descriptions>
          ),
        },
        {
          key: "attributes",
          label: `属性（${Object.keys(span.attributes ?? {}).length}）`,
          children: <KeyValueTable data={span.attributes ?? {}} />,
        },
        {
          key: "resource",
          label: `资源（${Object.keys(span.resource ?? {}).length}）`,
          children: <KeyValueTable data={span.resource ?? {}} />,
        },
        {
          key: "events",
          label: `事件（${span.events?.length ?? 0}）`,
          children: <EventList events={span.events ?? []} />,
        },
        {
          key: "links",
          label: `链接（${span.links?.length ?? 0}）`,
          children: <LinkList links={span.links ?? []} />,
        },
      ]}
    />
  );
}

function KeyValueTable({ data }: { data: Record<string, string> }) {
  const rows = Object.entries(data).map(([key, value]) => ({ key, value }));
  if (rows.length === 0) {
    return <div>没有记录</div>;
  }
  return (
    <Table
      size="small"
      rowKey="key"
      pagination={false}
      dataSource={rows}
      columns={[
        { title: "键", dataIndex: "key", width: 240 },
        { title: "值", dataIndex: "value", ellipsis: true },
      ]}
    />
  );
}

function EventList({ events }: { events: SpanEvent[] }) {
  if (events.length === 0) {
    return <div>没有记录</div>;
  }
  return (
    <Space direction="vertical" style={{ width: "100%" }}>
      {events.map((event, index) => (
        <Card key={`${event.name}-${index}`} size="small" title={event.name || "(未命名事件)"}>
          <div style={{ marginBottom: 8, color: "rgba(0,0,0,0.45)" }}>
            {formatTimestamp(String(Math.round(event.time_unix_nano / 1000)))}
          </div>
          <KeyValueTable data={event.attributes ?? {}} />
        </Card>
      ))}
    </Space>
  );
}

function LinkList({ links }: { links: SpanLink[] }) {
  if (links.length === 0) {
    return <div>没有记录</div>;
  }
  return (
    <Table
      size="small"
      rowKey={(row) => `${row.trace_id}:${row.span_id}`}
      pagination={false}
      dataSource={links}
      columns={[
        {
          title: "关联 trace",
          dataIndex: "trace_id",
          render: (value: string) => (
            <Link to={`/traces/${value}`} style={{ fontFamily: "monospace" }}>
              {value}
            </Link>
          ),
        },
        { title: "span_id", dataIndex: "span_id", width: 180 },
      ]}
    />
  );
}
