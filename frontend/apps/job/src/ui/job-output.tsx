import { Alert, Typography } from "antd";

export function JobOutput({
  title,
  text,
  truncated,
}: {
  title: string;
  text?: string | null;
  truncated?: boolean;
}) {
  return (
    <div>
      <Typography.Text strong>{title}</Typography.Text>
      {truncated ? (
        <Alert type="warning" showIcon message="输出超过上限，已截断" style={{ margin: "4px 0" }} />
      ) : null}
      <pre
        style={{
          maxHeight: 240,
          overflow: "auto",
          background: "#fafafa",
          border: "1px solid #f0f0f0",
          borderRadius: 4,
          padding: 8,
          margin: "4px 0 0",
          whiteSpace: "pre-wrap",
          wordBreak: "break-all",
        }}
      >
        {text ?? ""}
      </pre>
    </div>
  );
}
