import { FormEvent, useCallback, useEffect, useMemo, useState } from "react";
import {
  createApp,
  deleteApp,
  listApps,
  recordAppClick,
  updateApp,
  type DesktopApp,
} from "../api";

const NAV_TAG = "__nav__"; // 喜欢栏的虚拟 tag，仅用于分组，不落库、不参与筛选

type Dialog = { mode: "create" } | { mode: "edit"; app: DesktopApp };

type Section = { tag: string; apps: DesktopApp[]; totalClicks: number };

function topApps(apps: DesktopApp[], n: number): DesktopApp[] {
  return apps
    .filter((a) => (a.clicks ?? 0) > 0)
    .sort(
      (a, b) =>
        (b.clicks ?? 0) - (a.clicks ?? 0) ||
        a.created_at.localeCompare(b.created_at),
    )
    .slice(0, n);
}

function groupSections(apps: DesktopApp[]): Section[] {
  const byTag = new Map<string, DesktopApp[]>();
  for (const app of apps) {
    for (const tag of app.tags ?? []) {
      const list = byTag.get(tag) ?? [];
      list.push(app);
      byTag.set(tag, list);
    }
  }
  return [...byTag.entries()]
    .map(([tag, list]) => ({
      tag,
      apps: [...list].sort(
        (a, b) => b.clicks - a.clicks || a.name.localeCompare(b.name),
      ),
      totalClicks: list.reduce((sum, a) => sum + a.clicks, 0),
    }))
    .sort(
      (x, y) =>
        y.totalClicks - x.totalClicks || x.tag.localeCompare(y.tag),
    );
}

const TAG_MAX_LEN = 32;
const TAGS_PER_APP_MAX = 10;

function firstGlyph(name: string): string {
  const ch = Array.from(name.trim())[0];
  return ch ? ch.toUpperCase() : "?";
}

function openApp(url: string) {
  window.open(url, "_blank", "noopener,noreferrer");
}

function tryAddTag(tags: string[], raw: string): { tags: string[]; error: string | null } {
  const tag = raw.trim();
  if (!tag) {
    return { tags, error: null };
  }
  if (Array.from(tag).length > TAG_MAX_LEN) {
    return { tags, error: `tag must be 1 to ${TAG_MAX_LEN} characters` };
  }
  if (tags.includes(tag)) {
    return { tags, error: null };
  }
  if (tags.length >= TAGS_PER_APP_MAX) {
    return { tags, error: `app tag limit is ${TAGS_PER_APP_MAX}` };
  }
  return { tags: [...tags, tag], error: null };
}

export function App() {
  const [apps, setApps] = useState<DesktopApp[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [dialog, setDialog] = useState<Dialog | null>(null);
  const [name, setName] = useState("");
  const [url, setUrl] = useState("");
  const [tags, setTags] = useState<string[]>([]);
  const [tagDraft, setTagDraft] = useState("");
  const [tagError, setTagError] = useState<string | null>(null);
  const [now, setNow] = useState(() => new Date());
  const [selectedTags, setSelectedTags] = useState<string[]>([]);

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

  const catalogTags = useMemo(() => {
    const uniq = new Set<string>();
    for (const app of apps) {
      for (const tag of app.tags ?? []) {
        uniq.add(tag);
      }
    }
    return [...uniq].sort((a, b) => a.localeCompare(b));
  }, [apps]);

  const visibleApps = useMemo(() => {
    if (selectedTags.length === 0) {
      return apps;
    }
    return apps.filter((app) => selectedTags.every((tag) => (app.tags ?? []).includes(tag)));
  }, [apps, selectedTags]);

  function toggleFilterTag(tag: string) {
    setSelectedTags((cur) => (cur.includes(tag) ? cur.filter((t) => t !== tag) : [...cur, tag]));
  }

  function asyncOpen(app: DesktopApp) {
    openApp(app.url);
    void recordAppClick(app.app_id)
      .then(load)
      .catch(() => {
        // 计数失败不影响打开，不弹全局错误。
      });
  }

  function openCreate() {
    setName("");
    setUrl("http://");
    setTags([]);
    setTagDraft("");
    setTagError(null);
    setDialog({ mode: "create" });
  }

  function openEdit(app: DesktopApp) {
    setName(app.name);
    setUrl(app.url);
    setTags(app.tags ?? []);
    setTagDraft("");
    setTagError(null);
    setDialog({ mode: "edit", app });
  }

  function commitTagDraft(): string[] | null {
    const result = tryAddTag(tags, tagDraft);
    if (result.error) {
      setTagError(result.error);
      return null;
    }
    setTags(result.tags);
    setTagDraft("");
    setTagError(null);
    return result.tags;
  }

  async function onSubmit(ev: FormEvent) {
    ev.preventDefault();
    try {
      let nextTags = tags;
      if (tagDraft.trim()) {
        const committed = commitTagDraft();
        if (!committed) return;
        nextTags = committed;
      }
      if (dialog?.mode === "edit") {
        await updateApp(dialog.app.app_id, name, url, nextTags);
      } else {
        await createApp(name, url, nextTags);
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

  const navApps = useMemo(() => topApps(visibleApps, 8), [visibleApps]);
  const sections = useMemo(() => groupSections(visibleApps), [visibleApps]);
  const untagged = useMemo(
    () => visibleApps.filter((a) => (a.tags ?? []).length === 0),
    [visibleApps],
  );

  function renderTile(app: DesktopApp) {
    return (
      <div key={app.app_id}>
        <button type="button" className="tile" onClick={() => asyncOpen(app)}>
          <div className="glyph">{firstGlyph(app.name)}</div>
          <div className="label">{app.name}</div>
        </button>
        {app.tags.length > 0 ? (
          <div className="tile-tags">
            {app.tags.map((tag) => (
              <span key={tag} className="chip chip-static">
                {tag}
              </span>
            ))}
          </div>
        ) : null}
        <div className="actions">
          <span className="clicks" title="打开次数">{app.clicks}</span>
          <button type="button" onClick={() => openEdit(app)}>
            编辑
          </button>
          <button type="button" onClick={() => void onDelete(app)}>
            删除
          </button>
        </div>
      </div>
    );
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
      <div className="filter-bar" role="toolbar" aria-label="Tag 筛选">
        <button
          type="button"
          className={selectedTags.length === 0 ? "on" : ""}
          aria-pressed={selectedTags.length === 0}
          onClick={() => setSelectedTags([])}
        >
          全部
        </button>
        {catalogTags.map((tag) => {
          const on = selectedTags.includes(tag);
          return (
            <button
              key={tag}
              type="button"
              className={on ? "on" : ""}
              aria-pressed={on}
              onClick={() => toggleFilterTag(tag)}
            >
              {tag}
            </button>
          );
        })}
      </div>
      <section className="nav-row" aria-label="喜欢">
        <h2 className="section-title">喜欢</h2>
        <div className="grid">
          {navApps.map(renderTile)}
        </div>
      </section>
      {sections.map((section) => (
        <section key={section.tag} className="tag-section" aria-label={section.tag}>
          <h2 className="section-title">
            {section.tag}
            <span className="section-clicks" title="组内打开次数合计">{section.totalClicks}</span>
          </h2>
          <div className="grid">
            {section.apps.map(renderTile)}
          </div>
        </section>
      ))}
      {untagged.length > 0 ? (
        <section className="tag-section" aria-label="未分组">
          <h2 className="section-title">未分组</h2>
          <div className="grid">
            {untagged.map(renderTile)}
          </div>
        </section>
      ) : null}
      <button type="button" className="tile add" onClick={openCreate}>
        <div className="glyph">+</div>
        <div className="label">添加 App</div>
      </button>
      {apps.length === 0 && !error ? (
        <div className="empty">目录是空的。把 GSE 或其他页面加进来。</div>
      ) : null}
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
            <label>
              Tag
              <div className="tag-editor">
                {tags.map((tag) => (
                  <span key={tag} className="chip">
                    {tag}
                    <button
                      type="button"
                      aria-label={`移除 ${tag}`}
                      onClick={() => {
                        setTags(tags.filter((t) => t !== tag));
                        setTagError(null);
                      }}
                    >
                      x
                    </button>
                  </span>
                ))}
                <input
                  value={tagDraft}
                  onChange={(e) => setTagDraft(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      e.preventDefault();
                      commitTagDraft();
                    }
                  }}
                  placeholder="回车添加"
                  maxLength={TAG_MAX_LEN}
                />
              </div>
            </label>
            {tagError ? <div className="tag-error">{tagError}</div> : null}
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
