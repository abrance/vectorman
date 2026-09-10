import { useCallback, useEffect } from "react";
import type { JobTemplate, TemplateInput } from "@vectorman/adapters";
import { useRuntime } from "../../app/runtime";
import { toAppError } from "./errors";
import { useQueryRecord } from "./use-query";

const KEY = "templates.list";

export type TemplateVarSource = Pick<
  JobTemplate,
  "script" | "args" | "env" | "working_dir"
>;

/**
 * 本地提取模板声明的 `${name}` 变量，供提交表单渲染输入项。
 * 名称规则与后端一致：首字符字母或下划线，其余字母/数字/下划线。
 */
export function extractVariables(t: TemplateVarSource): string[] {
  const sources = [
    t.script,
    ...(t.args ?? []),
    ...Object.values(t.env ?? {}),
    t.working_dir ?? "",
  ];
  const names = new Set<string>();
  const re = /\$\{([A-Za-z_][A-Za-z0-9_]*)\}/g;
  for (const src of sources) {
    re.lastIndex = 0;
    let m: RegExpExecArray | null;
    while ((m = re.exec(src)) !== null) {
      names.add(m[1]);
    }
  }
  return [...names].sort();
}

export function useJobTemplates() {
  const { templates, query, notifier } = useRuntime();
  const list = useQueryRecord<JobTemplate[]>(query, KEY);

  const refresh = useCallback(
    async (silent: boolean) => {
      if (!silent) {
        query.setLoading(KEY);
      }
      try {
        const data = await templates.listTemplates();
        query.setSuccess(KEY, data);
      } catch (e) {
        const err = toAppError(e);
        if (silent) {
          notifier.warning(err.message);
        } else {
          query.setError(KEY, err);
          notifier.error(err);
        }
      }
    },
    [templates, query, notifier],
  );

  useEffect(() => {
    void refresh(false);
  }, [refresh]);

  const create = useCallback(
    async (input: TemplateInput): Promise<JobTemplate> => {
      const t = await templates.createTemplate(input);
      await refresh(false);
      notifier.success(`模板已创建：${t.name}`);
      return t;
    },
    [templates, refresh, notifier],
  );

  const update = useCallback(
    async (templateId: string, input: TemplateInput): Promise<JobTemplate> => {
      const t = await templates.updateTemplate(templateId, input);
      await refresh(false);
      notifier.success(`模板已更新：${t.name}`);
      return t;
    },
    [templates, refresh, notifier],
  );

  const remove = useCallback(
    async (templateId: string): Promise<void> => {
      await templates.deleteTemplate(templateId);
      await refresh(false);
      notifier.success("模板已删除");
    },
    [templates, refresh, notifier],
  );

  return { list, refresh, create, update, remove };
}
