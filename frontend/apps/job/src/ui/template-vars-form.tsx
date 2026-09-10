import { Form, Input, Typography } from "antd";

export function TemplateVarsForm({ vars }: { vars: string[] }) {
  if (vars.length === 0) {
    return null;
  }
  return (
    <div>
      <Typography.Text strong>变量</Typography.Text>
      {vars.map((v) => (
        <Form.Item
          key={v}
          name={["vars", v]}
          label={v}
          rules={[{ required: true, message: `请填写 ${v}` }]}
        >
          <Input placeholder={`\${${v}}`} />
        </Form.Item>
      ))}
    </div>
  );
}
