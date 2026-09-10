import { Layout, Menu } from "antd";
import { Navigate, NavLink, Route, Routes, useLocation } from "react-router-dom";
import { JobsPage } from "../pages/jobs-page";
import { TemplatesPage } from "../pages/templates-page";
import { ToastHost } from "./ToastHost";

const items = [
  { key: "/jobs", label: <NavLink to="/jobs">作业</NavLink> },
  { key: "/templates", label: <NavLink to="/templates">模板</NavLink> },
];

export function App() {
  const location = useLocation();
  return (
    <Layout style={{ minHeight: "100vh" }}>
      <ToastHost />
      <Layout.Sider theme="light" width={200}>
        <div style={{ padding: 16, fontWeight: 600 }}>作业平台</div>
        <Menu mode="inline" items={items} selectedKeys={[location.pathname]} />
      </Layout.Sider>
      <Layout.Content style={{ padding: 24 }}>
        <Routes>
          <Route path="/" element={<Navigate to="/jobs" replace />} />
          <Route path="/jobs" element={<JobsPage />} />
          <Route path="/templates" element={<TemplatesPage />} />
        </Routes>
      </Layout.Content>
    </Layout>
  );
}
