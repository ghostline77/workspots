import { useEffect, useMemo, useRef, useState } from "react";
import { Button } from "@ghostline/ui-core";
import {
  Viewport,
  nativeMaterials,
  type ViewportHandle,
  type ViewportSource,
} from "@ghostline/ui-core/viewport";
import { nativeSkinning } from "./nativeSkinning";
import type { Object3D } from "three";

function prepareCharacter(object: Object3D) {
  // Native characters face -Z; the shared viewport's front camera faces +Z.
  object.rotateY(Math.PI);
  object.updateMatrixWorld(true);
  nativeSkinning(object);
  nativeMaterials(object);
}

type Preview = {
  key: string;
  state: string;
  scene: string | null;
  error: string | null;
  receipt: any;
};
export function BrowserPreview({
  clip,
  profile,
}: {
  clip: string;
  profile: string;
}) {
  const [job, setJob] = useState<Preview | null>(null),
    [error, setError] = useState(""),
    [busy, setBusy] = useState(false),
    [animation, setAnimation] = useState(""),
    [playing, setPlaying] = useState(false);
  const [revision, setRevision] = useState(0);
  const [frame, setFrame] = useState(0);
  const viewport = useRef<ViewportHandle>(null);
  const request = useRef<AbortController | null>(null);
  useEffect(() => () => request.current?.abort(), []);
  const source = useMemo<ViewportSource | undefined>(
    () => (job?.scene ? { kind: "gltf", url: job.scene } : undefined),
    [job?.scene],
  );
  useEffect(() => {
    if (!job || !["queued", "preparing"].includes(job.state)) return;
    const abort = new AbortController();
    const timer = setInterval(
      () =>
        fetch("/api/browser/" + job.key, { signal: abort.signal })
          .then(async (r) => {
            const value = await r.json();
            if (!r.ok) throw new Error(value.error);
            return value;
          })
          .then(setJob)
          .catch((e) => {
            if (!abort.signal.aborted) setError(e.message);
          }),
      1500,
    );
    return () => {
      abort.abort();
      clearInterval(timer);
    };
  }, [job?.key, job?.state]);
  async function prepare() {
    request.current?.abort();
    const abort = new AbortController();
    request.current = abort;
    setBusy(true);
    setError("");
    try {
      const response = await fetch("/api/browser", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ clip, profile, retry: true }),
        signal: abort.signal,
      });
      const value = await response.json();
      if (!response.ok) throw new Error(value.error);
      setJob(value);
      setRevision((value) => value + 1);
    } catch (e: any) {
      if (!abort.signal.aborted) setError(e.message);
    } finally {
      if (!abort.signal.aborted) setBusy(false);
    }
  }
  const pending = busy || job?.state === "queued" || job?.state === "preparing";
  return (
    <section className="interactive-preview" aria-label="Interactive animation">
      <div className="interactive-actions">
        <Button
          variant="primary"
          disabled={!profile || pending}
          onClick={prepare}
        >
          {pending
            ? "Preparing 3D animation…"
            : job?.scene
              ? "Reload 3D animation"
              : "Load 3D animation"}
        </Button>
        <Button
          disabled={!animation}
          onClick={() => {
            viewport.current?.playAnimation(playing ? null : animation);
            setPlaying(!playing);
          }}
        >
          {playing ? "Stop animation" : "Play animation"}
        </Button>
        {job?.receipt && (
          <span className="muted">
            {job.receipt.duration.toFixed(1)}s · native bone motion · 30
            samples/s
          </span>
        )}
      </div>
      {job?.receipt && <label className="muted">Inspect frame {frame}
        <input aria-label="Inspect animation frame" type="range" min={0} max={Math.max(0, Math.floor(job.receipt.duration * 30))} step={1} value={frame} disabled={!animation} onChange={event => {
          const frame = Number(event.target.value); setFrame(frame); setPlaying(false);
          viewport.current?.seekAnimation(animation, frame / 30);
        }} />
      </label>}
      {(error || job?.error) && (
        <p className="error" role="alert">
          {error || job?.error}
        </p>
      )}
      <Viewport
        key={revision}
        source={source}
        label="Animated NPC viewport"
        ref={viewport}
        grid
        materialAdapter={prepareCharacter}
        onLoad={(_, names) => {
          setAnimation(names[0] || "");
          setPlaying(Boolean(names[0]));
          const bounds = job?.receipt?.bounds;
          if (bounds)
            viewport.current?.focus(
              [-bounds.center[0], bounds.center[1], -bounds.center[2]],
              bounds.radius,
            );
          if (names[0]) viewport.current?.playAnimation(names[0]);
        }}
        onError={(cause) => setError(cause.message)}
        overlay={
          !source ? (
            <div className="interactive-help">
              <p>
                {pending
                  ? "Preparing the character and sampled bone motion. Playback runs locally once loaded."
                  : "Load this motion on the selected character to play it in 3D."}
              </p>
              <p>Drag to orbit · right-drag to pan · scroll to zoom</p>
            </div>
          ) : undefined
        }
      />
      <p className="muted">
        Interactive Three.js preview. Materials approximate the native renderer;
        furniture, props, facial animation, cloth simulation and runtime IK are
        omitted.
      </p>
    </section>
  );
}
