import { Button, Drawer, Form, Input, InputNumber, Modal, Select, Space, Switch, Table, Tag } from "antd";
import { useEffect, useState } from "react";
import { useNavigate } from "react-router-dom";
import type { CollectItem } from "@vectorman/adapters";
import { formatTimestamp } from "@vectorman/primitives";
import { useRuntime } from "../app/runtime";
import {
  COLLECT_KINDS,
  toCollectFormValues,
  type CollectFormValues,
} from "../features/collect-form";
import { toAppError } from "../features/errors";
import { summarizeStreams, useCollectItems } from "../features/use-collect";

type DrawerMode = "view" | "create" | "edit";

const kindLabel = (kind: string) => COLLECT_KINDS.find((k) => k.value === kind)?.label ?? kind;

const DEFAULTS: CollectFormValues = {
  name: "",
  agent_ids: [],
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
};

export function CollectPage() {
  const { notifier } = useRuntime();
  const { list, agents, streams, refresh, loadAgents, getOne, save, setEnabled, remove } =
    useCollectItems();
  const navigate = useNavigate();
  const [form] = Form.useForm<CollectFormValues>();
  const [open, setOpen] = useState(false);
  const [mode, setMode] = useState<DrawerMode>("create");
  const [editingId, setEditingId] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const [missing, setMissing] = useState<string | null>(null);
  const kind = Form.useWatch("kind", form) ?? "metrics_host";

  useEffect(() => {
    void refresh();
    void loadAgents();
  }, [refresh, loadAgents]);

  const summary = summarizeStreams(streams.data ?? []);

  const openCreate = () => {
    setMode("create");
    setEditingId(null);
    setMissing(null);
    form.resetFields();
    form.setFieldsValue(DEFAULTS);
    setOpen(true);
  };

  const openWith = async (itemId: string, next: DrawerMode) => {
    setMode(next);
    setEditingId(itemId);
    setMissing(null);
    form.resetFields();
    setOpen(true);
    try {
      form.setFieldsValue(toCollectFormValues(await getOne(itemId)));
    } catch (e) {
      setMissing(toAppError(e).message);
    }
  };

  const submit = async () => {
    try {
      const values = await form.validateFields();
      setSubmitting(true);
      await save(editingId, values);
      setOpen(false);
    } catch (e) {
      if (e && typeof e === "object" && "errorFields" in e) {
        return;
      }
      notifier.error(toAppError(e));
    } finally {
      setSubmitting(false);
    }
  };

  const confirmDelete = (item: CollectItem) => {
    Modal.confirm({
      title: "删除采集项",
      content: `将删除采集项 ${item.name}（${item.item_id}）`,
      okText: "删除",
      cancelText: "取消",
      onOk: () => remove(item.item_id),
    });
  };

  const openSearch = (item: CollectItem) => {
    const agentId = item.agent_ids[0] ?? "";
    const query = `agent_id=${encodeURIComponent(agentId)}&data_id=${encodeURIComponent(item.item_id)}`;
    if (item.kind === "metrics_host") {
      navigate(`/metrics?${query}`);
    } else {
      navigate(`/logs?${query}&data_type=logs`);
    }
  };

  return (
    <Space direction="vertical" style={{ width: "100%" }} size="middle">
      <Space>
        <Button type="primary" onClick={openCreate}>
          新建采集项
        </Button>
        <Button onClick={() => void refresh()}>刷新</Button>
      </Space>
      <Table
        rowKey="item_id"
        loading={list.status === "loading"}
        dataSource={list.data ?? []}
        locale={{ emptyText: list.error?.message ?? "暂无采集项" }}
        columns={[
          { title: "名称", dataIndex: "name" },
          {
            title: "类型",
            dataIndex: "kind",
            render: (value: string) => <Tag>{kindLabel(value)}</Tag>,
          },
          {
            title: "目标 Agents",
            dataIndex: "agent_ids",
            render: (ids: string[]) => ids.join("、"),
          },
          {
            title: "启用",
            dataIndex: "enabled",
            render: (_, row) => (
              <Switch
                checked={row.enabled}
                aria-label={`启用 ${row.name}`}
                onChange={(checked) => void setEnabled(row, checked)}
              />
            ),
          },
          {
            title: "最近接入",
            render: (_, row) => {
              const s = summary.get(row.item_id);
              return s ? formatTimestamp(String(s.lastSeenMicros)) : "-";
            },
          },
          {
            title: "accepted",
            render: (_, row) => summary.get(row.item_id)?.accepted ?? 0,
          },
          {
            title: "操作",
            render: (_, row) => (
              <Space>
                <Button type="link" aria-label={`编辑 ${row.name}`} onClick={() => void openWith(row.item_id, "edit")}>
                  编辑
                </Button>
                <Button type="link" aria-label={`详情 ${row.name}`} onClick={() => void openWith(row.item_id, "view")}>
                  详情
                </Button>
                <Button type="link" aria-label={`数据检索 ${row.name}`} onClick={() => openSearch(row)}>
                  数据检索
                </Button>
                <Button type="link" danger aria-label={`删除 ${row.name}`} onClick={() => confirmDelete(row)}>
                  删除
                </Button>
              </Space>
            ),
          },
        ]}
      />
      <Drawer
        open={open}
        width={560}
        destroyOnClose
        title={mode === "create" ? "新建采集项" : mode === "edit" ? "编辑采集项" : "采集项详情"}
        onClose={() => setOpen(false)}
        extra={
          mode === "view" || missing ? null : (
            <Space>
              <Button onClick={() => setOpen(false)}>取消</Button>
              <Button type="primary" loading={submitting} disabled={submitting} onClick={() => void submit()}>
                提交
              </Button>
            </Space>
          )
        }
      >
        {missing ? (
          <Space direction="vertical">
            <span>{missing}</span>
            <Button onClick={() => setOpen(false)}>关闭</Button>
          </Space>
        ) : (
          <Form form={form} layout="vertical" disabled={mode === "view"}>
            <Form.Item name="name" label="名称" rules={[{ required: true, message: "请输入名称" }]}>
              <Input />
            </Form.Item>
            <Form.Item
              name="agent_ids"
              label="目标 Agents"
              rules={[{ required: true, message: "至少选择一个 Agent" }]}
            >
              <Select
                mode="multiple"
                placeholder="选择 Agent"
                options={(agents.data ?? []).map((a) => ({ value: a.agent_id, label: a.agent_id }))}
              />
            </Form.Item>
            <Form.Item name="kind" label="类型" rules={[{ required: true }]}>
              <Select options={COLLECT_KINDS} />
            </Form.Item>
            <Form.Item name="enabled" label="启用" valuePropName="checked">
              <Switch />
            </Form.Item>

            {kind === "metrics_host" && (
              <Form.Item name="interval_secs" label="采集间隔（秒）">
                <InputNumber min={1} style={{ width: "100%" }} />
              </Form.Item>
            )}

            {kind === "log_file" && (
              <Form.Item name="path_patterns" label="路径 glob" rules={[{ required: true, message: "至少一个路径" }]}>
                <Select mode="tags" placeholder="例如 /var/log/*.log" />
              </Form.Item>
            )}

            {kind === "log_k8s_stdout" && (
              <>
                <Form.Item name="namespace" label="namespace" rules={[{ required: true, message: "请输入 namespace" }]}>
                  <Input />
                </Form.Item>
                <Form.Item
                  name="pod_name_pattern"
                  label="Pod 名 glob"
                  rules={[{ required: true, message: "请输入 Pod 名 glob" }]}
                >
                  <Input placeholder="例如 nginx-*" />
                </Form.Item>
                <Form.Item name="container" label="容器名（留空表示全部容器）">
                  <Input />
                </Form.Item>
                <Form.Item name="kubeconfig" label="kubeconfig（留空用 in-cluster 或 ~/.kube/config）">
                  <Input />
                </Form.Item>
              </>
            )}

            {(kind === "log_file" || kind === "log_k8s_stdout") && (
              <>
                <Form.Item name="start_mode" label="开始标记">
                  <Select
                    options={[
                      { value: "tail", label: "tail" },
                      { value: "head", label: "head" },
                    ]}
                  />
                </Form.Item>
                <Form.Item name="start_n" label="开始行数 start_n">
                  <InputNumber min={0} style={{ width: "100%" }} />
                </Form.Item>
                <Form.Item name="batch_max_records" label="批次上限">
                  <InputNumber min={1} style={{ width: "100%" }} />
                </Form.Item>
                <Form.Item name="flush_interval_secs" label="上报间隔（秒）">
                  <InputNumber min={1} style={{ width: "100%" }} />
                </Form.Item>
                <Form.Item name="include_regex" label="包含正则">
                  <Input />
                </Form.Item>
                <Form.Item name="exclude_regex" label="排除正则">
                  <Input />
                </Form.Item>
                <Form.List name="extract">
                  {(fields, { add, remove: removeRule }) => (
                    <>
                      {fields.map((field) => (
                        <Space key={field.key} align="baseline" style={{ display: "flex" }}>
                          <Form.Item name={[field.name, "kind"]} initialValue="regex">
                            <Select
                              style={{ width: 96 }}
                              options={[
                                { value: "regex", label: "regex" },
                                { value: "json", label: "json" },
                              ]}
                            />
                          </Form.Item>
                          <Form.Item name={[field.name, "expr"]}>
                            <Input placeholder="表达式" />
                          </Form.Item>
                          <Form.Item name={[field.name, "label"]}>
                            <Input placeholder="label" />
                          </Form.Item>
                          <Button type="link" danger onClick={() => removeRule(field.name)}>
                            删除
                          </Button>
                        </Space>
                      ))}
                      <Button type="link" onClick={() => add({ kind: "regex", expr: "", label: "" })}>
                        添加提取规则
                      </Button>
                    </>
                  )}
                </Form.List>
              </>
            )}

            <Form.Item name="retention_days" label="保存周期（天）">
              <InputNumber min={1} style={{ width: "100%" }} />
            </Form.Item>
          </Form>
        )}
      </Drawer>
    </Space>
  );
}
