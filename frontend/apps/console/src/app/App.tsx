import { Layout, Menu } from "antd";
import { Navigate, NavLink, Route, Routes, useLocation } from "react-router-dom";
import {
  AccessPointsPage,
  AgentConfigsPage,
  AgentsPage,
  HostsPage,
  ToastHost,
} from "@vectorman/node";
import { JobsPage, TemplatesPage } from "@vectorman/job";

const items = [
  {
    key: "node",
    type: "group" as const,
    label: "节点管理",
    children: [
      { key: "/hosts", label: <NavLink to="/hosts">主机</NavLink> },
      { key: "/access-points", label: <NavLink to="/access-points">接入点</NavLink> },
      { key: "/agents", label: <NavLink to="/agents">Agent</NavLink> },
      { key: "/agent-configs", label: <NavLink to="/agent-configs">Agent 配置</NavLink> },
    ],
  },
  {
    key: "job",
    type: "group" as const,
    label: "作业平台",
    children: [
      { key: "/jobs", label: <NavLink to="/jobs">作业</NavLink> },
      { key: "/templates", label: <NavLink to="/templates">模板</NavLink> },
    ],
  },
];

export function App() {
  const location = useLocation();
  return (
    <Layout style={{ minHeight: "100vh" }}>
      <ToastHost />
      <Layout.Sider theme="light" width={200}>
        <div style={{ padding: 16, fontWeight: 600 }}>vectorman 控制台</div>
        <Menu mode="inline" items={items} selectedKeys={[location.pathname]} />
      </Layout.Sider>
      <Layout.Content style={{ padding: 24 }}>
        <Routes>
          <Route path="/" element={<Navigate to="/hosts" replace />} />
          <Route path="/hosts" element={<HostsPage />} />
          <Route path="/access-points" element={<AccessPointsPage />} />
          <Route path="/agents" element={<AgentsPage />} />
          <Route path="/agent-configs" element={<AgentConfigsPage />} />
          <Route path="/jobs" element={<JobsPage />} />
          <Route path="/templates" element={<TemplatesPage />} />
        </Routes>
      </Layout.Content>
    </Layout>
  );
}
