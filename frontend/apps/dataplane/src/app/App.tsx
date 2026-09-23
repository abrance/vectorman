import { Layout, Menu } from "antd";
import { NavLink, Navigate, Route, Routes, useLocation } from "react-router-dom";
import { CollectPage } from "../pages/collect-page";
import { LogsPage } from "../pages/logs-page";
import { MetricsPage } from "../pages/metrics-page";
import { ApmPage } from "../pages/apm-page";
import { SettingsPage } from "../pages/settings-page";
import { TopologyPage } from "../pages/topology-page";
import { TraceDetailPage } from "../pages/trace-detail-page";
import { TracesPage } from "../pages/traces-page";
import { ToastHost } from "./ToastHost";

const items = [
  { key: "/", label: <NavLink to="/">采集链路</NavLink> },
  { key: "/metrics", label: <NavLink to="/metrics">指标</NavLink> },
  { key: "/logs", label: <NavLink to="/logs">日志</NavLink> },
  { key: "/traces", label: <NavLink to="/traces">trace</NavLink> },
  { key: "/topology", label: <NavLink to="/topology">服务拓扑</NavLink> },
  { key: "/apm", label: <NavLink to="/apm">APM 指标</NavLink> },
  { key: "/settings", label: <NavLink to="/settings">设置</NavLink> },
];

export function App() {
  const location = useLocation();
  const selectedKey = location.pathname.startsWith("/traces")
    ? "/traces"
    : location.pathname.startsWith("/settings")
      ? "/settings"
      : location.pathname;
  return (
    <Layout style={{ minHeight: "100vh" }}>
      <ToastHost />
      <Layout.Sider theme="light" width={180}>
        <div style={{ padding: 16, fontWeight: 600 }}>vectorman 数据面</div>
        <Menu mode="inline" items={items} selectedKeys={[selectedKey]} />
      </Layout.Sider>
      <Layout.Content style={{ padding: 24 }}>
        <Routes>
          <Route path="/" element={<CollectPage />} />
          <Route path="/metrics" element={<MetricsPage />} />
          <Route path="/logs" element={<LogsPage />} />
          <Route path="/traces" element={<TracesPage />} />
          <Route path="/traces/:traceId" element={<TraceDetailPage />} />
          <Route path="/topology" element={<TopologyPage />} />
          <Route path="/apm" element={<ApmPage />} />
          <Route path="/settings" element={<SettingsPage />} />
          <Route path="/settings/service-aliases" element={<SettingsPage defaultTab="aliases" />} />
          <Route path="*" element={<Navigate to="/" replace />} />
        </Routes>
      </Layout.Content>
    </Layout>
  );
}
