import { FormEvent, useCallback, useEffect, useMemo, useState } from "react";
import {
  createApp,
  deleteApp,
  listApps,
  updateApp,
  type DesktopApp,
} from "../api";

type Dialog = { mode: "create" } | { mode: "edit"; app: DesktopApp };

function firstGlyph(name: string): string {
  const ch = Array.from(name.trim())[0];
  return ch ? ch.toUpperCase() : "?";
}

function openApp(url: string) {
  window.open(url, "_blank", "noopener,noreferrer");
}

export function App() {
  const [apps, setApps] = useState<DesktopApp[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [dialog, setDialog] = useState<Dialog | null>(null);
  const [name, setName] = useState("");
  const [url, setUrl] = useState("");
  const [now, setNow] = useState(() => new Date());

  const load = useCallback(async () => {
    try {
      setError(null);
      setApps(await listApps());
    } catch (e) {
      setError(e instanceof Error ? e.message : "加载失败");
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    const id = window.setInterval(() => setNow(new Date()), 1000);
    return () => window.clearInterval(id);
  }, []);

  const clock = useMemo(
    () =>
      now.toLocaleString("zh-CN", {
        hour12: false,
        month: "2-digit",
        day: "2-digit",
        hour: "2-digit",
        minute: "2-digit",
      }),
    [now],
  );

  function openCreate() {
    setName("");
    setUrl("http://");
    setDialog({ mode: "create" });
  }

  function openEdit(app: DesktopApp) {
    setName(app.name);
    setUrl(app.url);
    setDialog({ mode: "edit", app });
  }

  async function onSubmit(ev: FormEvent) {
    ev.preventDefault();
    try {
      if (dialog?.mode === "edit") {
        await updateApp(dialog.app.app_id, name, url);
      } else {
        await createApp(name, url);
      }
      setDialog(null);
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : "保存失败");
    }
  }

  async function onDelete(app: DesktopApp) {
    if (!window.confirm(`删除 App「${app.name}」？`)) return;
    try {
      await deleteApp(app.app_id);
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : "删除失败");
    }
  }

  return (
    <div className="desktop">
      <div className="watermark">VECTORMAN</div>
      <header className="topbar">
        <div className="brand">Desktop</div>
        <div className="clock">{clock}</div>
      </header>
      {error ? (
        <div className="error">
          {error}
          <button type="button" onClick={() => void load()}>
            重试
          </button>
        </div>
      ) : null}
      <div className="grid">
        {apps.map((app) => (
          <div key={app.app_id}>
            <button type="button" className="tile" onClick={() => openApp(app.url)}>
              <div className="glyph">{firstGlyph(app.name)}</div>
              <div className="label">{app.name}</div>
            </button>
            <div className="actions">
              <button type="button" onClick={() => openEdit(app)}>
                编辑
              </button>
              <button type="button" onClick={() => void onDelete(app)}>
                删除
              </button>
            </div>
          </div>
        ))}
        <button type="button" className="tile add" onClick={openCreate}>
          <div className="glyph">+</div>
          <div className="label">添加 App</div>
        </button>
        {apps.length === 0 && !error ? (
          <div className="empty">目录是空的。把 GSE 或其他页面加进来。</div>
        ) : null}
      </div>
      {dialog ? (
        <div className="modal-backdrop">
          <form className="modal" onSubmit={(e) => void onSubmit(e)}>
            <h2>{dialog.mode === "edit" ? "编辑 App" : "添加 App"}</h2>
            <label>
              名称
              <input value={name} onChange={(e) => setName(e.target.value)} required maxLength={64} />
            </label>
            <label>
              URL
              <input value={url} onChange={(e) => setUrl(e.target.value)} required maxLength={2048} />
            </label>
            <div className="modal-actions">
              <button type="button" className="ghost" onClick={() => setDialog(null)}>
                取消
              </button>
              <button type="submit" className="primary">
                保存
              </button>
            </div>
          </form>
        </div>
      ) : null}
    </div>
  );
}
