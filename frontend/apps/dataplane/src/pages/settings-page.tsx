import {
  Alert,
  Button,
  Card,
  Descriptions,
  Form,
  Input,
  Modal,
  Popconfirm,
  Select,
  Space,
  Switch,
  Table,
  Tabs,
  Tag,
  message,
} from "antd";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useSearchParams } from "react-router-dom";
import type { AliasInput, AliasRecord } from "@vectorman/adapters";
import { formatTimestamp } from "@vectorman/primitives";
import { useAliases, useSelfMetrics, useStorageStats } from "../features/apm/use-apm";

const MATCH_KINDS = [
  { value: "process_name", label: "process_name（进程名精确）" },
  { value: "process_prefix", label: "process_prefix（进程名前缀）" },
  { value: "pod_prefix", label: "pod_prefix（Pod 名前缀）" },
  { value: "cidr", label: "cidr（对端网段）" },
];

function formatBytes(bytes: number | undefined): string {
  if (bytes === undefined || Number.isNaN(bytes)) {
    return "-";
  }
  if (bytes < 1024) {
    return `${bytes} B`;
  }
  if (bytes < 1024 * 1024) {
    return `${(bytes / 1024).toFixed(1)} KiB`;
  }
  if (bytes < 1024 * 1024 * 1024) {
    return `${(bytes / 1024 / 1024).toFixed(1)} MiB`;
  }
  return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GiB`;
}

/// 设置页：存储与运行状态 + 静态服务名映射。
///
/// `/settings/service-aliases` 直接落到映射页签（拓扑页的「建立映射」入口会带
/// `match_kind` / `match_value` 预填）。
export function SettingsPage({ defaultTab = "storage" }: { defaultTab?: "storage" | "aliases" }) {
  const [params] = useSearchParams();
  const [tab, setTab] = useState(defaultTab);
  useEffect(() => {
    if (defaultTab === "aliases") {
      setTab("aliases");
    }
  }, [defaultTab]);

  return (
    <Card size="small">
      <Tabs
        activeKey={tab}
        onChange={(key) => setTab(key as typeof tab)}
        items={[
          { key: "storage", label: "存储与运行状态", children: <StoragePanel /> },
          {
            key: "aliases",
            label: "服务名映射",
            children: (
              <AliasesPanel
                initialKind={params.get("match_kind") ?? undefined}
                initialValue={params.get("match_value") ?? undefined}
              />
            ),
          },
        ]}
      />
    </Card>
  );
}

function StoragePanel() {
  const stats = useStorageStats();
  const metrics = useSelfMetrics();
  const statsRun = stats.run;
  const metricsRun = metrics.run;

  useEffect(() => {
    void statsRun();
    void metricsRun();
  }, [statsRun, metricsRun]);

  const data = stats.result.data;
  const readings = metrics.result.data ?? {};

  return (
    <Space direction="vertical" style={{ width: "100%" }} size="middle">
      {data?.degraded && (
        <Alert
          type="error"
          showIcon
          message="时序存储处于降级状态"
          description={data.last_background_error ?? "底层后台任务报错，请查看 dataserver 日志"}
        />
      )}

      <Descriptions size="small" column={3} bordered title="时序存储（聚合指标）">
        <Descriptions.Item label="序列数">{readings.ts_series_count ?? data?.series_count ?? "-"}</Descriptions.Item>
        <Descriptions.Item label="保留期">
          {data ? `${data.retention_days} 天` : "-"}
          {data && !data.retention_enforced ? "（未执行）" : ""}
        </Descriptions.Item>
        <Descriptions.Item label="降级">{data?.degraded ? "是" : "否"}</Descriptions.Item>
        <Descriptions.Item label="内存占用">{formatBytes(data?.memory_used_bytes)}</Descriptions.Item>
        <Descriptions.Item label="内存预算">
          {data?.memory_budget_bytes ? formatBytes(data.memory_budget_bytes) : "不限"}
        </Descriptions.Item>
        <Descriptions.Item label="WAL">{formatBytes(data?.wal_size_bytes)}</Descriptions.Item>
        <Descriptions.Item label="过期段数">{data?.expired_segments_total ?? "-"}</Descriptions.Item>
        <Descriptions.Item label="未来时间点">
          {data?.future_skew_points_total ?? "-"}
        </Descriptions.Item>
        <Descriptions.Item label="采样时间">
          {data ? formatTimestamp(String(data.sampled_at_ts)) : "-"}
        </Descriptions.Item>
      </Descriptions>

      <Descriptions size="small" column={3} bordered title="APM 保留与淘汰（自监控读数）">
        <Descriptions.Item label="数据目录占用">
          {formatBytes(readings.apm_data_bytes)}
        </Descriptions.Item>
        <Descriptions.Item label="保留任务执行">{readings.apm_retention_runs ?? "-"} 轮</Descriptions.Item>
        <Descriptions.Item label="已删明细">{readings.apm_details_deleted ?? "-"}</Descriptions.Item>
        <Descriptions.Item label="已删摘要">{readings.apm_summaries_deleted ?? "-"}</Descriptions.Item>
        <Descriptions.Item label="已删边摘要">{readings.apm_edges_deleted ?? "-"}</Descriptions.Item>
        <Descriptions.Item label="接入限流批次">
          {readings.apm_ingest_throttled_batches ?? "-"}
        </Descriptions.Item>
        <Descriptions.Item label="已配对边">{readings.apm_paired_edges ?? "-"}</Descriptions.Item>
        <Descriptions.Item label="待配对 span">{readings.apm_pending_spans ?? "-"}</Descriptions.Item>
        <Descriptions.Item label="TS 降级">{readings.ts_degraded ?? "-"}</Descriptions.Item>
      </Descriptions>

      <div style={{ fontSize: 12, color: "rgba(0,0,0,0.45)" }}>
        容量上限由 dataserver 的 `apm_max_bytes` 控制（0 表示不限）；超限时按最久远优先淘汰 APM
        数据，只删 APM 自身数据。明细/摘要按天过期，端点半默认 30 天。指标为自监控周期采样的读数，
        点「刷新」看最新值。
      </div>

      <Space>
        <Button
          onClick={() => {
            void statsRun();
            void metricsRun();
          }}
          loading={stats.result.status === "loading" || metrics.result.status === "loading"}
        >
          刷新
        </Button>
      </Space>
    </Space>
  );
}

type BulkPreview = {
  rows: { kind: string; value: string; service: string; action: "新增" | "覆盖" }[];
  invalid: number;
};

/// 解析批量导入文本：每行 `match_kind,match_value,service`（支持空格或制表符分隔）。
export function parseBulkImport(
  text: string,
  existing: AliasRecord[],
): BulkPreview {
  const known = new Map(existing.map((row) => [`${row.match_kind}:${row.match_value}`, row]));
  const preview: BulkPreview = { rows: [], invalid: 0 };
  for (const rawLine of text.split("\n")) {
    const line = rawLine.trim();
    if (!line || line.startsWith("#")) {
      continue;
    }
    const parts = line.split(/[,\s\t]+/).filter(Boolean);
    if (parts.length < 3) {
      preview.invalid += 1;
      continue;
    }
    const [kind, value, service] = parts;
    if (!["process_name", "process_prefix", "pod_prefix", "cidr"].includes(kind)) {
      preview.invalid += 1;
      continue;
    }
    preview.rows.push({
      kind,
      value,
      service,
      action: known.has(`${kind}:${value}`) ? "覆盖" : "新增",
    });
  }
  return preview;
}

function AliasesPanel({
  initialKind,
  initialValue,
}: {
  initialKind?: string;
  initialValue?: string;
}) {
  const { result, reload, create, update, remove } = useAliases();
  const [form] = Form.useForm<AliasInput>();
  const [editing, setEditing] = useState<AliasRecord | null>(null);
  const [open, setOpen] = useState(false);
  const [bulkOpen, setBulkOpen] = useState(false);
  const [bulkText, setBulkText] = useState("");
  const [notice, noticeHolder] = message.useMessage();

  useEffect(() => {
    void reload();
  }, [reload]);

  useEffect(() => {
    if (initialKind) {
      setOpen(true);
      form.setFieldsValue({
        match_kind: initialKind,
        match_value: initialValue ? (initialKind === "cidr" && !initialValue.includes("/") ? `${initialValue}/32` : initialValue) : "",
        service: "",
        enabled: true,
      });
    }
  }, [form, initialKind, initialValue]);

  const existing = result.data ?? [];
  const preview = useMemo(() => parseBulkImport(bulkText, existing), [bulkText, existing]);

  const openCreate = () => {
    setEditing(null);
    form.resetFields();
    form.setFieldsValue({ match_kind: "pod_prefix", enabled: true });
    setOpen(true);
  };

  const openEdit = (record: AliasRecord) => {
    setEditing(record);
    form.setFieldsValue({
      match_kind: record.match_kind,
      match_value: record.match_value,
      service: record.service,
      enabled: record.enabled,
      note: record.note,
    });
    setOpen(true);
  };

  const submit = useCallback(async () => {
    const values = await form.validateFields();
    const payload: AliasInput = { ...values, enabled: values.enabled ?? true };
    const ok = editing ? await update(editing.alias_id, payload) : await create(payload);
    if (ok) {
      notice.success(editing ? "已更新映射" : "已新增映射");
      setOpen(false);
    }
  }, [form, editing, create, update, notice]);

  const submitBulk = useCallback(async () => {
    let ok = 0;
    for (const row of preview.rows) {
      const done = await create({ match_kind: row.kind, match_value: row.value, service: row.service, enabled: true });
      if (done) {
        ok += 1;
      }
    }
    notice.success(`已导入 ${ok} 条（跳过 ${preview.invalid} 条非法行）`);
    setBulkOpen(false);
    setBulkText("");
  }, [preview, create, notice]);

  return (
    <Space direction="vertical" style={{ width: "100%" }} size="middle">
      {noticeHolder}
      <Space>
        <Button type="primary" onClick={openCreate}>
          新建映射
        </Button>
        <Button onClick={() => setBulkOpen(true)}>批量导入</Button>
        <Button onClick={() => void reload()} loading={result.status === "loading"}>
          刷新
        </Button>
      </Space>

      <div style={{ fontSize: 12, color: "rgba(0,0,0,0.45)" }}>
        反查优先级：process_name → process_prefix → pod_prefix → cidr → 端点表 → `unknown-&lt;ip&gt;`。
        映射只用于 eBPF 边与未识别目标的归一，不会改写 OTLP span 自带的服务名。
      </div>

      <Table<AliasRecord>
        size="small"
        rowKey="alias_id"
        loading={result.status === "loading"}
        dataSource={existing}
        pagination={false}
        locale={{ emptyText: "还没有映射（无插桩应用会以 unknown-<ip> 出现在拓扑里）" }}
        columns={[
          { title: "匹配方式", dataIndex: "match_kind", width: 150 },
          { title: "匹配值", dataIndex: "match_value", width: 240 },
          { title: "服务名", dataIndex: "service", width: 180 },
          {
            title: "启用",
            dataIndex: "enabled",
            width: 90,
            render: (enabled: boolean, row) => (
              <Switch
                size="small"
                checked={enabled}
                onChange={(checked) =>
                  void update(row.alias_id, {
                    match_kind: row.match_kind,
                    match_value: row.match_value,
                    service: row.service,
                    enabled: checked,
                    note: row.note,
                  })
                }
              />
            ),
          },
          { title: "备注", dataIndex: "note", ellipsis: true },
          {
            title: "更新时间",
            dataIndex: "updated_ts",
            width: 170,
            render: (value: number) => formatTimestamp(String(value)),
          },
          {
            title: "操作",
            width: 140,
            render: (_, row) => (
              <Space>
                <Button size="small" type="link" onClick={() => openEdit(row)}>
                  编辑
                </Button>
                <Popconfirm
                  title="删除该映射？"
                  description="删除后该匹配条件将回落到端点表/未识别。"
                  onConfirm={() => void remove(row.alias_id)}
                >
                  <Button size="small" type="link" danger>
                    删除
                  </Button>
                </Popconfirm>
              </Space>
            ),
          },
        ]}
      />

      <Modal
        open={open}
        title={editing ? "编辑映射" : "新建映射"}
        onCancel={() => setOpen(false)}
        onOk={() => void submit()}
        okText="保存"
        // 编辑时匹配条件不可改（服务端以 `kind:value` 作主键），改条件请删除后重建。
        cancelButtonProps={{ style: { display: "none" } }}
      >
        <Form form={form} layout="vertical">
          <Form.Item name="match_kind" label="匹配方式" rules={[{ required: true }]}>
            <Select options={MATCH_KINDS} disabled={Boolean(editing)} />
          </Form.Item>
          <Form.Item
            name="match_value"
            label="匹配值"
            rules={[{ required: true, message: "匹配值不能为空" }]}
          >
            <Input placeholder="例如 order-api 或 10.0.0.0/8" disabled={Boolean(editing)} />
          </Form.Item>
          <Form.Item name="service" label="服务名" rules={[{ required: true, message: "服务名不能为空" }]}>
            <Input placeholder="例如 order-api" />
          </Form.Item>
          <Form.Item name="enabled" label="启用" valuePropName="checked">
            <Switch />
          </Form.Item>
          <Form.Item name="note" label="备注">
            <Input.TextArea rows={2} placeholder="可选，例如：订单服务无插桩" />
          </Form.Item>
        </Form>
      </Modal>

      <Modal
        open={bulkOpen}
        title="批量导入映射"
        width={720}
        onCancel={() => setBulkOpen(false)}
        onOk={() => void submitBulk()}
        okText={`导入 ${preview.rows.length} 条`}
        okButtonProps={{ disabled: preview.rows.length === 0 }}
      >
        <Space direction="vertical" style={{ width: "100%" }}>
          <div style={{ fontSize: 12, color: "rgba(0,0,0,0.45)" }}>
            每行一条：`match_kind, match_value, service`（逗号、空格或制表符分隔；`#` 开头为注释）。
            已存在的匹配条件会被覆盖。
          </div>
          <Input.TextArea
            rows={8}
            value={bulkText}
            onChange={(e) => setBulkText(e.target.value)}
            placeholder={"pod_prefix,order-api,order-api\ncidr,10.0.0.0/8,legacy-app"}
          />
          <Space size="large">
            <span>将新增 {preview.rows.filter((r) => r.action === "新增").length} 条</span>
            <span>将覆盖 {preview.rows.filter((r) => r.action === "覆盖").length} 条</span>
            {preview.invalid > 0 && <Tag color="orange">非法行 {preview.invalid}</Tag>}
          </Space>
          {preview.rows.length > 0 && (
            <Table
              size="small"
              rowKey={(row) => `${row.kind}:${row.value}`}
              pagination={false}
              dataSource={preview.rows}
              columns={[
                { title: "动作", dataIndex: "action", width: 80 },
                { title: "匹配方式", dataIndex: "kind", width: 150 },
                { title: "匹配值", dataIndex: "value" },
                { title: "服务名", dataIndex: "service" },
              ]}
            />
          )}
        </Space>
      </Modal>
    </Space>
  );
}
