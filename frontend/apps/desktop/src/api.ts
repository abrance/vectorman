export type DesktopApp = {
  app_id: string;
  name: string;
  url: string;
  created_at: string;
  updated_at: string;
};

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
  return res.json();
}

export async function createApp(name: string, url: string): Promise<DesktopApp> {
  const res = await fetch("/api/console/apps", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ name, url }),
  });
  if (!res.ok) throw new Error(await readError(res));
  return res.json();
}

export async function updateApp(appId: string, name: string, url: string): Promise<DesktopApp> {
  const res = await fetch(`/api/console/apps/${encodeURIComponent(appId)}`, {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ name, url }),
  });
  if (!res.ok) throw new Error(await readError(res));
  return res.json();
}

export async function deleteApp(appId: string): Promise<void> {
  const res = await fetch(`/api/console/apps/${encodeURIComponent(appId)}`, {
    method: "DELETE",
  });
  if (!res.ok) throw new Error(await readError(res));
}
