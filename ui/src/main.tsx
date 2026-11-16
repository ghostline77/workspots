import { useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import {
  AppShell,
  Badge,
  Button,
  Field,
  Input,
  Panel,
  Select,
} from "@ghostline/ui-core";
import "@ghostline/ui-core/styles.css";
import "./style.css";
import { BrowserPreview } from "./BrowserPreview";
import { CharacterPicker } from "./CharacterPicker";
import { GeneratedLibrary } from "./GeneratedLibrary";

type Row = {
  id: string;
  animation: string;
  rig: string;
  uses: number;
  state: string;
  depot_path?: string;
  label?: string;
  animation_count?: number;
  resolved_clips?: number;
};
type Profile = { id: string; name: string; rig: string };
type Render = {
  id: number;
  state: string;
  character: string;
  profile: string;
  video: string | null;
  error: string | null;
  receipt: any;
};
type Detail = {
  id: string;
  animation: string;
  rig: string;
  caption: any;
  review_status: string;
  original_video: string | null;
  renders: Render[];
  plan: any;
};
type Status = {
  counts: Record<string, number>;
  coverage: Record<string, number>;
  profiles: Profile[];
  seconds_per_clip: number | null;
  eta_seconds: number | null;
  current: any;
  error: string | null;
  recent: any[];
  paused: boolean;
};
async function api(path: string, body?: any) {
  const r = await fetch(
    path,
    body
      ? {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(body),
        }
      : undefined,
  );
  const value = await r.json();
  if (!r.ok) throw new Error(value.error || r.statusText);
  return value;
}
function duration(seconds: number | null) {
  if (seconds === null) return "Estimating…";
  return seconds > 3600
    ? `${(seconds / 3600).toFixed(1)}h`
    : `${Math.ceil(seconds / 60)}m`;
}
function App() {
  const [mode, setMode] = useState<"3d" | "video">("3d");
  const [status, setStatus] = useState<Status | null>(null),
    [query, setQuery] = useState(""),
    [rig, setRig] = useState(""),
    [state, setState] = useState(""),
    [kind, setKind] = useState("clips"),
    [page, setPage] = useState(0);
  const [rows, setRows] = useState<Row[]>([]),
    [loadingRows, setLoadingRows] = useState(true),
    [total, setTotal] = useState(0),
    [selected, setSelected] = useState(location.hash.slice(1)),
    [detail, setDetail] = useState<Detail | null>(null);
  const [profile, setProfile] = useState(""),
    [video, setVideo] = useState(""),
    [error, setError] = useState(""),
    [busy, setBusy] = useState(false),
    [register, setRegister] = useState(false);
  useEffect(() => {
    const changed = () => setSelected(location.hash.slice(1));
    window.addEventListener("hashchange", changed);
    return () => window.removeEventListener("hashchange", changed);
  }, []);
  const refresh = () =>
    api("/api/status")
      .then(setStatus)
      .catch((e) => setError(e.message));
  useEffect(() => {
    document.body.classList.add("gl-root");
    refresh();
    const timer = setInterval(refresh, 4000);
    return () => clearInterval(timer);
  }, []);
  useEffect(() => {
    let cancelled = false;
    setLoadingRows(true);
    const timer = setTimeout(
      () =>
        api(
          `/api/clips?${new URLSearchParams({ q: query, rig, state, page: String(page), kind })}`,
        )
          .then((v) => {
            if (!cancelled) {
              setRows(v.items);
              setTotal(v.total);
              setLoadingRows(false);
            }
          })
          .catch((e) => {
            if (!cancelled) {
              setError(e.message);
              setLoadingRows(false);
            }
          }),
      180,
    );
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [
    query,
    rig,
    state,
    page,
    kind,
    status?.counts.complete,
    status?.counts.failed,
  ]);
  useEffect(() => {
    if (!selected) {
      setDetail(null);
      return;
    }
    let cancelled = false;
    setDetail(null);
    api("/api/clips/" + selected)
      .then((d) => {
        if (!cancelled) setDetail(d);
      })
      .catch((e) => {
        if (!cancelled) setError(e.message);
      });
    location.hash = selected;
    return () => {
      cancelled = true;
    };
  }, [selected]);
  useEffect(() => {
    if (!selected) return;
    let cancelled = false;
    api("/api/clips/" + selected)
      .then((d) => {
        if (!cancelled) setDetail(d);
      })
      .catch((e) => {
        if (!cancelled) setError(e.message);
      });
    return () => {
      cancelled = true;
    };
  }, [status?.counts.complete, status?.counts.failed]);
  useEffect(() => {
    if (!detail || !status) return;
    const compatible = status.profiles.filter((p) => p.rig === detail.rig);
    if (!compatible.some((p) => p.id === profile))
      setProfile(compatible[0]?.id || "");
  }, [detail?.id, status?.profiles.length]);
  useEffect(() => {
    if (!detail) return;
    const latest =
      detail.renders.find(
        (r) => r.state === "complete" && r.profile === profile,
      ) || detail.renders.find((r) => r.state === "complete");
    setVideo(latest?.video || detail.original_video || "");
  }, [detail?.id, detail?.renders.map((r) => r.state).join(","), profile]);
  async function render() {
    setMode("video");
    if (!detail || !profile) return;
    const ready = detail.renders.find(
      (r) => r.profile === profile && r.state === "complete",
    );
    if (ready?.video) {
      setVideo(ready.video);
      return;
    }
    setBusy(true);
    setError("");
    try {
      await api("/api/render", { clip: detail.id, profile, retry: true });
      setDetail(await api("/api/clips/" + detail.id));
      refresh();
    } catch (e: any) {
      setError(e.message);
    } finally {
      setBusy(false);
    }
  }
  const complete = status?.counts.complete || 0,
    failed = status?.counts.failed || 0,
    queued = status?.counts.queued || 0;
  const active = detail?.renders.find((r) => r.profile === profile);
  // A view switch renders before the asynchronous response replaces the rows.
  const displayedRows = rows.filter((row) =>
    kind === "clips"
      ? typeof row.animation === "string"
      : typeof row.depot_path === "string",
  );
  return (
    <AppShell
      product="Workspots"
      project={
        <>
          <strong>Motion library</strong>
          <span className="context">Native NPC previews · 30 FPS</span>
        </>
      }
      actions={
        <>
          <Badge
            tone={status?.error ? "danger" : queued ? "warning" : "success"}
          >
            {status?.error
              ? "Queue needs attention"
              : status?.paused
                ? "Queue paused"
                : queued
                  ? "Rendering library"
                  : "Library ready"}
          </Badge>
          <Button
            onClick={async () => {
              try {
                await api("/api/queue", {
                  action: status?.paused || status?.error ? "resume" : "pause",
                });
                refresh();
              } catch (e: any) {
                setError(e.message);
              }
            }}
          >
            {status?.paused || status?.error ? "Resume queue" : "Pause queue"}
          </Button>
          <Button onClick={() => setRegister((v) => !v)}>Add character</Button>
          <Button onClick={() => { location.hash = "generated"; }}>Kimodo motions</Button>
        </>
      }
    >
      <div className="summary">
        <div>
          <strong>{complete.toLocaleString()}</strong>
          <span>NPC renders ready</span>
        </div>
        <div>
          <strong>{queued.toLocaleString()}</strong>
          <span>
            queued · {duration(status?.eta_seconds ?? null)} remaining
          </span>
        </div>
        <div>
          <strong>{status?.seconds_per_clip?.toFixed(1) || "—"}s</strong>
          <span>per clip · single GPU worker</span>
        </div>
        <div>
          <strong>{failed}</strong>
          <span>render failures</span>
        </div>
        <div>
          <strong>{status?.coverage.unresolved.toLocaleString() || "—"}</strong>
          <span>unresolved registrations</span>
        </div>
      </div>
      {(error || status?.error) && (
        <div className="error" role="alert">
          {error || status?.error}
          <Button variant="ghost" onClick={() => setError("")}>
            Dismiss
          </Button>
        </div>
      )}
      {register && (
        <CharacterPicker
          onClose={() => setRegister(false)}
          onRegistered={async (id, sourceRig) => {
            setProfile(id);
            await refresh();
            setRegister(false);
            if (detail && detail.rig !== sourceRig) {
              setRig(sourceRig);
              setQuery("");
              setPage(0);
              setSelected("");
              location.hash = "";
            }
          }}
        />
      )}
      <main className="workspace">
        <aside className="library">
          <div className="library-top">
            <h1>Browse the database</h1>
            <div className="tabs">
              <Button
                variant={kind === "clips" ? "primary" : "default"}
                onClick={() => {
                  setRows([]);
                  setKind("clips");
                  setPage(0);
                  setQuery("");
                }}
              >
                Motions
              </Button>
              <Button
                variant={kind === "workspots" ? "primary" : "default"}
                onClick={() => {
                  setRows([]);
                  setKind("workspots");
                  setPage(0);
                  setQuery("");
                }}
              >
                Workspots
              </Button>
            </div>
            <Field label="Search">
              <Input
                type="search"
                placeholder="Phone, surrender, sit…"
                value={query}
                onChange={(e) => {
                  setQuery(e.target.value);
                  setPage(0);
                }}
              />
            </Field>
            {kind === "clips" && (
              <div className="filters">
                <Select
                  aria-label="Rig filter"
                  value={rig}
                  onChange={(e) => {
                    setRig(e.target.value);
                    setPage(0);
                  }}
                >
                  <option value="">All rigs</option>
                  <option value="male">Male</option>
                  <option value="female">Female</option>
                </Select>
                <Select
                  aria-label="Render status filter"
                  value={state}
                  onChange={(e) => {
                    setState(e.target.value);
                    setPage(0);
                  }}
                >
                  <option value="">All states</option>
                  <option value="complete">NPC ready</option>
                  <option value="queued">Queued</option>
                  <option value="failed">Failed</option>
                  <option value="unrendered">No NPC render</option>
                </Select>
              </div>
            )}
            <span className="muted">
              {total.toLocaleString()}{" "}
              {kind === "clips" ? "resolved motions" : "workspots"}
            </span>
          </div>
          <div className="results" aria-label="Search results">
            {displayedRows.map((row) =>
              kind === "clips" ? (
                <button
                  className={
                    "result " + (selected === row.id ? "selected" : "")
                  }
                  key={row.id}
                  onClick={() => setSelected(row.id)}
                >
                  <span className="motion-name">
                    {row.animation.replaceAll("__", " · ")}
                  </span>
                  <span className="result-meta">
                    <span>
                      {row.rig} · {row.uses} source uses
                    </span>
                    <Badge
                      tone={
                        row.state === "complete"
                          ? "success"
                          : row.state === "failed"
                            ? "danger"
                            : "neutral"
                      }
                    >
                      {row.state === "complete"
                        ? "NPC ready"
                        : row.state || "Not queued"}
                    </Badge>
                  </span>
                </button>
              ) : (
                <button
                  className="result"
                  key={row.depot_path}
                  onClick={() => {
                    setRows([]);
                    setKind("clips");
                    setQuery(row.depot_path || "");
                    setPage(0);
                  }}
                >
                  <span className="motion-name">
                    {row.label || row.depot_path?.split("\\").pop()}
                  </span>
                  <span className="path">{row.depot_path}</span>
                  <span className="muted">
                    {row.resolved_clips} resolved motions ·{" "}
                    {row.animation_count} animation entries
                  </span>
                </button>
              ),
            )}
            {!displayedRows.length && (
              <p className="empty">
                {loadingRows
                  ? "Loading…"
                  : kind === "clips" && query.endsWith(".workspot")
                  ? "No resolved motions for this workspot. Its registrations may be unresolved or its rig unsupported."
                  : "No matches. Try a shorter phrase or another filter."}
              </p>
            )}
          </div>
          <div className="pagination">
            <Button disabled={!page} onClick={() => setPage(page - 1)}>
              Previous
            </Button>
            <span>
              {page + 1} / {Math.max(1, Math.ceil(total / 35))}
            </span>
            <Button
              disabled={(page + 1) * 35 >= total}
              onClick={() => setPage(page + 1)}
            >
              Next
            </Button>
          </div>
        </aside>
        <section className="detail" aria-label="Motion details">
          {!detail ? (
            <div className="welcome">
              <Badge>WORKSPOT DATABASE</Badge>
              <h2>
                Find the motion.
                <br />
                See it on your character.
              </h2>
              <p>
                Search {status?.coverage.resolved.toLocaleString() || "8,707"}{" "}
                resolved motions and their source workspots. NPC previews appear
                here as the render queue completes.
              </p>
              <p className="muted">
                Captions are model suggestions from the original source-rig
                videos. They remain unreviewed and are not runtime validation.
              </p>
              <div className="coverage">
                <strong>Coverage</strong>
                <p>
                  {status?.coverage.workspots} catalogued workspots ·{" "}
                  {status?.coverage.skipped} skipped workspots ·{" "}
                  {status?.coverage.unresolved} unresolved registrations.
                </p>
              </div>
            </div>
          ) : (
            <>
              <header className="detail-header">
                <span className="eyebrow">
                  {detail.rig.toUpperCase()} MOTION · {detail.id}
                </span>
                <h2>{detail.animation.replaceAll("__", " · ")}</h2>
                <Badge tone="warning">
                  {detail.review_status === "model_suggested_unreviewed"
                    ? "Unreviewed model suggestion"
                    : detail.review_status.replaceAll("_", " ")}{" "}
                  · source-rig caption
                </Badge>
              </header>
              <div className="preview-modes">
                <Button
                  variant={mode === "3d" ? "primary" : "default"}
                  onClick={() => setMode("3d")}
                >
                  Interactive 3D
                </Button>
                <Button
                  variant={mode === "video" ? "primary" : "default"}
                  onClick={() => setMode("video")}
                >
                  Rendered video
                </Button>
              </div>
              {mode === "3d" ? (
                <BrowserPreview
                  key={detail.id + profile}
                  clip={detail.id}
                  profile={profile}
                />
              ) : (
                <div className="preview">
                  {video ? (
                    <video
                      key={video}
                      src={video}
                      controls
                      loop
                      playsInline
                      preload="metadata"
                    />
                  ) : (
                    <p>No video yet. Queue a render below.</p>
                  )}
                </div>
              )}
              <div className="preview-toolbar">
                {mode === "video" && (
                  <Field label="Video variant">
                    <Select
                      value={video}
                      onChange={(e) => setVideo(e.target.value)}
                    >
                      {detail.original_video && (
                        <option value={detail.original_video}>
                          Original source rig · 12 FPS
                        </option>
                      )}
                      {detail.renders
                        .filter((r) => r.video)
                        .map((r) => (
                          <option key={r.id} value={r.video!}>
                            {r.character} · 30 FPS
                          </option>
                        ))}
                      {!video && <option value="">No video available</option>}
                    </Select>
                  </Field>
                )}
                <Field label="Preview character">
                  <Select
                    value={profile}
                    onChange={(e) => setProfile(e.target.value)}
                  >
                    {status?.profiles
                      .filter((p) => p.rig === detail.rig)
                      .map((p) => (
                        <option key={p.id} value={p.id}>
                          {p.name}
                        </option>
                      ))}
                  </Select>
                </Field>
                <Button
                  variant="default"
                  disabled={busy || !profile || active?.state === "running"}
                  onClick={render}
                >
                  {active?.state === "complete"
                    ? "View ready video"
                    : active?.state === "running"
                      ? "Rendering…"
                      : active?.state === "queued"
                        ? "Render video next"
                        : active?.state === "failed"
                          ? "Retry video"
                          : "Queue video"}
                </Button>
              </div>
              {active?.state === "complete" && (
                <p className="muted">
                  {active.receipt?.probe.nb_read_frames} native sampled frames ·
                  30 FPS · up to 8 seconds
                </p>
              )}
              {active?.state === "queued" && (
                <p className="notice">
                  Video queued. “Render video next” moves this request ahead of
                  bulk clips after the current clip finishes.
                </p>
              )}
              {active?.error && (
                <p role="alert" className="error">
                  {active.error}
                </p>
              )}
              <div className="evidence">
                <Panel title="Observed motion">
                  {detail.caption?.observed_motion ? (
                    Object.entries(detail.caption.observed_motion).map(
                      ([key, value]) => (
                        <div className="caption-row" key={key}>
                          <dt>{key.replaceAll("_", " ")}</dt>
                          <dd>
                            {typeof value === "string"
                              ? value
                              : JSON.stringify(value)}
                          </dd>
                        </div>
                      ),
                    )
                  ) : (
                    <p>No caption available for this motion.</p>
                  )}
                </Panel>
                <Panel title="Suggested context">
                  <p>
                    {detail.caption?.inferred_context?.likely_action ||
                      "No contextual suggestion."}
                  </p>
                  <p className="muted">
                    Furniture, props, partners, cloth simulation and runtime IK
                    are omitted. Hair simulation is not included.
                  </p>
                  <details>
                    <summary>Uncertainties and provenance</summary>
                    <pre>
                      {JSON.stringify(
                        {
                          context: detail.caption?.inferred_context,
                          unverified: detail.caption?.unverified,
                          uncertainties: detail.caption?.uncertainties,
                          animset: detail.plan.resolved_animset,
                          rig: detail.plan.rig,
                        },
                        null,
                        2,
                      )}
                    </pre>
                  </details>
                </Panel>
              </div>
              <Panel
                title={`Source workspots · ${detail.plan.uses.length} uses`}
              >
                <div className="source-uses">
                  {detail.plan.uses.map((use: any, i: number) => (
                    <div key={i}>
                      <code>{use.workspot}</code>
                      <span className="muted">
                        {use.role} · entry {use.entry_id} · {use.field}
                      </span>
                    </div>
                  ))}
                </div>
              </Panel>
            </>
          )}
        </section>
      </main>
    </AppShell>
  );
}
function Root() {
  const [hash, setHash] = useState(location.hash.slice(1));
  useEffect(() => {
    const changed = () => setHash(location.hash.slice(1));
    window.addEventListener("hashchange", changed);
    return () => window.removeEventListener("hashchange", changed);
  }, []);
  return hash === "generated" || hash.startsWith("generated/")
    ? <GeneratedLibrary selected={hash.startsWith("generated/") ? hash.slice(10) : ""} />
    : <App />;
}
createRoot(document.getElementById("root")!).render(<Root />);
