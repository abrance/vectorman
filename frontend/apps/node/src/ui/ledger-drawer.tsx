import { Button, Drawer, Space } from "antd";
import type { ReactNode } from "react";

export type DrawerMode = "view" | "create" | "edit";

export function LedgerDrawer({
  open,
  title,
  mode,
  loading,
  missing,
  onClose,
  onSubmit,
  children,
}: {
  open: boolean;
  title: string;
  mode: DrawerMode;
  loading?: boolean;
  missing?: string | null;
  onClose: () => void;
  onSubmit?: () => void;
  children: ReactNode;
}) {
  return (
    <Drawer
      open={open}
      title={title}
      width={480}
      onClose={onClose}
      destroyOnClose
      extra={
        mode === "view" || missing ? null : (
          <Space>
            <Button onClick={onClose}>取消</Button>
            <Button type="primary" loading={loading} disabled={loading} onClick={onSubmit}>
              提交
            </Button>
          </Space>
        )
      }
    >
      {missing ? (
        <Space direction="vertical">
          <span>{missing}</span>
          <Button onClick={onClose}>关闭</Button>
        </Space>
      ) : (
        children
      )}
    </Drawer>
  );
}
