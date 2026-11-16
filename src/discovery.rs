use crate::{
    native::{checked, command, schema_args},
    store::{Store, atomic_json, file_hash, hash, rows, string},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};

impl Store {
    fn archive_path(&self, path: &str) -> Result<PathBuf> {
        let path = Path::new(path).canonicalize()?;
        ensure!(
            path.is_file()
                && path
                    .extension()
                    .is_some_and(|s| s.eq_ignore_ascii_case("archive")),
            "Choose an existing .archive file"
        );
        Ok(path)
    }
    fn inspect_command(&self, path: &Path, extra: &[&str]) -> Result<Value> {
        let exe = self.helper("archive_inspector", "workspot_archive.exe")?;
        let output = checked(
            command(exe)
                .args(["--game", string(&self.config, "game")?, "--archive"])
                .arg(path)
                .args(schema_args(&self.config))
                .args(extra),
        )?;
        Ok(serde_json::from_slice(&output.stdout)?)
    }
    pub fn inspect_archive(&self, path: &str) -> Result<Value> {
        let _guard = self
            .discovery_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let path = self.archive_path(path)?;
        let identity = file_hash(&path)?;
        let key = hash(
            json!([
                identity,
                file_hash(self.helper("archive_inspector", "workspot_archive.exe")?)?,
                self.config["schema"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(file_hash)
                    .transpose()?
            ])
            .to_string()
            .as_bytes(),
        );
        let cache = self
            .work
            .join("character-discovery")
            .join(format!("{key}.json"));
        let mut result = if cache.is_file() {
            serde_json::from_slice(&fs::read(&cache)?)?
        } else {
            let result = self.inspect_command(&path, &[])?;
            ensure!(
                file_hash(&path)? == identity,
                "Archive changed during discovery"
            );
            fs::create_dir_all(cache.parent().context("Missing cache parent")?)?;
            atomic_json(&cache, &result)?;
            result
        };
        result["archive"] = json!(path.to_string_lossy());
        result["archive_sha256"] = json!(identity);
        Ok(result)
    }
    pub fn resolve_archive(&self, request: &Value) -> Result<Value> {
        let path = string(request, "archive")?;
        let identity = string(request, "archive_sha256")?;
        let entity = string(request, "entity")?;
        let appearance = string(request, "appearance")?;
        let catalog = self.inspect_archive(path)?;
        ensure!(
            catalog["archive_sha256"] == identity,
            "Archive changed; inspect it again"
        );
        let selected = catalog["entities"]
            .as_array()
            .context("No entities")?
            .iter()
            .find(|e| e["resource"] == entity)
            .context("Choose a discovered entity")?;
        ensure!(
            selected["appearances"]
                .as_array()
                .is_some_and(|a| a.iter().any(|v| v["name"] == appearance)),
            "Choose an appearance discovered in this archive"
        );
        let _guard = self
            .discovery_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut result = self.inspect_command(
            &self.archive_path(path)?,
            &["--entity", entity, "--appearance", appearance],
        )?;
        ensure!(
            file_hash(path)? == identity,
            "Archive changed during entity resolution"
        );
        let db = self.db(false)?;
        for row in rows(&db, "SELECT id,name,config FROM profiles", &[])? {
            let p: Value = serde_json::from_str(string(&row, "config")?)?;
            if p["entity"]
                .as_str()
                .is_some_and(|s| s.replace('\\', "/") == entity.replace('\\', "/"))
                && p["appearance"] == appearance
                && p["overlay_hashes"] == json!([identity])
            {
                let usable = p["overlays"].as_array().is_some_and(|a| {
                    a.iter().all(|v| {
                        v.as_str()
                            .is_some_and(|s| file_hash(s).is_ok_and(|h| h == identity))
                    })
                });
                result["existing_recipe"] = json!({"id":if usable{string(&row,"id")?}else{""},"name":row["name"],"origin_anchors":p["origin_anchors"],"rigid_hair":p["rigid_hair"]});
                break;
            }
        }
        Ok(result)
    }
}
