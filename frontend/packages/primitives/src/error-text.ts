import type { AppError } from "./error";

/// 服务端 → 页面文案的唯一映射表。
///
/// 服务端的 `message` 是给开发者看的（内部键名、serde 长句、`line N column M`），
/// 直接显示在 toast 上就是事故现场。所有对外提示都必须先过这里：
/// 认得的模式翻成人话，认不得的英文原文回落到「按错误码给的通用句」，绝不直出。
///
/// 新增一条规则时同时加测试（见 error-text.test.ts）：规则依赖服务端 message 的具体
/// 措辞，没有断言锁住格式，下次后端改字就会静默退化成通用句。

type Rule = {
  /// 服务端 message 的匹配模式。
  match: RegExp;
  /// 命中后的中文文案；`m` 是 match 结果，用来取字段名等变量。
  render: (m: RegExpMatchArray) => string;
};

const RULES: Rule[] = [
  // serde 未知字段：`params.bogus_console_probe: unknown field \`bogus_console_probe\`,
  // expected one of \`a\`, \`b\`, … at line 1 column 417`
  {
    match: /unknown field [`'"]?([A-Za-z0-9_.]+)[`'"]?/,
    render: (m) => `配置里有无法识别的字段：${m[1]}（可选字段见「参数」页说明）`,
  },
  // serde 缺字段：`missing field \`items\``
  {
    match: /missing field [`'"]?([A-Za-z0-9_.]+)[`'"]?/,
    render: (m) => `请求缺少必需字段：${m[1]}`,
  },
  // 类型不匹配：`invalid type: string "x", expected u64`
  {
    match: /invalid type: .*?, expected ([A-Za-z0-9_]+)/,
    render: (m) => `字段类型不对：服务端要求 ${m[1]}`,
  },
  // 心跳上限：`heartbeat_interval_secs 不得大于 30（判活窗口 heartbeat_timeout_secs/3 秒）`
  {
    match: /heartbeat_interval_secs 不得大于 (\d+)/,
    render: (m) => `心跳周期不得超过 ${m[1]} 秒（由服务端判活窗口决定）`,
  },
  // 作业超时越界：`timeout_secs must be within 1..=3600`
  // 上限由服务端 config 的 job_max_timeout_secs 决定，所以从 message 里取，不写死。
  {
    match: /timeout_secs must be within 1\.\.=(\d+)/,
    render: (m) => `超时时间必须在 1 到 ${m[1]} 秒之间`,
  },
  // 数据面没配管理口：`gse_admin_url is not configured`
  {
    match: /gse_admin_url is not configured/,
    render: () => "数据面未配置「GSE 管理口地址」，采集链路无法转发；补上配置后再试",
  },
  // 采集项类型不认识：`unsupported kind: xxx`
  {
    match: /unsupported kind: (\S+)/,
    render: (m) => `不支持的采集项类型：${m[1]}`,
  },
  // `item_id 重复: xxx`
  {
    match: /item_id 重复: (\S+)/,
    render: (m) => `采集项 ID 重复：${m[1]}（同一台 Agent 内不能重名）`,
  },
  // `item_id 不得为空`
  {
    match: /item_id 不得为空/,
    render: () => "采集项 ID 不能为空",
  },
  // `heartbeat_interval_secs 必须大于 0`
  {
    match: /heartbeat_interval_secs 必须大于 0/,
    render: () => "心跳周期必须大于 0 秒",
  },
  // 找不到对象：`agent xxx not found`
  {
    match: /\b(agent|host|access point) (\S+) not found/,
    render: (m) => {
      const what = m[1] === "host" ? "主机" : m[1] === "access point" ? "接入点" : "Agent";
      return `${what}不存在或已被删除：${m[2]}`;
    },
  },
];

/// 错误码 → 认不出 message 时给的通用句。宁可含糊，也不把英文原文甩给运维。
const BY_CODE: Record<string, string> = {
  invalid_argument: "这份配置没通过服务端校验，请检查参数后再保存",
  revision_conflict: "配置已被他人修改，请重新加载后再保存",
  not_found: "对象不存在或已被删除",
  unauthorized: "没有权限执行这个操作，请检查令牌是否有效",
  unavailable: "服务端暂时不可用，请稍后重试",
  query_failed: "请求失败，请稍后重试",
};

/// 判断一段文字是不是「服务端原文」（基本没有中文，且像英文句/JSON）。
///
/// 只用于决定要不要回落通用句：已经翻好的中文、用户自己的中文提示一律原样保留。
function looksUntranslated(message: string): boolean {
  if (!message) return true;
  if (/[\u4e00-\u9fa5]/.test(message)) return false;
  return /[A-Za-z]/.test(message);
}

/// 把服务端错误翻成页面上能看懂的一句话。
export function toUserMessage(err: AppError): string {
  const message = err.message ?? "";
  for (const rule of RULES) {
    const m = message.match(rule.match);
    if (m) return rule.render(m);
  }
  if (!looksUntranslated(message)) return message;
  return BY_CODE[err.code] ?? "操作失败，请稍后重试";
}
