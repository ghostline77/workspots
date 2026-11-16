# Browse workspots and generate previews

## Run

Requires Rust 1.92+, Node 20+, FFmpeg/FFprobe and a native game installation. Reflection metadata is
bundled with the native helpers; the optional `schema` configuration field supplies
an advanced override.

Build the streaming renderer and UI:

```powershell
cargo install --locked --git https://github.com/ghostline77/renderer --example workspot_worker --example workspot_browser --example workspot_archive
cargo build --release --locked
```

Create `work/config.json` using `config.example.json`. Character archives are
explicit overlays; they are never installed in the game. Supply the workspot
catalogue directory containing `full-batch/plan.json`, `workspots.sqlite3`,
`caption-results/caption-search.sqlite3`, and original `previews/`.

```powershell
./target/release/ghostline-workspots.exe --catalogue PATH_TO_CATALOGUE --config work/config.json --bulk
```

Open <http://127.0.0.1:8820/>. Omit `--bulk` for on-demand-only usage. Repeating
`--bulk` preserves completed jobs. The service binds its port before opening the
queue, preventing another instance on that port from resetting active jobs.
Use one service instance per work directory. Stop the service before changing
renderer binaries, catalogue plans or configuration.

Start with `--paused` to browse without consuming render jobs. `--no-workers`
also disables CPU preview preparation. `GET /api/status` reports service and
queue state; `POST /api/shutdown` stops the service gracefully. SQLite with FTS5
is bundled with the Rust service.

## Browse and render

- Search animation names, caption text and source workspots. Both motion and
  workspot lists are paginated; caption bodies load for one selected motion.
- Select an animation to inspect its caption, full source-use mappings, original
  video and completed NPC variants. Captions retain their original source-rig
  provenance and review status; new NPC videos do not imply new caption evidence.
- **Interactive 3D → Load 3D animation** prepares a cached animated GLB for the
  selected character. Native decoding and calibrated skin/animation export run
  in a bounded CPU-only Rust process. Three.js in the shared `ui-core` viewport
  performs live skeletal playback and rendering, including orbit, pan and zoom.
  Both sets of four native influences are retained in GPU skinning, bounds and
  picking. Playback interpolates 30 sampled poses per second for up to eight
  seconds. Play/Stop and camera presets work locally without preparing another
  scene or queuing a video. Character orientation is aligned to the viewport's
  front view. Initial framing covers nine sampled poses across the motion; Fit
  all and camera presets recompute bounds for the current skeletal pose.
  Materials remain a browser PBR approximation.
- **Rendered video** keeps the source-rig and completed native NPC videos
  available separately. The interactive preview does not alter the bulk queue.
- Register a provided character by uploading a built `.archive` or entering its
  local path. The picker discovers entity templates and authored appearances,
  resolves the compatible male/female rig and reuses a tested assembly recipe
  when the archive hash matches. The manual form supports vanilla entities and
  explicit entity/appearance inputs. Vanilla entities need no overlay.
  An optional authoring manifest is rejected if `refit_clothing` is enabled.
  This service never builds clothing or calls the refitter. Existing archives
  are used as supplied, including any prior authoring choices.
- **Render next** promotes a queued animation above bulk work after the current
  clip finishes. A character switch reloads its assembly. Requests are deduplicated
  by clip and character/settings identity. Failed clips can be retried explicitly.
- **Pause queue** finishes the current clip, then releases GPU resources. Resume
  continues pending work. Errors that terminate the native process pause the
  queue rather than failing every remaining clip.

## Outputs and provenance

`work/workspots.sqlite3` stores canonical motions, complete source-use mappings,
character profiles and durable render jobs. Original databases are attached read
only; original captions and human review stores are never modified.

`work/renders/<character-identity>/<clip-id>/` contains `preview.mp4`, `poster.png`,
`receipt.json` and native sequence metadata. Profile identity includes archive
hashes, entity, appearance, renderer executable hash and render settings. The
receipt retains the native source identity and sequence build/manifest identity,
video hash, frame count, FPS and source-plan hash. Interrupted jobs resume;
missing or changed completed videos are requeued at service startup.

Rendering samples native animation time at **30 FPS**, 512×512, for up to eight
seconds. Videos are encoded as H.264. Temporary frame PNGs are removed only after
FPS/frame-count checks, a full decode check and an atomic video/receipt write.
Each clip receives a camera fitted to nine sampled poses. Root motion is preserved.
One retained GPU scene handles consecutive clips for a character.

`work/worker.log` and `work/queue.log` retain diagnostics. When the queue is
exhausted, `work/completion.json` records counts and GPU worker exit. No cloud
resources are used or provisioned.

Interactive preparations use `work/browser/<cache-key>/scene.glb` and a source/hash
receipt. Requests are deduplicated and limited to four pending scenes. The CPU
preparer exits after ten idle seconds and recycles after four requests. The bulk
GPU worker recycles after eight clips to bound retained decoded-resource memory.
Completed videos survive recycling and service restarts. Browser errors remain
separate from native video job results. `work/browser-worker.log` records export
diagnostics.

Coverage is explicit: resolved source motions can be rendered; unresolved
registrations and skipped workspots remain gaps. A failed clip is never complete.
These are individual animation previews, not full workspot-tree playback or game
runtime validation. Furniture, props, partner actors, runtime IK, facial animation
and cloth simulation are omitted. An explicitly configured rigid hair component
follows the head without simulation.

## Generated motion previews

The Kimodo library imports independent `motion.json` packages, retaining their
original SOMA BVH/NPZ and generation evidence under `work/generated/`.
Choose Patch, Velvet, or the SOMA source mannequin in **Preview character**.

NPC retargeting runs in Rust through `POST /api/generated/retarget` with a motion
ID, prepared character key and optional hand pose. It maps SOMA motion onto the native rest pose,
preserves native limb lengths, scales hip displacement to character height, and
synchronizes detached facial rig roots with their body attachments. Neck/head
motion preserves each NPC's neutral orientation. All linked skeletons receive
the corresponding motion, including the head carrying rigid preview hair.
Linked joints share the body's calibration even when a facial rig omits the
child limb. The torso and clavicles retain native neutral curvature. Arms and
fingers derive orientation from posed bend/palm frames and native skin-bone axes,
avoiding arbitrary axial source spin. Chest support bones retain their authored
local transforms; no procedural chest compensation is applied.

**Hand pose** defaults to the generated motion. Explicit open-right, open-left
and open-both-palm options edit finger motion in Rust while preserving the source
BVH/NPZ. Opening one hand does not alter the other hand or any body joint. The
choice is retained per motion in the browser and recorded in retarget provenance.

The result contains 30 FPS local quaternion/position tracks indexed by GLB node,
native rest transforms and bounds. Three.js binds these tracks to loaded objects
and plays them; it performs no NPC retargeting. Cached results are identified by
algorithm version, hand-pose choice and BVH, character GLB and rest-pose hashes. This is a preview
conversion, without game IK/cloth/facial animation or `.anims` export; generated
motions remain unreviewed and are not runtime verified.
The frame inspection slider pauses on a selected 30 FPS sample in both generated
and native animation previews, allowing repeatable pose comparisons.

## Check

```powershell
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --check
npm --prefix ui run typecheck
npm --prefix ui test
```

Point `worker` in your config to the installed workspot_worker executable. Keep the three workers in the same directory.
