import { Layout, Menu } from "antd";
import { Navigate, NavLink, Route, Routes, useLocation } from "react-router-dom";
import { AccessPointsPage } from "../pages/access-points-page";
import { AgentConfigsPage } from "../pages/agent-configs-page";
import { AgentsPage } from "../pages/agents-page";
import { HostsPage } from "../pages/hosts-page";
import { ToastHost } from "./ToastHost";

const items = [
  { key: "/hosts", label: <NavLink to="/hosts">主机</NavLink> },
  { key: "/access-points", label: <NavLink to="/access-points">接入点</NavLink> },
  { key: "/agents", label: <NavLink to="/agents">Agent</NavLink> },
  { key: "/agent-configs", label: <NavLink to="/agent-configs">Agent 配置</NavLink> },
];

export function App() {
  const location = useLocation();
  return (
    <Layout style={{ minHeight: "100vh" }}>
      <ToastHost />
      <Layout.Sider theme="light" width={200}>
        <div style={{ padding: 16, fontWeight: 600 }}>节点管理</div>
        <Menu mode="inline" items={items} selectedKeys={[location.pathname]} />
      </Layout.Sider>
      <Layout.Content style={{ padding: 24 }}>
        <Routes>
          <Route path="/" element={<Navigate to="/hosts" replace />} />
          <Route path="/hosts" element={<HostsPage />} />
          <Route path="/access-points" element={<AccessPointsPage />} />
          <Route path="/agents" element={<AgentsPage />} />
          <Route path="/agent-configs" element={<AgentConfigsPage />} />
        </Routes>
      </Layout.Content>
    </Layout>
  );
}
