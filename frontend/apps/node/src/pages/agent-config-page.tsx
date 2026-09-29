import {
  Alert,
  Button,
  Card,
  Descriptions,
  Form,
  Input,
  InputNumber,
  Modal,
  Select,
  Space,
  Switch,
  Table,
  Tabs,
  Tag,
  Typography,
} from "antd";
import { useCallback, useEffect, useState } from "react";
import { useNavigate, useParams } from "react-router-dom";
import {
  COLLECT_KINDS,
  isEbpfKind,
  toCollectFormValues,
  toCollectItemInput,
  type AgentSpecPutBody,
  type AgentSpecView,
  type CollectFormValues,
  type SpecItem,
  type SpecParams,
} from "@vectorman/adapters";
import { formatTimestamp } from "@vectorman/primitives";
import { useRuntime } from "../app/runtime";
import { toAppError } from "../features/ledger/errors";
import {
  MASK,
  defaultParams,
  itemChanges,
  notEnforcedLabel,
  paramDiffRows,
  syncStatusMeta,
} from "../features/ledger/spec-diff";
import { useAgentSpecs } from "../features/ledger/use-agent-specs";

const kindLabel = (kind: string) => COLLECT_KINDS.find((k) => k.value === kind)?.label ?? kind;

const ITEM_DEFAULTS: CollectFormValues = {
  name: "",
  kind: "metrics_host",
  enabled: true,
  retention_days: 1,
  interval_secs: 15,
  path_patterns: [],
  namespace: "",
  pod_name_pattern: "",
  container: "",
  kubeconfig: "",
  start_mode: "tail",
  start_n: 0,
  batch_max_records: 100,
  flush_interval_secs: 5,
  include_regex: "",
  exclude_regex: "",
  extract: [],
  include_loopback: false,
  raw_events_enabled: false,
  slow_threshold_micros: 100_000,
};

/// 一台 Agent 的配置整页：参数表单 / 原始 JSON / 逐字段差异 / 采集项。
///
/// 「保存」只写期望，「下发」才推送 —— 两个按钮分开是刻意的：改完多个字段再一次下发，
/// 避免每改一个字段就把 Agent 重启一遍采集器。
export function AgentConfigPage() {
  const { agentId = "" } = useParams<{ agentId: string }>();
  const navigate = useNavigate();
  const { notifier } = useRuntime();
  const { getOne, put, apply } = useAgentSpecs();
  const [form] = Form.useForm<SpecParams>();
  const [itemForm] = Form.useForm<CollectFormValues>();

  const [view, setView] = useState<AgentSpecView | null>(null);
  const [items, setItems] = useState<SpecItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [applying, setApplying] = useState(false);
  const [jsonText, setJsonText] = useState("");
  const [tab, setTab] = useState("form");
  const [itemOpen, setItemOpen] = useState(false);
  const [editingItemId, setEditingItemId] = useState<string | null>(null);
  const itemKind = (Form.useWatch("kind", itemForm) ?? "metrics_host") as string;

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const v = await getOne(agentId);
      setView(v);
      // 没有期望 spec 时用 Agent 上报的本地生效值预填：运维通常是在现状上改。
      const params = v.desired?.spec.params ?? v.applied?.spec.params ?? defaultParams();
      form.setFieldsValue(params);
      setItems(v.desired?.spec.items ?? v.applied?.spec.items ?? []);
    } catch (e) {
      notifier.error(toAppError(e));
    } finally {
      setLoading(false);
    }
  }, [agentId, getOne, form, notifier]);

  useEffect(() => {
    void load();
  }, [load]);

  const save = async (body: AgentSpecPutBody) => {
    setSaving(true);
    try {
      const saved = await put(agentId, body);
      setView(saved);
      setItems(saved.desired?.spec.items ?? []);
    } catch (e) {
      notifier.error(toAppError(e));
    } finally {
      setSaving(false);
    }
  };

  const saveForm = () =>
    void save({ params: form.getFieldsValue() as SpecParams, items });

  const confirmApply = () => {
    Modal.confirm({
      title: `下发 ${agentId} 的配置？`,
      content:
        view?.session_state === "online"
          ? "把整份期望配置推给该 Agent 并取回生效回执。采集项整表也会一起下发（含删除与停用）。"
          : "该 Agent 当前没有在线会话，下发会被拒绝。",
      okText: "下发",
      cancelText: "取消",
      onOk: async () => {
        setApplying(true);
        try {
          await apply(agentId);
          await load();
        } catch (e) {
          notifier.error(toAppError(e));
          throw e;
        } finally {
          setApplying(false);
        }
      },
    });
  };

  const openJson = () => {
    setJsonText(JSON.stringify({ params: form.getFieldsValue(), items }, null, 2));
    setTab("json");
  };

  const saveJson = () => {
    let parsed: AgentSpecPutBody;
    try {
      parsed = JSON.parse(jsonText) as AgentSpecPutBody;
    } catch (e) {
      notifier.error(toAppError(e));
      return;
    }
    if (typeof parsed !== "object" || parsed === null || !("params" in parsed)) {
      notifier.warning("JSON 必须是含 params 与 items 的对象");
      return;
    }
    if (parsed.params) {
      form.setFieldsValue(parsed.params as SpecParams);
    }
    if (Array.isArray(parsed.items)) {
      setItems(parsed.items as SpecItem[]);
    }
    void save(parsed);
  };

  const openItem = (item?: SpecItem) => {
    if (item) {
      setEditingItemId(item.item_id);
      itemForm.setFieldsValue(toCollectFormValues(item));
    } else {
      setEditingItemId(null);
      itemForm.setFieldsValue({ ...ITEM_DEFAULTS });
    }
    setItemOpen(true);
  };

  const submitItem = () => {
    const values = itemForm.getFieldsValue();
    if (!values.name?.trim()) {
      notifier.warning("采集项名称必填");
      return;
    }
    const built = toCollectItemInput(values) as SpecItem;
    const next = editingItemId
      ? items.map((i) => (i.item_id === editingItemId ? { ...built, item_id: editingItemId } : i))
      : [...items, { ...built, item_id: "" }];
    setItems(next);
    setItemOpen(false);
  };

  const status = view?.sync_status ?? "unknown";
  const meta = syncStatusMeta(status);
  const notEnforced = view?.applied?.not_enforced ?? [];

  return (
    <Space direction="vertical" style={{ width: "100%" }} size="middle">
      <Space>
        <Button onClick={() => navigate("/agent-configs")}>返回列表</Button>
        <Typography.Title level={4} style={{ margin: 0 }}>
          {agentId}
        </Typography.Title>
        <Tag color={meta.color}>{meta.label}</Tag>
        {view?.session_state ? <Tag>会话 {view.session_state}</Tag> : null}
      </Space>

      <Descriptions size="small" column={3} bordered>
        <Descriptions.Item label="期望 revision">
          <Typography.Text code>
            {view?.desired?.revision ? view.desired.revision.slice(0, 8) : "—"}
          </Typography.Text>
        </Descriptions.Item>
        <Descriptions.Item label="生效 revision">
          <Typography.Text code>
            {view?.applied?.revision ? view.applied.revision.slice(0, 8) : "（本地基线）"}
          </Typography.Text>
        </Descriptions.Item>
        <Descriptions.Item label="生效结果">{view?.applied?.outcome ?? "—"}</Descriptions.Item>
        <Descriptions.Item label="期望更新于">{formatTimestamp(view?.updated_at)}</Descriptions.Item>
        <Descriptions.Item label="生效上报于">{formatTimestamp(view?.reported_at)}</Descriptions.Item>
        <Descriptions.Item label="host_id">{view?.host_id || "—"}</Descriptions.Item>
      </Descriptions>

      <Alert type="info" showIcon message={meta.hint} />
      {notEnforced.length > 0 ? (
        <Alert
          type="warning"
          showIcon
          message="以下字段已下发并记录，但 Agent 侧未实现（不会真的生效）"
          description={
            <Space wrap>
              {notEnforced.map((f) => (
                <Tag key={f}>{notEnforcedLabel(f)}</Tag>
              ))}
            </Space>
          }
        />
      ) : null}

      <Space>
        <Button type="primary" loading={saving} onClick={saveForm}>
          保存期望配置
        </Button>
        <Button loading={applying} disabled={!view?.desired} onClick={confirmApply}>
          下发到该 Agent
        </Button>
        <Button onClick={() => void load()}>刷新</Button>
        <Typography.Text type="secondary">
          保存只写台账；点「下发」才推送给 Agent（下发失败不影响已保存的期望值）。
        </Typography.Text>
      </Space>

      <Tabs
        activeKey={tab}
        onChange={setTab}
        items={[
          {
            key: "form",
            label: "参数",
            children: (
              <Form form={form} layout="vertical" disabled={loading}>
                <Card size="small" title="心跳" style={{ marginBottom: 12 }}>
                  <Form.Item
                    name="heartbeat_interval_secs"
                    label="心跳周期（秒）"
                    extra="受服务端判活窗口限制：不得超过 heartbeat_timeout_secs / 3（缺省 30 秒），否则会被拒绝"
                  >
                    <InputNumber min={1} style={{ width: 200 }} />
                  </Form.Item>
                </Card>
                <Card size="small" title="作业执行" style={{ marginBottom: 12 }}>
                  <Form.Item name="allowed_interpreters" label="解释器白名单">
                    <Select mode="tags" style={{ width: "100%" }} placeholder="bash / sh / python3" />
                  </Form.Item>
                  <Form.Item name="job_default_interpreter" label="默认解释器">
                    <Input style={{ width: 200 }} />
                  </Form.Item>
                  <Form.Item name="max_concurrent_jobs" label="并发作业上限">
                    <InputNumber min={1} style={{ width: 200 }} />
                  </Form.Item>
                  <Form.Item name="job_work_dir" label="作业临时目录" extra="留空用系统临时目录">
                    <Input style={{ width: 320 }} />
                  </Form.Item>
                </Card>
                <Card size="small" title="OTLP trace 接收器" style={{ marginBottom: 12 }}>
                  <Form.Item name="otlp_enabled" label="启用接收器" valuePropName="checked">
                    <Switch />
                  </Form.Item>
                  <Form.Item name="otlp_listen" label="监听地址">
                    <Input style={{ width: 240 }} />
                  </Form.Item>
                  <Form.Item name="otlp_max_body_bytes" label="请求体上限（字节）">
                    <InputNumber min={1024} style={{ width: 200 }} />
                  </Form.Item>
                  <Form.Item
                    name="otlp_token"
                    label="OTLP Bearer token"
                    extra={`读出为 ${MASK} 表示已设置；原样保留即不修改`}
                  >
                    <Input.Password style={{ width: 320 }} />
                  </Form.Item>
                  <Form.Item name="otlp_allowed_cidrs" label="来源网段白名单">
                    <Select mode="tags" style={{ width: "100%" }} placeholder="10.0.0.0/8" />
                  </Form.Item>
                </Card>
                <Card size="small" title="资源与日志" style={{ marginBottom: 12 }}>
                  <Form.Item name="cpu_limit_percent" label="CPU 上限（%）">
                    <InputNumber style={{ width: 200 }} />
                  </Form.Item>
                  <Form.Item name="mem_limit_percent" label="内存上限（%）">
                    <InputNumber style={{ width: 200 }} />
                  </Form.Item>
                  <Form.Item name="log_level" label="日志级别">
                    <Input style={{ width: 200 }} />
                  </Form.Item>
                  <Typography.Text type="secondary">
                    这三项目前只记录与上报，Agent 侧没有真实实现 —— 保存后页面会标「未实现」。
                  </Typography.Text>
                </Card>
                <Card size="small" title="认证 token" style={{ marginBottom: 12 }}>
                  <Form.Item
                    name="token"
                    label="token"
                    extra={`读出为 ${MASK} 表示已设置；原样保留即不修改。改成新值会同时轮换台账凭据，并让 Agent 重连重认证。`}
                  >
                    <Input.Password style={{ width: 320 }} />
                  </Form.Item>
                </Card>
                <Card size="small" title="身份（不可下发，只读）">
                  <Typography.Text type="secondary">
                    server_addr 与 agent_id 只能改 Agent 本地配置文件并重启进程；下发含这两个字段的配置会被
                    Agent 直接拒绝。
                  </Typography.Text>
                </Card>
              </Form>
            ),
          },
          {
            key: "items",
            label: `采集项（${items.length}）`,
            children: (
              <Space direction="vertical" style={{ width: "100%" }}>
                <Space>
                  <Button type="primary" onClick={() => openItem()}>
                    新增采集项
                  </Button>
                  <Typography.Text type="secondary">
                    采集项属于这台 Agent 的 spec：改动随「保存期望配置」落库，随「下发」生效。
                  </Typography.Text>
                </Space>
                <Table
                  rowKey={(row) => row.item_id || row.name}
                  dataSource={items}
                  pagination={false}
                  columns={[
                    {
                      title: "item_id",
                      dataIndex: "item_id",
                      render: (id: string) => (
                        <Typography.Text code>{id || "（保存时生成）"}</Typography.Text>
                      ),
                    },
                    { title: "名称", dataIndex: "name" },
                    {
                      title: "类型",
                      dataIndex: "kind",
                      render: (kind: string) => <Tag>{kindLabel(kind)}</Tag>,
                    },
                    {
                      title: "启用",
                      dataIndex: "enabled",
                      render: (enabled: boolean, row) => (
                        <Switch
                          checked={enabled}
                          onChange={(checked) =>
                            setItems((prev) =>
                              prev.map((i) =>
                                i === row || i.item_id === row.item_id ? { ...i, enabled: checked } : i,
                              ),
                            )
                          }
                        />
                      ),
                    },
                    {
                      title: "保留天数",
                      render: (_, row) => (
                        <Typography.Text>
                          {(row.storage as { retention_days?: number })?.retention_days ?? 1}
                        </Typography.Text>
                      ),
                    },
                    {
                      title: "操作",
                      render: (_, row) => (
                        <Space>
                          <Button type="link" onClick={() => openItem(row)}>
                            编辑
                          </Button>
                          <Button
                            type="link"
                            danger
                            onClick={() =>
                              setItems((prev) => prev.filter((i) => i !== row))
                            }
                          >
                            移除
                          </Button>
                        </Space>
                      ),
                    },
                  ]}
                />
              </Space>
            ),
          },
          {
            key: "diff",
            label: `差异（${paramDiffRows(view?.diff).length + itemChanges(view?.diff).length}）`,
            children: (
              <Space direction="vertical" style={{ width: "100%" }} size="middle">
                <Typography.Text type="secondary">
                  期望值与 Agent 上报的生效值之间的差异。空白表示当前一致。
                </Typography.Text>
                <Table
                  rowKey="field"
                  size="small"
                  dataSource={paramDiffRows(view?.diff)}
                  locale={{ emptyText: "参数无差异" }}
                  columns={[
                    { title: "字段", dataIndex: "field" },
                    { title: "期望", dataIndex: "desired" },
                    { title: "生效", dataIndex: "applied" },
                  ]}
                />
                <Table
                  rowKey="itemId"
                  size="small"
                  dataSource={itemChanges(view?.diff)}
                  locale={{ emptyText: "采集项无差异" }}
                  columns={[
                    { title: "item_id", dataIndex: "itemId" },
                    {
                      title: "变化",
                      dataIndex: "change",
                      render: (change: string) => {
                        const label =
                          change === "added" ? "生效值缺少" : change === "removed" ? "需移除" : "内容不同";
                        return <Tag color={change === "changed" ? "orange" : "red"}>{label}</Tag>;
                      },
                    },
                  ]}
                />
              </Space>
            ),
          },
          {
            key: "json",
            label: "原始 JSON",
            children: (
              <Space direction="vertical" style={{ width: "100%" }}>
                <Space>
                  <Button onClick={openJson}>载入当前值</Button>
                  <Button type="primary" loading={saving} onClick={saveJson}>
                    按 JSON 保存
                  </Button>
                  <Typography.Text type="secondary">
                    高级模式：直接编辑整份 spec（params + items）。JSON 非法时不会提交。
                  </Typography.Text>
                </Space>
                <Input.TextArea
                  rows={22}
                  value={jsonText}
                  onChange={(e) => setJsonText(e.target.value)}
                  style={{ fontFamily: "monospace" }}
                />
              </Space>
            ),
          },
        ]}
      />

      <Modal
        open={itemOpen}
        title={editingItemId ? "编辑采集项" : "新增采集项"}
        onCancel={() => setItemOpen(false)}
        onOk={submitItem}
        okText="加入 spec"
        destroyOnClose
        width={640}
      >
        <Form form={itemForm} layout="vertical">
          <Form.Item name="name" label="名称" rules={[{ required: true }]}>
            <Input />
          </Form.Item>
          <Form.Item name="kind" label="类型" rules={[{ required: true }]}>
            <Select options={COLLECT_KINDS} />
          </Form.Item>
          <Form.Item name="enabled" label="启用" valuePropName="checked">
            <Switch />
          </Form.Item>
          <Form.Item name="retention_days" label="保留天数">
            <InputNumber min={1} />
          </Form.Item>

          {itemKind === "metrics_host" ? (
            <Form.Item name="interval_secs" label="采集间隔（秒）">
              <InputNumber min={1} />
            </Form.Item>
          ) : null}

          {itemKind === "log_file" ? (
            <Form.Item name="path_patterns" label="路径 glob">
              <Select mode="tags" placeholder="/var/log/*.log" />
            </Form.Item>
          ) : null}

          {itemKind === "log_k8s_stdout" ? (
            <>
              <Form.Item name="namespace" label="命名空间">
                <Input />
              </Form.Item>
              <Form.Item name="pod_name_pattern" label="Pod 名 glob">
                <Input />
              </Form.Item>
              <Form.Item name="container" label="容器名">
                <Input placeholder="留空表示该 Pod 全部容器" />
              </Form.Item>
              <Form.Item name="kubeconfig" label="kubeconfig 路径">
                <Input placeholder="留空用 ~/.kube/config" />
              </Form.Item>
            </>
          ) : null}

          {itemKind === "apm_otlp" ? (
            <>
              <Form.Item name="service_allowlist" label="服务名白名单">
                <Input placeholder="逗号分隔，留空不限" />
              </Form.Item>
              <Form.Item name="service_denylist" label="服务名黑名单">
                <Input placeholder="逗号分隔" />
              </Form.Item>
              <Form.Item name="attribute_allowlist" label="属性白名单">
                <Input placeholder="逗号分隔" />
              </Form.Item>
              <Form.Item name="batch_max_records" label="攒批条数">
                <InputNumber min={1} max={5000} />
              </Form.Item>
            </>
          ) : null}

          {isEbpfKind(itemKind) ? (
            <>
              <Form.Item name="include_loopback" label="包含本地回环" valuePropName="checked">
                <Switch />
              </Form.Item>
              <Form.Item name="raw_events_enabled" label="上报原始事件" valuePropName="checked">
                <Switch />
              </Form.Item>
              {itemKind === "ebpf_syscall" ? (
                <Form.Item name="slow_threshold_micros" label="慢调用阈值（微秒）">
                  <InputNumber min={0} />
                </Form.Item>
              ) : null}
            </>
          ) : null}

          {itemKind === "log_file" || itemKind === "log_k8s_stdout" ? (
            <>
              <Form.Item name="start_mode" label="起始位置">
                <Select
                  options={[
                    { value: "tail", label: "tail（只看新增）" },
                    { value: "head", label: "head（从头读）" },
                  ]}
                />
              </Form.Item>
              <Form.Item name="batch_max_records" label="攒批条数">
                <InputNumber min={1} />
              </Form.Item>
              <Form.Item name="include_regex" label="行包含正则">
                <Input />
              </Form.Item>
              <Form.Item name="exclude_regex" label="行排除正则">
                <Input />
              </Form.Item>
            </>
          ) : null}
        </Form>
      </Modal>
    </Space>
  );
}
