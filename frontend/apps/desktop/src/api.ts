export type DesktopApp = {
  app_id: string;
  name: string;
  url: string;
  tags: string[];
  created_at: string;
  updated_at: string;
};

// 旧版本后端可能不返回 tags，统一补空数组以保证 DesktopApp 不变量，
// 避免渲染层对 tags 取值时崩溃。
function normalizeApp(app: DesktopApp): DesktopApp {
  return { ...app, tags: app.tags ?? [] };
}

async function readError(res: Response): Promise<string> {
  try {
    const body = (await res.json()) as { message?: string; error?: string };
    return body.message || body.error || res.statusText;
  } catch {
    return res.statusText;
  }
}

export async function listApps(): Promise<DesktopApp[]> {
  const res = await fetch("/api/console/apps");
  if (!res.ok) throw new Error(await readError(res));
  const apps = (await res.json()) as DesktopApp[];
  return apps.map(normalizeApp);
}

export async function createApp(
  name: string,
  url: string,
  tags: string[],
): Promise<DesktopApp> {
  const res = await fetch("/api/console/apps", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ name, url, tags }),
  });
  if (!res.ok) throw new Error(await readError(res));
  return normalizeApp((await res.json()) as DesktopApp);
}

export async function updateApp(
  appId: string,
  name: string,
  url: string,
  tags: string[],
): Promise<DesktopApp> {
  const res = await fetch(`/api/console/apps/${encodeURIComponent(appId)}`, {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ name, url, tags }),
  });
  if (!res.ok) throw new Error(await readError(res));
  return normalizeApp((await res.json()) as DesktopApp);
}

export async function deleteApp(appId: string): Promise<void> {
  const res = await fetch(`/api/console/apps/${encodeURIComponent(appId)}`, {
    method: "DELETE",
  });
  if (!res.ok) throw new Error(await readError(res));
}
