import { Layout, Menu } from "antd";
import { NavLink, Navigate, Route, Routes, useLocation } from "react-router-dom";
import { CollectPage } from "../pages/collect-page";
import { LogsPage } from "../pages/logs-page";
import { MetricsPage } from "../pages/metrics-page";
import { ToastHost } from "./ToastHost";

const items = [
  { key: "/", label: <NavLink to="/">采集链路</NavLink> },
  { key: "/metrics", label: <NavLink to="/metrics">指标</NavLink> },
  { key: "/logs", label: <NavLink to="/logs">日志</NavLink> },
];

export function App() {
  const location = useLocation();
  return (
    <Layout style={{ minHeight: "100vh" }}>
      <ToastHost />
      <Layout.Sider theme="light" width={180}>
        <div style={{ padding: 16, fontWeight: 600 }}>vectorman 数据面</div>
        <Menu mode="inline" items={items} selectedKeys={[location.pathname]} />
      </Layout.Sider>
      <Layout.Content style={{ padding: 24 }}>
        <Routes>
          <Route path="/" element={<CollectPage />} />
          <Route path="/metrics" element={<MetricsPage />} />
          <Route path="/logs" element={<LogsPage />} />
          <Route path="*" element={<Navigate to="/" replace />} />
        </Routes>
      </Layout.Content>
    </Layout>
  );
}
