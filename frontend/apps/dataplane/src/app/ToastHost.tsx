import { message } from "antd";
import { useEffect } from "react";
import { useRuntime } from "./runtime";

export function ToastHost() {
  const { notifier } = useRuntime();
  const [api, holder] = message.useMessage();

  useEffect(() => {
    return notifier.subscribe((notice) => {
      if (notice.level === "success") {
        api.success(notice.message);
      } else if (notice.level === "warning") {
        api.warning(notice.message);
      } else {
        api.error(notice.message);
      }
    });
  }, [notifier, api]);

  return holder;
}
