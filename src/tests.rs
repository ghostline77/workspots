use crate::{
    store::{Store, file_hash, first},
    web,
};
use anyhow::Result;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use rusqlite::Connection;
use serde_json::json;
use std::{fs, sync::Arc};
use tower::ServiceExt;

fn fixture() -> Result<(tempfile::TempDir, Store)> {
    let temp = tempfile::tempdir()?;
    let root = temp.path();
    let cat = root.join("catalogue");
    fs::create_dir_all(cat.join("full-batch"))?;
    fs::create_dir_all(cat.join("caption-results"))?;
    let work = root.join("work");
    fs::create_dir_all(&work)?;
    let plan = json!({"jobs":[{"id":"a","animation_name":"phone","rig":"man_base.rig","uses":[{"workspot":"phone.workspot","role":"clip","entry_id":1,"field":"animName"}],"resolved_animset":"a.anims"},{"id":"b","animation_name":"sit","rig":"woman_base.rig","uses":[],"resolved_animset":"b.anims"}],"unresolved":[{}],"skipped":[{}]});
    fs::write(cat.join("full-batch/plan.json"), plan.to_string())?;
    let db = Connection::open(cat.join("workspots.sqlite3"))?;
    db.execute_batch("CREATE TABLE workspots(depot_path TEXT,label TEXT,animation_count INTEGER,review_status TEXT);INSERT INTO workspots VALUES('phone.workspot','Phone',1,'unreviewed');")?;
    let db = Connection::open(cat.join("caption-results/caption-search.sqlite3"))?;
    db.execute_batch("CREATE TABLE captions(id TEXT,caption_json TEXT,review_status TEXT);CREATE VIRTUAL TABLE caption_search USING fts5(clip_id UNINDEXED,animation,workspots,description);INSERT INTO captions VALUES('a','{}','unreviewed');INSERT INTO caption_search VALUES('a','phone','phone.workspot','Talking on a phone');")?;
    let worker = root.join("worker.exe");
    fs::write(&worker, b"fixture")?;
    let mut config = json!({"worker":worker,"game":root,"schema":root.join("schema.json"),"characters":[{"name":"Male","rig":"male","entity":"a.ent","appearance":"default","overlays":[],"origin_anchors":["Component"]},{"name":"Female","rig":"female","entity":"b.ent","appearance":"default","overlays":[],"origin_anchors":["Component"]}]});
    let manifest = root.join("character.json");
    fs::write(&manifest, "{\"refit_clothing\":false}")?;
    config["characters"][0]["project"] = json!(manifest);
    fs::write(root.join("config.json"), config.to_string())?;
    let store = Store::open(&cat, &root.join("config.json"), &work, true, 8820)?;
    Ok((temp, store))
}

#[test]
fn generated_import_preserves_source_evidence_and_rejects_changed_media() -> Result<()> {
    let (temp, store) = fixture()?;
    let source = temp.path().join("generated-source");
    fs::create_dir(&source)?;
    fs::write(source.join("motion.npz"), b"source motion fixture")?;
    fs::write(
        source.join("motion.bvh"),
        "HIERARCHY\nROOT Root\n{\nOFFSET 0 0 0\nCHANNELS 6 Xposition Yposition Zposition Zrotation Yrotation Xrotation\n}\nMOTION\nFrames: 2\nFrame Time: 0.0333333333333333\n0 0 0 0 0 0\n0 0 0 0 0 0\n",
    )?;
    fs::write(source.join("generation.log"), b"upstream run")?;
    fs::write(source.join("runner.py"), b"exact executed runner")?;
    let manifest = json!({"format":"ghostline-generated-motion","version":1,"skeleton":"soma77",
    "fps":30,"frames":2,"prompt":"A person waves.","files":{
        "motion.npz":file_hash(source.join("motion.npz"))?,
        "motion.bvh":file_hash(source.join("motion.bvh"))?,
        "generation.log":file_hash(source.join("generation.log"))?
    }});
    let path = source.join("motion.json");
    fs::write(&path, manifest.to_string())?;
    let imported = store.import_generated(&path)?;
    assert_eq!(imported["review_status"], "unreviewed");
    assert_eq!(store.import_generated(&path)?["id"], imported["id"]);
    assert_eq!(
        store.generated_list()?["items"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        first(&store.db(false)?, "SELECT count(*) AS n FROM jobs", &[])?["n"],
        2
    );
    let folder = store
        .work
        .join("generated")
        .join(imported["id"].as_str().unwrap());
    assert_eq!(
        fs::read(folder.join("runner.py"))?,
        b"exact executed runner"
    );
    assert!(folder.join("import.json").is_file());
    fs::write(source.join("motion.bvh"), b"changed source")?;
    assert!(store.import_generated(&path).is_err());
    assert!(
        store
            .generated_detail(imported["id"].as_str().unwrap())
            .is_ok()
    );
    fs::write(folder.join("motion.bvh"), b"changed local media")?;
    assert!(
        store
            .generated_detail(imported["id"].as_str().unwrap())
            .is_err()
    );
    assert!(store.generated_detail("../../outside").is_err());
    Ok(())
}

#[test]
fn queue_deduplicates_and_preserves_complete_outputs_across_restart() -> Result<()> {
    let (t, s) = fixture()?;
    let male = &s.defaults[0];
    s.enqueue("a", male, false)?;
    s.enqueue("a", male, false)?;
    assert!(s.enqueue("a", &s.defaults[1], false).is_err());
    let d = s.db(false)?;
    assert_eq!(first(&d, "SELECT count(*) AS n FROM jobs", &[])?["n"], 2);
    let folder = s.work.join("renders").join(male).join("a");
    fs::create_dir_all(&folder)?;
    fs::write(folder.join("preview.mp4"), b"video fixture")?;
    let receipt = json!({"video_sha256":file_hash(folder.join("preview.mp4"))?});
    d.execute(
        "UPDATE jobs SET state='complete',receipt=?,started=1,finished=2 WHERE clip='a'",
        [receipt.to_string()],
    )?;
    d.execute("UPDATE jobs SET state='running' WHERE clip='b'", [])?;
    drop(d);
    let reloaded = Store::open(
        &s.catalogue,
        &t.path().join("config.json"),
        &s.work,
        true,
        8820,
    )?;
    let counts = reloaded.status()?["counts"].clone();
    assert_eq!(counts["complete"], 1);
    assert_eq!(counts["queued"], 1);
    assert_eq!(reloaded.defaults, s.defaults);
    Ok(())
}

#[test]
fn legacy_profile_ids_are_reused() -> Result<()> {
    let (t, s) = fixture()?;
    let d = s.db(false)?;
    let id = &s.defaults[0];
    let mut original = s.profile(id)?;
    original["project"] = json!(t.path().join("character.json").to_string_lossy());
    d.execute(
        "UPDATE profiles SET config=? WHERE id=?",
        rusqlite::params![original.to_string(), id],
    )?;
    d.execute("UPDATE profiles SET id='legacy-pilot-id' WHERE id=?", [id])?;
    d.execute(
        "UPDATE jobs SET profile='legacy-pilot-id' WHERE profile=?",
        [id],
    )?;
    drop(d);
    let reloaded = Store::open(
        &s.catalogue,
        &t.path().join("config.json"),
        &s.work,
        true,
        8820,
    )?;
    assert_eq!(reloaded.defaults[0], "legacy-pilot-id");
    assert_eq!(reloaded.status()?["counts"]["queued"], 2);
    Ok(())
}

#[test]
fn search_preserves_full_source_use_mappings_and_readonly_catalogue() -> Result<()> {
    let (_t, s) = fixture()?;
    assert_eq!(s.search("talking", "", "", 0, "clips")?["total"], 1);
    assert_eq!(s.search("phone.workspot", "", "", 0, "clips")?["total"], 1);
    assert_eq!(s.detail("a")?["plan"]["uses"][0]["entry_id"], 1);
    assert_eq!(
        s.search("", "", "", 0, "workspots")?["items"][0]["resolved_clips"],
        1
    );
    let d = s.db(true)?;
    assert!(d.execute("DELETE FROM catalogue.workspots", []).is_err());
    Ok(())
}

#[test]
fn changed_plan_and_refitted_manifest_are_rejected() -> Result<()> {
    let (t, s) = fixture()?;
    let mut p = s.config["characters"][0].clone();
    let manifest = t.path().join("character.json");
    fs::write(&manifest, "{\"refit_clothing\":true}")?;
    p["project"] = json!(manifest);
    assert!(s.register(p).is_err());
    let plan = s.catalogue.join("full-batch/plan.json");
    let text = fs::read_to_string(&plan)?;
    fs::write(&plan, text + " ")?;
    assert!(
        Store::open(
            &s.catalogue,
            &t.path().join("config.json"),
            &s.work,
            true,
            8820
        )
        .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn api_uses_rust_and_rejects_cross_origin_mutations() -> Result<()> {
    let (_t, s) = fixture()?;
    let router = web::router(Arc::new(s));
    let response = router
        .clone()
        .oneshot(Request::get("/api/status").body(Body::empty())?)
        .await?;
    assert_eq!(response.headers()["x-ghostline-backend"], "rust");
    let v: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1_000_000).await?)?;
    assert_eq!(v["backend"], "rust");
    let response = router
        .oneshot(
            Request::post("/api/characters")
                .header("host", "127.0.0.1:8820")
                .header("origin", "https://example.com")
                .header("content-type", "application/json")
                .body(Body::from("{}"))?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    Ok(())
}

#[tokio::test]
async fn media_ranges_and_archive_uploads_work_without_python() -> Result<()> {
    let (_t, s) = fixture()?;
    let folder = s.work.join("renders/p/a");
    fs::create_dir_all(&folder)?;
    fs::write(folder.join("preview.mp4"), b"0123456789")?;
    let router = web::router(Arc::new(s));
    let response = router
        .clone()
        .oneshot(
            Request::get("/media/renders/p/a/preview.mp4")
                .header("range", "bytes=2-5")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(&to_bytes(response.into_body(), 100).await?[..], b"2345");
    let mut archive = b"RDAR".to_vec();
    archive.extend(vec![0; 100_000]);
    for _ in 0..2 {
        let response = router
            .clone()
            .oneshot(
                Request::post("/api/archive-upload?filename=actor.archive")
                    .header("host", "127.0.0.1:8820")
                    .header("content-length", archive.len())
                    .body(Body::from(archive.clone()))?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let v: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 10000).await?)?;
        assert!(std::path::Path::new(v["archive"].as_str().unwrap()).is_file());
    }
    Ok(())
}
