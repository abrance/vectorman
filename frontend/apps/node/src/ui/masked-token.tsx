import { Button, Space, Typography } from "antd";
import { useState } from "react";
import { useRuntime } from "../app/runtime";

export function MaskedToken({ value }: { value: string }) {
  const [shown, setShown] = useState(false);
  const { notifier } = useRuntime();

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(value);
      notifier.success("已复制 token");
    } catch {
      notifier.warning("复制失败");
    }
  };

  return (
    <Space>
      <Typography.Text code>{shown ? value : "••••"}</Typography.Text>
      <Button size="small" onClick={() => setShown((s) => !s)}>
        {shown ? "隐藏" : "显示"}
      </Button>
      <Button size="small" onClick={() => void copy()}>
        复制
      </Button>
    </Space>
  );
}
