import { describe, expect, it } from "vitest";
import type { CollectItem } from "@vectorman/adapters";
import {
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
