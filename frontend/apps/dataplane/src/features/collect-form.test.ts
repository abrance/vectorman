import { describe, expect, it } from "vitest";
import type { CollectItem } from "@vectorman/adapters";
import {
  COLLECT_KINDS,
  splitList,
  toCollectFormValues,
  toCollectItemInput,
  type CollectFormValues,
} from "./collect-form";

const base: CollectFormValues = {
  name: "  cpu  ",
  agent_ids: ["a-1", "a-2"],
  kind: "metrics_host",
  enabled: true,
  retention_days: 0,
  interval_secs: 0,
};

describe("toCollectItemInput", () => {
  it("trims name and falls back to defaults", () => {
    const input = toCollectItemInput(base);
    expect(input.name).toBe("cpu");
    expect(input.agent_ids).toEqual(["a-1", "a-2"]);
    expect(input.collector).toEqual({ interval_secs: 15 });
    expect(input.storage).toEqual({ retention_days: 1 });
  });

  it("keeps only relevant fields per kind", () => {
    const logFile = toCollectItemInput({
      ...base,
      kind: "log_file",
      path_patterns: [" /var/log/*.log ", ""],
      start_mode: "head",
      start_n: 3,
      batch_max_records: 10,
      flush_interval_secs: 2,
      include_regex: " keep ",
      exclude_regex: "",
      extract: [
        { kind: "regex", expr: "user=(\\w+)", label: "user" },
        { kind: "regex", expr: "", label: "empty" },
      ],
    });
    expect(logFile.collector).toEqual({
      path_patterns: ["/var/log/*.log"],
      start_mode: "head",
      start_n: 3,
      batch_max_records: 10,
      flush_interval_secs: 2,
      clean: {
        include_regex: "keep",
        extract: [{ kind: "regex", expr: "user=(\\w+)", label: "user" }],
      },
    });
  });

  it("round-trips a log_k8s_stdout item", () => {
    const item: CollectItem = {
      item_id: "item-9",
      agent_ids: ["a-1"],
      name: "pods",
      kind: "log_k8s_stdout",
      enabled: false,
      collector: {
        namespace: "default",
        pod_name_pattern: "nginx-*",
        container: "",
        kubeconfig: "",
        start_mode: "tail",
        start_n: 0,
        batch_max_records: 100,
        flush_interval_secs: 5,
        clean: { exclude_regex: "health" },
      },
      storage: { retention_days: 3 },
    };
    const input = toCollectItemInput(toCollectFormValues(item));
    expect(input).toEqual({
      name: "pods",
      agent_ids: ["a-1"],
      kind: "log_k8s_stdout",
      enabled: false,
      collector: item.collector,
      storage: { retention_days: 3 },
    });
  });
});

describe("apm_otlp", () => {
  it("splits and dedupes名单, defaults batching", () => {
    expect(splitList("a, b\na,,c ")).toEqual(["a", "b", "c"]);
    expect(splitList(undefined)).toEqual([]);

    const input = toCollectItemInput({
      name: "apm",
      agent_ids: ["a-1"],
      kind: "apm_otlp",
      enabled: true,
      retention_days: 3,
      service_allowlist: "order-api, payment",
      service_denylist: "debug",
      attribute_allowlist: "http.request.method",
    });
    expect(input.kind).toBe("apm_otlp");
    expect(input.collector).toEqual({
      service_allowlist: ["order-api", "payment"],
      service_denylist: ["debug"],
      attribute_allowlist: ["http.request.method"],
      batch_max_records: 100,
      flush_interval_secs: 5,
    });
    expect(input.storage).toEqual({ retention_days: 3 });
  });

  it("round-trips through the form values", () => {
    const values = toCollectFormValues({
      item_id: "item-apm",
      name: "apm",
      agent_ids: ["a-1"],
      kind: "apm_otlp",
      enabled: true,
      collector: {
        service_allowlist: ["order-api"],
        service_denylist: [],
        attribute_allowlist: ["http.request.method"],
        batch_max_records: 50,
        flush_interval_secs: 10,
      },
      storage: { retention_days: 3 },
    });
    expect(values.service_allowlist).toBe("order-api");
    expect(values.service_denylist).toBe("");
    expect(values.attribute_allowlist).toBe("http.request.method");
    const back = toCollectItemInput(values);
    expect(back.collector).toMatchObject({
      service_allowlist: ["order-api"],
      batch_max_records: 50,
      flush_interval_secs: 10,
    });
  });
});

describe("ebpf", () => {
  it("下拉里包含全部 eBPF 类型（界面能开出这些采集项）", () => {
    const values = COLLECT_KINDS.map((k) => k.value);
    for (const kind of ["ebpf_network", "ebpf_tcp", "ebpf_process", "ebpf_syscall"]) {
      expect(values).toContain(kind);
    }
  });

  it("按类型构造 collector：只带该类型相关的参数", () => {
    const network = toCollectItemInput({
      ...base,
      kind: "ebpf_network",
      flush_interval_secs: 0,
    });
    expect(network.collector).toEqual({
      flush_interval_secs: 10,
      include_loopback: false,
      raw_events_enabled: false,
    });

    // syscall 额外带慢调用阈值（未填走缺省 100ms）。
    const syscall = toCollectItemInput({
      ...base,
      kind: "ebpf_syscall",
      flush_interval_secs: 30,
      raw_events_enabled: true,
      include_loopback: true,
    });
    expect(syscall.collector).toEqual({
      flush_interval_secs: 30,
      include_loopback: true,
      raw_events_enabled: true,
      slow_threshold_micros: 100_000,
    });

    // 其它 eBPF 类型不出现 syscall 专有字段。
    const tcp = toCollectItemInput({ ...base, kind: "ebpf_tcp" });
    expect(tcp.collector).not.toHaveProperty("slow_threshold_micros");

    // 阈值可显式指定，且不会被当成负数。
    const custom = toCollectItemInput({
      ...base,
      kind: "ebpf_syscall",
      slow_threshold_micros: 250_000,
    });
    expect(custom.collector.slow_threshold_micros).toBe(250_000);
    const negative = toCollectItemInput({
      ...base,
      kind: "ebpf_syscall",
      slow_threshold_micros: -5,
    });
    expect(negative.collector.slow_threshold_micros).toBe(0);
  });

  it("编辑回填：eBPF 参数能读回表单", () => {
    const item: CollectItem = {
      item_id: "item-1",
      agent_ids: ["a-1"],
      name: "ebpf net",
      kind: "ebpf_network",
      enabled: true,
      collector: {
        flush_interval_secs: 20,
        include_loopback: true,
        raw_events_enabled: true,
      },
      storage: { retention_days: 3 },
    };
    const values = toCollectFormValues(item);
    expect(values.kind).toBe("ebpf_network");
    expect(values.flush_interval_secs).toBe(20);
    expect(values.include_loopback).toBe(true);
    expect(values.raw_events_enabled).toBe(true);
    expect(values.slow_threshold_micros).toBe(100_000);
    // 回填后再提交保持同一形状（round-trip）。
    expect(toCollectItemInput(values).collector).toEqual(item.collector);
  });
});
