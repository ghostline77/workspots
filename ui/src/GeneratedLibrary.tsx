import { useEffect, useMemo, useRef, useState } from "react";
import { AppShell, Badge, Button, Field, Input, Panel, Select } from "@ghostline/ui-core";
import { Viewport, type ViewportHandle, type ViewportSource } from "@ghostline/ui-core/viewport";
import { nativeMaterials } from "@ghostline/ui-core/viewport";
import { SkinnedMesh, type Object3D } from "three";
import { GLTFLoader } from "three/addons/loaders/GLTFLoader.js";
import { generatedMotion } from "./generatedMotion";
import { disposeCharacter, nativeGeneratedClip } from "./nativeGeneratedClip";
import { nativeSkinning } from "./nativeSkinning";

type Motion = { id: string; prompt: string; frames: number; fps: number; created: number;
  manifest?: any; bvh?: string; npz?: string; provenance?: string };
async function request(path: string, body?: unknown, signal?: AbortSignal) {
  const response = await fetch(path, body ? { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body), signal } : { signal });
  const value = await response.json();
  if (!response.ok) throw new Error(value.error || response.statusText);
  return value;
}
function prepareNpc(object: Object3D) {
  nativeSkinning(object); nativeMaterials(object);
  return () => { object.traverse(node => { if (node instanceof SkinnedMesh) node.skeleton.dispose(); }); };
}
async function loadCharacter(profile: string, motion: string, handPose:string, signal: AbortSignal) {
  let job = await request("/api/generated/character", { profile }, signal);
  while (["queued", "preparing"].includes(job.state)) {
    await new Promise<void>((resolve, reject) => {
      const abort = () => { clearTimeout(timer); reject(new DOMException("Aborted", "AbortError")); };
      const timer = setTimeout(() => { signal.removeEventListener("abort", abort); resolve(); }, 1200);
      signal.addEventListener("abort", abort, { once: true });
    });
    job = await request("/api/generated/character/" + job.key, undefined, signal);
  }
  if (job.state !== "ready") throw new Error(job.error || "Character preparation failed.");
  const baked = await request("/api/generated/retarget", { motion, character:job.key, hand_pose:handPose }, signal);
  const [response, clip] = await Promise.all([fetch(job.scene, { signal }), request(baked.clip, undefined, signal)]);
  if (!response.ok) throw new Error("Could not load the character scene.");
  const gltf = await new GLTFLoader().parseAsync(await response.arrayBuffer(), new URL(".", new URL(job.scene, location.href)).href);
  if (signal.aborted) { disposeCharacter(gltf.scene); throw new DOMException("Aborted", "AbortError"); }
  return { gltf, clip };
}
function SourcePreview({ motion, target, handPose }: { motion: Motion; target: string | null; handPose:string }) {
  const [source, setSource] = useState<ViewportSource>();
  const [error, setError] = useState("");
  const [playing, setPlaying] = useState(false);
  const [frame, setFrame] = useState(0);
  const viewport = useRef<ViewportHandle>(null);
  const bounds = useRef<ReturnType<typeof generatedMotion>["bounds"]>(undefined);
  useEffect(() => {
    const abort = new AbortController();
    let owned: ReturnType<typeof generatedMotion> | ReturnType<typeof nativeGeneratedClip> | undefined;
    setSource(undefined); setError(""); setPlaying(false);
    if (target === null) return;
    (async () => {
      if (target) {
        const character = await loadCharacter(target, motion.id, handPose, abort.signal);
        try {
          if (abort.signal.aborted) { disposeCharacter(character.gltf.scene); return; }
          owned = nativeGeneratedClip(character.gltf, character.clip);
        } catch (cause) { disposeCharacter(character.gltf.scene); throw cause; }
      } else {
        const response = await fetch(motion.bvh!, { signal:abort.signal });
        if (!response.ok) throw new Error("Could not load the imported animation.");
        const text = await response.text();
        if (abort.signal.aborted) return;
        owned = generatedMotion(text);
      }
      bounds.current = owned.bounds;
      setSource({ kind:"object", object:owned.object, animations:owned.animations });
    })().catch(cause => { if (!abort.signal.aborted) setError(cause.message); });
    return () => { abort.abort(); owned?.dispose(); };
  }, [motion.bvh, target, handPose]);
  return <section className="generated-preview">
    <div className="interactive-actions"><Button disabled={!source} onClick={() => {
      viewport.current?.playAnimation(playing ? null : "Generated motion"); setPlaying(!playing);
    }}>{playing ? "Stop animation" : "Play animation"}</Button><Button disabled={!source} onClick={() => {
      const box=bounds.current;if(box) viewport.current?.focus([box.center[0],box.center[1]+box.radius*.4,box.center[2]],box.radius*.38);
    }}>Upper body</Button><span className="muted">{motion.frames} frames · {motion.fps} FPS · {target ? "Retargeted NPC preview" : "SOMA source skeleton"}</span></div>
    <Field label={`Inspect frame ${frame} / ${motion.frames - 1}`}><input aria-label="Inspect animation frame" type="range" min={0} max={motion.frames - 1} step={1} value={frame} disabled={!source} onChange={event => {
      const frame = Number(event.target.value); setFrame(frame); setPlaying(false);
      viewport.current?.seekAnimation("Generated motion", frame / motion.fps);
    }} /></Field>
    {error && <div className="error" role="alert">{error}</div>}
    <Viewport ref={viewport} source={source} label="Generated motion viewport" className="generated-viewport" materialAdapter={prepareNpc}
      onLoad={() => { if (bounds.current) viewport.current?.focus(bounds.current.center, bounds.current.radius); viewport.current?.playAnimation("Generated motion"); setPlaying(true); }}
      onError={cause => setError(cause.message)}
      overlay={!source && !error ? <div className="interactive-help">{target ? "Preparing the NPC and retargeted motion…" : "Loading motion…"}</div> : undefined} />
    <p className="muted">{target ? "SOMA motion adapted to this NPC’s rest pose and proportions. Game IK, facial animation and cloth simulation are omitted." : "Generated source motion on a SOMA mannequin."} Drag to orbit · right-drag to pan · scroll to zoom.</p>
  </section>;
}

export function GeneratedLibrary({ selected }: { selected: string }) {
  const [items, setItems] = useState<Motion[]>([]);
  const [motion, setMotion] = useState<Motion | null>(null);
  const [query, setQuery] = useState("");
  const [path, setPath] = useState("");
  const [showImport, setShowImport] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [characters, setCharacters] = useState<{ id:string; name:string; rig:string }[]>([]);
  const [target, setTarget] = useState<string | null>(null);
  const [handPose,setHandPose]=useState(localStorage.getItem("workspots:handpose:"+selected)||"generated");
  useEffect(()=>setHandPose(localStorage.getItem("workspots:handpose:"+selected)||"generated"),[selected]);
  useEffect(() => { request("/api/generated/characters").then(value => { setCharacters(value.items); setTarget(value.items[0]?.id || ""); }).catch(cause => { setError(cause.message); setTarget(""); }); }, []);
  useEffect(() => { document.body.classList.add("gl-root"); request("/api/generated").then(value => setItems(value.items)).catch(cause => setError(cause.message)); }, []);
  useEffect(() => {
    let cancelled = false; setMotion(null);
    if (selected) request("/api/generated/" + encodeURIComponent(selected)).then(value => { if (!cancelled) setMotion(value); }).catch(cause => { if (!cancelled) setError(cause.message); });
    return () => { cancelled = true; };
  }, [selected]);
  const visible = useMemo(() => items.filter(item => item.prompt.toLowerCase().includes(query.toLowerCase())), [items, query]);
  return <AppShell product="Workspots" project={<><strong>Motion library</strong><span className="context">Generated motions · Kimodo</span></>}
    actions={<><Button onClick={() => { location.hash = ""; }}>Native library</Button><Button onClick={() => setShowImport(!showImport)}>Import motion</Button></>}>
    {error && <div className="error" role="alert">{error}<Button variant="ghost" onClick={() => setError("")}>Dismiss</Button></div>}
    {showImport && <Panel className="generated-import"><h2>Import a generated motion package</h2><p className="muted">Choose the local motion.json package produced by the Kimodo generator.</p>
      <form onSubmit={async event => { event.preventDefault(); setBusy(true); setError(""); try {
        const imported = await request("/api/generated/import", { path });
        setItems((await request("/api/generated")).items); setShowImport(false); location.hash = "generated/" + imported.id;
      } catch (cause: any) { setError(cause.message); } finally { setBusy(false); } }}>
        <Field label="Motion package path"><Input value={path} onChange={event => setPath(event.target.value)} placeholder="Path to motion.json" required /></Field>
        <Button type="submit" disabled={busy || !path.trim()}>{busy ? "Importing…" : "Import package"}</Button>
      </form></Panel>}
    <main className="workspace"><aside className="library"><div className="library-top"><h1>Generated motions</h1><Field label="Search"><Input value={query} onChange={event => setQuery(event.target.value)} /></Field><span className="muted">{visible.length} motions</span></div>
      <div className="results" aria-label="Generated motion results">{visible.map(item => <button key={item.id} className={"result " + (selected === item.id ? "selected" : "")} onClick={() => { location.hash = "generated/" + item.id; }}>
        <strong className="motion-name">{item.prompt}</strong><span className="result-meta">{(item.frames / item.fps).toFixed(1)}s · SOMA <Badge>Unreviewed</Badge></span></button>)}{!visible.length && <p className="empty">Import a Kimodo motion package to start previewing.</p>}</div></aside>
      <section className="detail" aria-label="Generated motion details">{motion ? <>
        <header className="detail-header"><span className="eyebrow">KIMODO · SOURCE MOTION</span><h1>{motion.prompt}</h1><div className="tabs"><Badge tone="warning">Unreviewed generation</Badge><span className="muted">{(motion.frames / motion.fps).toFixed(1)} seconds · seed {motion.manifest?.seed}</span></div></header>
        <Field label="Preview character"><Select aria-label="Preview character" value={target || ""} disabled={target === null} onChange={event => setTarget(event.target.value)}><option value="">SOMA source mannequin</option>{characters.map(character => <option value={character.id} key={character.id}>{character.name}</option>)}</Select></Field>
        <Field label="Hand pose"><Select aria-label="Hand pose" value={target ? handPose : "generated"} disabled={!target} onChange={event=>{setHandPose(event.target.value);localStorage.setItem("workspots:handpose:"+selected,event.target.value);}}><option value="generated">Generated motion</option><option value="open_right">Open right palm</option><option value="open_left">Open left palm</option><option value="open_palm">Open both palms</option></Select></Field>
        <SourcePreview key={motion.id + target + handPose} motion={motion} target={target} handPose={handPose} />
        <Panel><h2>Motion package</h2><p>Model: {motion.manifest?.model}. Downloads preserve the original SOMA motion.</p><div className="tabs"><a href={motion.bvh} download>Source BVH</a><a href={motion.npz} download>Source NPZ</a><a href={motion.provenance} target="_blank" rel="noreferrer">Import provenance</a></div></Panel>
        <details><summary>Generation provenance</summary><pre>{JSON.stringify(motion.manifest, null, 2)}</pre></details>
      </> : <p className="empty">{selected ? "Loading motion…" : "Select a generated motion to preview."}</p>}</section></main>
  </AppShell>;
}

