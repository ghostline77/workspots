//! Hash-validated imports of independent generated motions and their source evidence.
use crate::store::{Store, atomic_json, file_hash, first, hash, now, rows, string};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{fs, io::Read, path::Path};

impl Store {
    pub fn generated_retarget(&self, id: &str, key: &str, hand_pose: &str) -> Result<Value> {
        ensure!(
            ["generated", "open_palm", "open_right", "open_left"].contains(&hand_pose),
            "Unknown hand pose"
        );
        let motion = self.generated_detail(id)?;
        let character = self.generated_character_get(key)?;
        ensure!(character["state"] == "ready", "Character is not ready");
        let _guard = self
            .generated_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("Retarget lock unavailable"))?;
        let source = self.work.join("generated").join(id).join("motion.bvh");
        let dir = self.work.join("browser").join(key);
        let rest: Value = serde_json::from_slice(&fs::read(dir.join("rig-rest.json"))?)?;
        ensure!(
            rest["glb_sha256"] == character["receipt"]["glb_sha256"],
            "Rest pose belongs to different media"
        );
        let identity = json!({"algorithm":"soma-native-rest-v10","hand_pose":hand_pose,"source":motion["manifest"]["files"]["motion.bvh"],
            "glb":character["receipt"]["glb_sha256"],"rest":character["rest_sha256"]});
        let cache = hash(&serde_json::to_vec(&identity)?);
        let path = dir.join(format!("retarget-{cache}.json"));
        if !path.exists() {
            let mut file = fs::File::open(dir.join("scene.glb"))?;
            let mut header = [0; 20];
            file.read_exact(&mut header)?;
            ensure!(
                &header[..4] == b"glTF" && &header[16..20] == b"JSON",
                "Invalid character GLB"
            );
            let size = u32::from_le_bytes(header[12..16].try_into()?) as usize;
            ensure!(size <= 4 * 1024 * 1024, "Oversized GLB metadata");
            let mut bytes = vec![0; size];
            file.read_exact(&mut bytes)?;
            let glb: Value = serde_json::from_slice(&bytes)?;
            let mut result = crate::retarget::bake_with_hands(
                &fs::read_to_string(source)?,
                &rest,
                &glb,
                hand_pose,
            )?;
            result["identity"] = identity;
            atomic_json(&path, &result)?;
        }
        Ok(
            json!({"clip":format!("/media/browser/{key}/retarget-{cache}.json"),"sha256":file_hash(path)?,"engine":"rust"}),
        )
    }

    pub fn generated_characters(&self) -> Result<Value> {
        Ok(
            json!({"items":rows(&self.db(false)?, "SELECT id,name,rig FROM profiles ORDER BY name", &[])?}),
        )
    }

    pub fn generated_character(&self, profile: &str) -> Result<Value> {
        let p = self.profile(profile)?;
        let db = self.db(false)?;
        let cached = rows(
            &db,
            "SELECT clip FROM browser_previews WHERE profile=? AND state='ready' ORDER BY created DESC LIMIT 1",
            &[&profile],
        )?;
        let clip = if let Some(row) = cached.first() {
            row.clone()
        } else {
            first(
                &db,
                "SELECT id AS clip FROM clips WHERE rig=? ORDER BY id LIMIT 1",
                &[&string(&p, "rig")?],
            )?
        };
        let job = self.browser_request(string(&clip, "clip")?, profile, false)?;
        self.generated_character_get(string(&job, "key")?)
    }

    pub fn generated_character_get(&self, key: &str) -> Result<Value> {
        ensure!(
            key.len() == 32 && key.bytes().all(|b| b.is_ascii_hexdigit()),
            "Invalid character preview key"
        );
        let mut job = self.browser_get(key)?;
        if job["state"] != "ready" {
            return Ok(job);
        }
        let _guard = self
            .generated_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("Import lock unavailable"))?;
        let dir = self.work.join("browser").join(key);
        ensure!(
            file_hash(dir.join("scene.glb"))? == string(&job["receipt"], "glb_sha256")?,
            "Character media was changed"
        );
        let rest_path = dir.join("rig-rest.json");
        if !rest_path.exists() {
            let mut file = fs::File::open(dir.join("scene.glb"))?;
            let mut header = [0_u8; 20];
            file.read_exact(&mut header)?;
            ensure!(
                &header[..4] == b"glTF" && &header[16..20] == b"JSON",
                "Invalid character GLB"
            );
            let size = u32::from_le_bytes(header[12..16].try_into()?) as usize;
            ensure!(size <= 4 * 1024 * 1024, "Character metadata is too large");
            let mut bytes = vec![0; size];
            file.read_exact(&mut bytes)?;
            let glb: Value = serde_json::from_slice(&bytes)?;
            let names = glb["nodes"].as_array().context("Character nodes missing")?;
            let workers = self
                .work
                .join("browser-workers")
                .join(string(&job, "profile")?);
            let mut candidates = fs::read_dir(workers)?.collect::<std::io::Result<Vec<_>>>()?;
            candidates.sort_by_key(|entry| std::cmp::Reverse(entry.file_name()));
            let mut found = false;
            for entry in candidates {
                let path = entry.path().join("assembly.json");
                if !path.is_file() || fs::metadata(&path)?.len() > 16 * 1024 * 1024 {
                    continue;
                }
                let assembly: Value = serde_json::from_slice(&fs::read(&path)?)?;
                let Some(nodes) = assembly["nodes"].as_array() else {
                    continue;
                };
                if assembly["renderer_build"] != job["receipt"]["build"]
                    || nodes.len() != names.len()
                    || !nodes.iter().zip(names).all(|(a, b)| a["name"] == b["name"])
                {
                    continue;
                }
                let rest: Vec<_> = nodes
                    .iter()
                    .map(|node| json!({"name":node["name"],"local":node["local"]}))
                    .collect();
                atomic_json(
                    &rest_path,
                    &json!({"format":"ghostline-character-rest","version":1,
                    "profile":job["profile"],"glb_sha256":job["receipt"]["glb_sha256"],"nodes":rest}),
                )?;
                found = true;
                break;
            }
            ensure!(
                found,
                "Rest-pose metadata is unavailable; prepare this character again"
            );
        }
        job["rest"] = json!(format!("/media/browser/{key}/rig-rest.json"));
        job["rest_sha256"] = json!(file_hash(rest_path)?);
        Ok(job)
    }

    pub fn generated_list(&self) -> Result<Value> {
        Ok(json!({"items":rows(&self.db(false)?,
            "SELECT id,prompt,fps,frames,created FROM generated_motions ORDER BY created DESC LIMIT 500", &[])?}))
    }

    pub fn generated_detail(&self, id: &str) -> Result<Value> {
        ensure!(
            id.len() == 24 && id.bytes().all(|b| b.is_ascii_hexdigit()),
            "Invalid generated motion ID"
        );
        let mut value = first(
            &self.db(false)?,
            "SELECT id,prompt,fps,frames,created,manifest FROM generated_motions WHERE id=?",
            &[&id],
        )?;
        value["manifest"] = serde_json::from_str(string(&value, "manifest")?)?;
        for name in ["motion.bvh", "motion.npz"] {
            ensure!(
                file_hash(self.work.join("generated").join(id).join(name))?
                    == string(&value["manifest"]["files"], name)?,
                "Imported file was changed: {name}"
            );
        }
        value["review_status"] = json!("unreviewed");
        value["bvh"] = json!(format!("/media/generated/{id}/motion.bvh"));
        value["npz"] = json!(format!("/media/generated/{id}/motion.npz"));
        value["provenance"] = json!(format!("/media/generated/{id}/import.json"));
        value["bvh_scale_to_metres"] = json!(0.01);
        Ok(value)
    }

    pub fn import_generated(&self, path: &Path) -> Result<Value> {
        let _guard = self
            .generated_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("Import lock unavailable"))?;
        let path = path
            .canonicalize()
            .context("Motion package path does not exist")?;
        ensure!(
            fs::metadata(&path)?.len() <= 131_072,
            "Motion manifest is too large"
        );
        let source = path.parent().context("Package has no parent directory")?;
        let raw = fs::read(&path)?;
        let manifest: Value =
            serde_json::from_slice(&raw).context("Invalid motion package JSON")?;
        ensure!(
            manifest["format"] == "ghostline-generated-motion" && manifest["version"] == 1,
            "Expected a ghostline-generated-motion v1 package"
        );
        ensure!(
            manifest["skeleton"] == "soma77",
            "Only source SOMA motions are currently supported"
        );
        ensure!(manifest["fps"] == 30, "Expected 30 FPS source motion");
        let frames = manifest["frames"].as_u64().context("Frame count missing")?;
        ensure!(
            (1..=3600).contains(&frames),
            "Frame count is outside the supported range"
        );
        let prompt = string(&manifest, "prompt")?;
        ensure!(
            !prompt.trim().is_empty() && prompt.len() <= 8192,
            "Invalid prompt"
        );
        let root = self.work.join("generated");
        fs::create_dir_all(&root)?;
        let staging = tempfile::tempdir_in(&root)?;
        let mut captured = serde_json::Map::new();
        for name in ["motion.npz", "motion.bvh", "generation.log"] {
            let expected = string(&manifest["files"], name)?;
            ensure!(
                expected.len() == 64 && expected.bytes().all(|b| b.is_ascii_hexdigit()),
                "Invalid hash for {name}"
            );
            copy_package_file(source, staging.path(), name)?;
            let actual = file_hash(staging.path().join(name))?;
            ensure!(
                actual == expected,
                "Hash mismatch for {name}; package was changed or is incomplete"
            );
            captured.insert(name.to_owned(), json!(actual));
        }
        let bvh = fs::read_to_string(staging.path().join("motion.bvh"))?;
        let declared = bvh
            .lines()
            .find_map(|l| l.trim().strip_prefix("Frames:").map(str::trim))
            .context("BVH frame count missing")?
            .parse::<u64>()?;
        let frame_time = bvh
            .lines()
            .find_map(|l| l.trim().strip_prefix("Frame Time:").map(str::trim))
            .context("BVH frame time missing")?
            .parse::<f64>()?;
        ensure!(
            declared == frames
                && frame_time.is_finite()
                && (frame_time - 1.0 / 30.0).abs() < 0.000001,
            "BVH duration does not match the package"
        );
        for name in [
            "request.json",
            "runner.py",
            "runtime_bootstrap.py",
            "text-memory.json",
            "text-embeddings.safetensors",
        ] {
            if source.join(name).exists() {
                copy_package_file(source, staging.path(), name)?;
                captured.insert(
                    name.to_owned(),
                    json!(file_hash(staging.path().join(name))?),
                );
            }
        }
        fs::write(staging.path().join("source-manifest.json"), &raw)?;
        let source_hash = hash(&serde_json::to_vec(&manifest)?);
        let id = &source_hash[..24];
        atomic_json(
            &staging.path().join("import.json"),
            &json!({
                "format":"ghostline-motion-import","version":1,"id":id,"source_manifest_sha256":hash(&raw),
                "files":captured,"review_status":"unreviewed","runtime_verified":false,
                "bvh_scale_to_metres":0.01,"imported_at":now()
            }),
        )?;
        let destination = root.join(id);
        if destination.exists() {
            for (name, expected) in &captured {
                ensure!(
                    file_hash(destination.join(name))? == expected.as_str().unwrap_or_default(),
                    "Existing import was changed: {name}"
                );
            }
        } else {
            fs::rename(staging.path(), &destination)?;
        }
        self.db(false)?.execute("INSERT OR IGNORE INTO generated_motions(id,prompt,fps,frames,manifest,created) VALUES(?,?,30,?,?,?)",
            rusqlite::params![id,prompt,frames,manifest.to_string(),now()])?;
        self.generated_detail(id)
    }
}

fn copy_package_file(source: &Path, destination: &Path, name: &str) -> Result<()> {
    let path = source
        .join(name)
        .canonicalize()
        .with_context(|| format!("Missing {name}"))?;
    ensure!(
        path.parent() == Some(source),
        "Package file escapes its directory: {name}"
    );
    let metadata = fs::metadata(&path)?;
    ensure!(
        metadata.is_file() && metadata.len() <= 64 * 1024 * 1024,
        "Invalid or oversized package file: {name}"
    );
    fs::copy(path, destination.join(name))?;
    Ok(())
}
