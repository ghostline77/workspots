import { useEffect, useRef, useState } from "react";
import { Badge, Button, Field, Input, Panel, Select } from "@ghostline/ui-core";

type Appearance = { name: string; resource_appearance: string };
type Entity = {
  resource: string;
  depot_path: string | null;
  appearances: Appearance[];
  error?: string;
};
type Catalogue = {
  archive: string;
  archive_sha256: string;
  entities: Entity[];
  warnings: string[];
  resources: number;
};
type Resolution = {
  rig: string | null;
  rig_resources: string[];
  existing_recipe?: {
    id: string;
    name: string;
    origin_anchors: string[];
    rigid_hair?: string;
  };
};
const initial = {
  name: "",
  archive: "",
  entity: "",
  appearance: "",
  rig: "male",
  project: "",
  anchors: "Component",
  hair: "",
};

export function CharacterPicker({
  onRegistered,
  onClose,
}: {
  onRegistered: (id: string, rig: string) => void | Promise<void>;
  onClose: () => void;
}) {
  const [form, setForm] = useState(initial),
    [manual, setManual] = useState(false),
    [catalogue, setCatalogue] = useState<Catalogue | null>(null),
    [resolved, setResolved] = useState<Resolution | null>(null),
    [busy, setBusy] = useState(false),
    [error, setError] = useState("");
  const controller = useRef<AbortController | null>(null);
  useEffect(() => () => controller.current?.abort(), []);
  async function post(path: string, body: any, signal: AbortSignal) {
    const response = await fetch(path, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
      signal,
    });
    const value = await response.json();
    if (!response.ok) throw new Error(value.error);
    return value;
  }
  async function run(action: (signal: AbortSignal) => Promise<void>) {
    controller.current?.abort();
    const abort = new AbortController();
    controller.current = abort;
    setBusy(true);
    setError("");
    try {
      await action(abort.signal);
    } catch (e: any) {
      if (!abort.signal.aborted) setError(e.message);
    } finally {
      if (!abort.signal.aborted) setBusy(false);
    }
  }
  async function resolve(
    catalog: Catalogue,
    entity: string,
    appearance: string,
    signal: AbortSignal,
  ) {
    setResolved(null);
    setForm((f) => ({ ...f, archive: catalog.archive, entity, appearance }));
    if (!appearance) return;
    const value: Resolution = await post(
      "/api/archive-resolve",
      {
        archive: catalog.archive,
        archive_sha256: catalog.archive_sha256,
        entity,
        appearance,
      },
      signal,
    );
    setResolved(value);
    const recipe = value.existing_recipe;
    setForm((f) => ({
      ...f,
      rig: value.rig || "",
      name:
        f.name ||
        recipe?.name ||
        entity
          .split(/[\\/]/)
          .pop()
          ?.replace(/\.ent$/i, "") ||
        "Character",
      anchors: recipe ? recipe.origin_anchors.join(", ") : f.anchors,
      hair: recipe?.rigid_hair || "",
    }));
  }
  async function discover(path: string, signal: AbortSignal) {
    setCatalogue(null);
    setResolved(null);
    const data: Catalogue = await post(
      "/api/archive-discover",
      { archive: path },
      signal,
    );
    setCatalogue(data);
    const first = data.entities.find((e) => e.appearances.length && !e.error);
    if (first)
      await resolve(data, first.resource, first.appearances[0].name, signal);
    else
      setError(
        "No authored entity appearances were found. Manual setup supports vanilla or inherited resources.",
      );
  }
  async function upload(file: File) {
    await run(async (signal) => {
      const response = await fetch(
        "/api/archive-upload?filename=" + encodeURIComponent(file.name),
        {
          method: "POST",
          headers: { "Content-Type": "application/octet-stream" },
          body: file,
          signal,
        },
      );
      const value = await response.json();
      if (!response.ok) throw new Error(value.error);
      setForm((f) => ({ ...f, archive: value.archive }));
      await discover(value.archive, signal);
    });
  }
  const entity = catalogue?.entities.find((e) => e.resource === form.entity);
  const anchors = form.anchors
    .split(",")
    .map((v) => v.trim())
    .filter(Boolean);
  const recipe = resolved?.existing_recipe;
  const existing =
    !manual &&
    recipe?.id &&
    form.name === recipe.name &&
    JSON.stringify(anchors) === JSON.stringify(recipe.origin_anchors) &&
    form.hair === (recipe.rigid_hair || "") &&
    !form.project;
  async function register() {
    await run(async (signal) => {
      if (existing) {
        const latest = await post(
          "/api/archive-discover",
          { archive: form.archive },
          signal,
        );
        if (latest.archive_sha256 !== catalogue?.archive_sha256)
          throw new Error(
            "Archive changed; inspect it again before using this character",
          );
        await onRegistered(recipe!.id, form.rig);
        return;
      }
      const value = await post(
        "/api/characters",
        {
          name: form.name,
          rig: form.rig,
          entity: form.entity,
          appearance: form.appearance,
          overlays: form.archive ? [form.archive] : [],
          archive_sha256: manual ? undefined : catalogue?.archive_sha256,
          project: form.project || undefined,
          origin_anchors: anchors,
          rigid_hair: form.hair || undefined,
        },
        signal,
      );
      await onRegistered(value.id, form.rig);
    });
  }
  return (
    <section className="character-form">
      <Panel
        title="Add a character"
        actions={<Button onClick={onClose}>Close</Button>}
      >
        <p>
          Choose a built archive to find its entities and available appearances.
          Assets stay local; clothing refitting is disabled.
        </p>
        <div className="picker-mode">
          <Button
            variant={!manual ? "primary" : "default"}
            disabled={busy}
            onClick={() => setManual(false)}
          >
            From archive
          </Button>
          <Button
            variant={manual ? "primary" : "default"}
            disabled={busy}
            onClick={() => {
              setManual(true);
              setForm((f) => ({ ...f, rig: f.rig || "male" }));
            }}
          >
            Manual / vanilla
          </Button>
        </div>
        <div className="archive-source">
          <Field label={manual ? "Archive path (optional)" : "Archive path"}>
            <Input
              value={form.archive}
              disabled={busy}
              placeholder="Choose a file below, or enter a local .archive path"
              onChange={(e) => {
                setForm({
                  ...form,
                  archive: e.target.value,
                  entity: "",
                  appearance: "",
                });
                setCatalogue(null);
                setResolved(null);
              }}
            />
          </Field>
          {!manual && (
            <>
              <Button
                disabled={busy || !form.archive}
                onClick={() => run((signal) => discover(form.archive, signal))}
              >
                {busy ? "Inspecting…" : "Find characters"}
              </Button>
              <Field label="Choose archive file">
                <input
                  type="file"
                  accept=".archive"
                  disabled={busy}
                  onChange={(e) => {
                    const file = e.target.files?.[0];
                    if (file) void upload(file);
                  }}
                />
              </Field>
            </>
          )}
        </div>
        {!manual && catalogue && (
          <>
            <p>
              {catalogue.entities.length} entities found in{" "}
              {catalogue.resources} resources.
            </p>
            {catalogue.warnings.map((w, i) => (
              <p className="notice" key={i}>
                {w}
              </p>
            ))}
          </>
        )}
        <div className="form-grid">
          <Field label="Character name">
            <Input
              value={form.name}
              onChange={(e) => setForm({ ...form, name: e.target.value })}
            />
          </Field>
          {manual ? (
            <>
              <Field label="Entity depot path (.ent) or native hash">
                <Input
                  value={form.entity}
                  onChange={(e) => setForm({ ...form, entity: e.target.value })}
                />
              </Field>
              <Field label="Appearance name">
                <Input
                  value={form.appearance}
                  onChange={(e) =>
                    setForm({ ...form, appearance: e.target.value })
                  }
                />
              </Field>
              <Field label="Source rig">
                <Select
                  value={form.rig || "male"}
                  onChange={(e) => setForm({ ...form, rig: e.target.value })}
                >
                  <option value="male">Male</option>
                  <option value="female">Female</option>
                </Select>
              </Field>
            </>
          ) : (
            <>
              <Field label="Entity">
                <Select
                  value={form.entity}
                  disabled={busy || !catalogue}
                  onChange={(e) => {
                    const entry = catalogue!.entities.find(
                      (v) => v.resource === e.target.value,
                    )!;
                    void run((signal) =>
                      resolve(
                        catalogue!,
                        entry.resource,
                        entry.appearances[0]?.name || "",
                        signal,
                      ),
                    );
                  }}
                >
                  <option value="">Choose an entity</option>
                  {catalogue?.entities.map((e) => (
                    <option
                      key={e.resource}
                      value={e.resource}
                      disabled={Boolean(e.error) || !e.appearances.length}
                    >
                      {e.depot_path || `Entity hash ${e.resource}`}
                      {!e.appearances.length
                        ? " · no authored appearances"
                        : ""}
                    </option>
                  ))}
                </Select>
              </Field>
              <Field label="Appearance">
                <Select
                  value={form.appearance}
                  disabled={busy || !entity}
                  onChange={(e) =>
                    void run((signal) =>
                      resolve(catalogue!, form.entity, e.target.value, signal),
                    )
                  }
                >
                  <option value="">Choose an appearance</option>
                  {entity?.appearances.map((a) => (
                    <option key={a.name} value={a.name}>
                      {a.name}
                    </option>
                  ))}
                </Select>
              </Field>
              <div className="detected-rig">
                <span className="muted">Detected source rig</span>
                <Badge tone={resolved?.rig ? "success" : "neutral"}>
                  {resolved?.rig || (busy ? "Checking…" : "Not resolved")}
                </Badge>
              </div>
            </>
          )}
        </div>
        {!manual && resolved && !resolved.rig && (
          <p role="alert" className="error">
            This entity does not resolve to a single supported male/female
            source rig.
          </p>
        )}
        {!manual && recipe && (
          <p className="muted">
            Matched the existing {recipe.name} character by archive contents;
            its tested preview settings are reused.
          </p>
        )}
        <details>
          <summary>Advanced preview settings</summary>
          <div className="form-grid advanced-character">
            {(
              [
                ["project", "Character manifest (optional)"],
                ["anchors", "Origin anchors (comma-separated)"],
                ["hair", "Rigid hair component (optional)"],
              ] as const
            ).map(([key, label]) => (
              <Field key={key} label={label}>
                <Input
                  value={form[key]}
                  onChange={(e) => setForm({ ...form, [key]: e.target.value })}
                />
              </Field>
            ))}
          </div>
        </details>
        {error && (
          <p className="error" role="alert">
            {error}
          </p>
        )}
        <Button
          variant="primary"
          disabled={
            busy ||
            !form.name ||
            !form.entity ||
            !form.appearance ||
            (!manual && !resolved?.rig)
          }
          onClick={register}
        >
          {busy
            ? "Preparing character choices…"
            : existing
              ? "Use this character"
              : "Add character to library"}
        </Button>
      </Panel>
    </section>
  );
}
