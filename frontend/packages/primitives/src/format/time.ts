/**
 * 后端统一以微秒时间戳（自 Unix epoch）存储时间；`ledger_stamp` 形态为
 * `<micros>-<seq>`。本函数把这些值格式化为本地时区的 `YYYY-MM-DD HH:mm:ss`。
 */

const pad = (value: number): string => String(value).padStart(2, "0");

function toMillis(value: string): number | null {
  const digits = /^\d+/.exec(value.trim())?.[0];
  if (!digits) {
    return null;
  }
  const n = Number(digits);
  if (!Number.isFinite(n)) {
    return null;
  }
  if (n >= 1e14) {
    return Math.floor(n / 1000);
  }
  if (n >= 1e11) {
    return n;
  }
  return n * 1000;
}

export function formatTimestamp(value?: string | null): string {
  if (value === undefined || value === null || value === "") {
    return "-";
  }
  const ms = toMillis(value);
  if (ms === null) {
    return value;
  }
  const date = new Date(ms);
  if (Number.isNaN(date.getTime())) {
    return value;
  }
  return (
    `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}` +
    ` ${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`
  );
}
