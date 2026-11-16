use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags, ToSql, types::ValueRef};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub struct Store {
    pub catalogue: PathBuf,
    pub work: PathBuf,
    pub config: Value,
    pub settings: Value,
    pub defaults: Vec<String>,
    pub port: u16,
    pub paused: AtomicBool,
    pub closed: AtomicBool,
    pub error: Mutex<Option<String>>,
    pub current: Mutex<Option<Value>>,
    pub discovery_lock: Mutex<()>,
    pub browser_lock: Mutex<()>,
    pub generated_lock: Mutex<()>,
    pub shutdown: tokio::sync::Notify,
}
pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn file_hash(path: impl AsRef<Path>) -> Result<String> {
    let mut f = fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut b = vec![0; 1024 * 1024];
    loop {
        let n = f.read(&mut b)?;
        if n == 0 {
            break;
        }
        h.update(&b[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}
pub fn atomic_json(path: &Path, v: &Value) -> Result<()> {
    let mut f = tempfile::NamedTempFile::new_in(path.parent().context("No output parent")?)?;
    serde_json::to_writer_pretty(&mut f, v)?;
    f.flush()?;
    f.persist(path).map_err(|e| e.error)?;
    Ok(())
}
pub fn string<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("{k} is required"))
}
pub fn rows(db: &Connection, sql: &str, params: &[&dyn ToSql]) -> Result<Vec<Value>> {
    let mut statement = db.prepare(sql)?;
    let names = statement
        .column_names()
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    let mut cursor = statement.query(params)?;
    let mut output = Vec::new();
    while let Some(row) = cursor.next()? {
        let mut item = serde_json::Map::new();
        for (i, name) in names.iter().enumerate() {
            let value = match row.get_ref(i)? {
                ValueRef::Null => Value::Null,
                ValueRef::Integer(v) => json!(v),
                ValueRef::Real(v) => json!(v),
                ValueRef::Text(v) => json!(std::str::from_utf8(v)?),
                ValueRef::Blob(_) => Value::Null,
            };
            item.insert(name.clone(), value);
        }
        output.push(Value::Object(item));
    }
    Ok(output)
}
pub fn first(db: &Connection, sql: &str, params: &[&dyn ToSql]) -> Result<Value> {
    rows(db, sql, params)?
        .into_iter()
        .next()
        .context("Record not found")
}

impl Store {
    pub fn db(&self, sources: bool) -> Result<Connection> {
        let db = Connection::open_with_flags(
            self.work.join("workspots.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_URI,
        )?;
        db.busy_timeout(Duration::from_secs(30))?;
        if sources {
            for (name, path) in [
                (
                    "captions",
                    self.catalogue
                        .join("caption-results/caption-search.sqlite3"),
                ),
                ("catalogue", self.catalogue.join("workspots.sqlite3")),
            ] {
                let p = path.to_string_lossy().replace('\\', "/");
                let p = p.trim_start_matches("//?/");
                let uri = if cfg!(windows) {
                    format!("file:///{p}?mode=ro")
                } else {
                    format!("file:{p}?mode=ro")
                };
                db.execute(&format!("ATTACH DATABASE ? AS {name}"), [uri])?;
            }
        }
        Ok(db)
    }
    pub fn open(
        catalogue: &Path,
        config: &Path,
        work: &Path,
        bulk: bool,
        port: u16,
    ) -> Result<Self> {
        let config: Value = serde_json::from_slice(&fs::read(config)?)?;
        let settings = json!({"fps":30,"seconds":8,"size":512,"worker_sha256":file_hash(string(&config,"worker")?)?});
        let mut app = Self {
            catalogue: catalogue.canonicalize()?,
            work: work.canonicalize()?,
            config,
            settings,
            defaults: Vec::new(),
            port,
            paused: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            error: Mutex::new(None),
            current: Mutex::new(None),
            discovery_lock: Mutex::new(()),
            browser_lock: Mutex::new(()),
            generated_lock: Mutex::new(()),
            shutdown: tokio::sync::Notify::new(),
        };
        let mut db = app.db(false)?;
        db.execute_batch("PRAGMA journal_mode=WAL;
        CREATE TABLE IF NOT EXISTS clips(id TEXT PRIMARY KEY,animation TEXT,rig TEXT,plan TEXT);
        CREATE TABLE IF NOT EXISTS profiles(id TEXT PRIMARY KEY,name TEXT,rig TEXT,config TEXT);
        CREATE TABLE IF NOT EXISTS jobs(id INTEGER PRIMARY KEY,clip TEXT,profile TEXT,state TEXT,priority INTEGER DEFAULT 0,created REAL,started REAL,finished REAL,error TEXT,receipt TEXT,UNIQUE(clip,profile));
        CREATE INDEX IF NOT EXISTS job_queue ON jobs(state,priority,profile,id);
        CREATE TABLE IF NOT EXISTS metadata(key TEXT PRIMARY KEY,value TEXT);
        CREATE TABLE IF NOT EXISTS generated_motions(id TEXT PRIMARY KEY,prompt TEXT NOT NULL,fps INTEGER NOT NULL,frames INTEGER NOT NULL,manifest TEXT NOT NULL,created REAL NOT NULL);
        CREATE TABLE IF NOT EXISTS source_uses(clip TEXT,workspot TEXT,role TEXT,entry_id INTEGER,field TEXT);
        CREATE INDEX IF NOT EXISTS source_workspot ON source_uses(workspot);
        CREATE TABLE IF NOT EXISTS browser_previews(key TEXT PRIMARY KEY,clip TEXT,profile TEXT,state TEXT,error TEXT,receipt TEXT,created REAL);")?;
        let path = app.catalogue.join("full-batch/plan.json");
        let bytes = fs::read(&path)?;
        let identity = hash(&bytes);
        let plan: Value = serde_json::from_slice(&bytes)?;
        let old = rows(
            &db,
            "SELECT value FROM metadata WHERE key='plan_sha256'",
            &[],
        )?;
        ensure!(
            old.is_empty() || old[0]["value"] == identity,
            "Canonical plan changed; use a new work directory"
        );
        let tx = db.transaction()?;
        for job in plan["jobs"].as_array().context("No canonical jobs")? {
            let rig = if string(job, "rig")?.contains("woman_base") {
                "female"
            } else {
                "male"
            };
            tx.execute(
                "INSERT OR IGNORE INTO clips VALUES(?,?,?,?)",
                rusqlite::params![
                    string(job, "id")?,
                    string(job, "animation_name")?,
                    rig,
                    job.to_string()
                ],
            )?;
        }
        let uses: i64 = tx.query_row("SELECT count(*) FROM source_uses", [], |r| r.get(0))?;
        if uses == 0 {
            for job in plan["jobs"].as_array().context("No jobs")? {
                for u in job["uses"].as_array().context("No source uses")? {
                    tx.execute(
                        "INSERT INTO source_uses VALUES(?,?,?,?,?)",
                        rusqlite::params![
                            string(job, "id")?,
                            string(u, "workspot")?,
                            string(u, "role")?,
                            u["entry_id"].as_i64(),
                            string(u, "field")?
                        ],
                    )?;
                }
            }
        }
        let coverage = json!({"resolved":plan["jobs"].as_array().map_or(0,Vec::len),"unresolved":plan["unresolved"].as_array().map_or(0,Vec::len),"skipped":plan["skipped"].as_array().map_or(0,Vec::len)});
        tx.execute(
            "INSERT OR REPLACE INTO metadata VALUES('plan_sha256',?)",
            [identity],
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO metadata VALUES('coverage',?)",
            [coverage.to_string()],
        )?;
        tx.execute(
            "UPDATE jobs SET state='queued',started=NULL WHERE state='running'",
            [],
        )?;
        tx.execute(
            "UPDATE browser_previews SET state='queued' WHERE state='preparing'",
            [],
        )?;
        tx.commit()?;
        for job in rows(
            &db,
            "SELECT id,clip,profile,receipt FROM jobs WHERE state='complete'",
            &[],
        )? {
            let path = app
                .work
                .join("renders")
                .join(string(&job, "profile")?)
                .join(string(&job, "clip")?)
                .join("preview.mp4");
            let receipt: Value = serde_json::from_str(string(&job, "receipt")?)?;
            if !path.is_file()
                || file_hash(&path)? != receipt["video_sha256"].as_str().unwrap_or_default()
            {
                db.execute("UPDATE jobs SET state='queued',receipt=NULL,error='Missing or changed video; re-rendering' WHERE id=?",[job["id"].as_i64()])?;
            }
        }
        for p in app.config["characters"]
            .as_array()
            .context("No default characters")?
        {
            let id = app.register(p.clone())?;
            app.defaults.push(id);
        }
        if bulk {
            for id in &app.defaults {
                let rig = first(&db, "SELECT rig FROM profiles WHERE id=?", &[id])?;
                db.execute("INSERT OR IGNORE INTO jobs(clip,profile,state,priority,created) SELECT id,?,'queued',0,? FROM clips WHERE rig=?",rusqlite::params![id,now(),string(&rig,"rig")?])?;
            }
        }
        Ok(app)
    }
    pub fn helper(&self, key: &str, name: &str) -> Result<PathBuf> {
        Ok(self.config[key]
            .as_str()
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                Path::new(self.config["worker"].as_str().unwrap_or_default()).with_file_name(name)
            }))
    }
    pub fn profile(&self, id: &str) -> Result<Value> {
        let d = self.db(false)?;
        let p = first(&d, "SELECT config FROM profiles WHERE id=?", &[&id])?;
        Ok(serde_json::from_str(string(&p, "config")?)?)
    }
    pub fn register(&self, mut p: Value) -> Result<String> {
        let expected = p
            .as_object_mut()
            .context("Expected a character object")?
            .remove("archive_sha256");
        ensure!(
            matches!(p["rig"].as_str(), Some("male" | "female")),
            "Choose a male or female source rig"
        );
        ensure!(
            !string(&p, "name")?.trim().is_empty() && string(&p, "name")?.len() <= 100,
            "Character name is required (up to 100 characters)"
        );
        let entity = string(&p, "entity")?;
        ensure!(
            !entity.contains("..") && !entity.contains(':'),
            "Entity must be a depot path or native hash"
        );
        ensure!(
            entity.to_ascii_lowercase().ends_with(".ent")
                || entity.parse::<u64>().is_ok_and(|n| n > 0),
            "Expected an .ent resource or native hash"
        );
        ensure!(
            !string(&p, "appearance")?.is_empty(),
            "Appearance is required"
        );
        let overlays = p["overlays"].as_array().cloned().unwrap_or_default();
        ensure!(
            overlays.len() <= 8,
            "At most eight archive overlays are supported"
        );
        let mut paths = Vec::new();
        let mut hashes = Vec::new();
        for path in overlays {
            let path = Path::new(path.as_str().context("Invalid archive path")?).canonicalize()?;
            hashes.push(file_hash(&path)?);
            paths.push(path.to_string_lossy().into_owned());
        }
        if let Some(expected) = expected.filter(|v| !v.is_null()) {
            ensure!(
                hashes
                    == vec![
                        expected
                            .as_str()
                            .context("Invalid archive hash")?
                            .to_string()
                    ],
                "Archive changed after discovery; inspect it again"
            );
        }
        p["overlays"] = json!(paths);
        p["overlay_hashes"] = json!(hashes);
        if let Some(project) = p["project"].as_str().filter(|s| !s.is_empty()) {
            let path = Path::new(project).canonicalize()?;
            let value: Value = serde_json::from_slice(&fs::read(&path)?)?;
            ensure!(
                value["refit_clothing"] != true,
                "Refitted character rejected; rebuild with refit_clothing=false"
            );
            p["project"] = json!(path.to_string_lossy().trim_start_matches("\\\\?\\"));
            p["project_sha256"] = json!(file_hash(&path)?);
        }
        p["refit_clothing"] = json!(false);
        p["settings"] = self.settings.clone();
        let db = self.db(false)?;
        // Preserve legacy profile IDs and their render directories during migration.
        for old in rows(
            &db,
            "SELECT id,config FROM profiles WHERE name=?",
            &[&string(&p, "name")?],
        )? {
            let mut value: Value = serde_json::from_str(string(&old, "config")?)?;
            let normalize = |v: &mut Value| {
                if let Some(s) = v["entity"].as_str() {
                    v["entity"] = json!(s.replace('\\', "/").to_ascii_lowercase());
                }
                v.as_object_mut().map(|m| m.remove("overlays"));
            };
            let mut candidate = p.clone();
            normalize(&mut value);
            normalize(&mut candidate);
            if value == candidate {
                return Ok(string(&old, "id")?.to_owned());
            }
        }
        let id = hash(p.to_string().as_bytes())[..24].to_owned();
        db.execute(
            "INSERT OR IGNORE INTO profiles VALUES(?,?,?,?)",
            rusqlite::params![id, string(&p, "name")?, string(&p, "rig")?, p.to_string()],
        )?;
        Ok(id)
    }
    pub fn enqueue(&self, clip: &str, profile: &str, retry: bool) -> Result<Value> {
        let db = self.db(false)?;
        let a = first(&db, "SELECT rig FROM clips WHERE id=?", &[&clip])?;
        let b = first(&db, "SELECT rig FROM profiles WHERE id=?", &[&profile])?;
        ensure!(
            a["rig"] == b["rig"],
            "Character and animation rigs must match"
        );
        db.execute("INSERT OR IGNORE INTO jobs(clip,profile,state,priority,created) VALUES(?,?,'queued',100,?)",rusqlite::params![clip,profile,now()])?;
        db.execute(
            "UPDATE jobs SET priority=100 WHERE clip=? AND profile=?",
            [clip, profile],
        )?;
        if retry {
            db.execute("UPDATE jobs SET state='queued',error=NULL WHERE clip=? AND profile=? AND state='failed'",[clip,profile])?;
        }
        first(
            &db,
            "SELECT * FROM jobs WHERE clip=? AND profile=?",
            &[&clip, &profile],
        )
    }
    pub fn control(&self, action: &str) -> Result<Value> {
        match action {
            "pause" => self.paused.store(true, Ordering::Release),
            "resume" => {
                *self.error.lock().unwrap_or_else(|e| e.into_inner()) = None;
                self.paused.store(false, Ordering::Release);
            }
            _ => anyhow::bail!("Expected pause or resume"),
        };
        Ok(json!({"paused":self.paused.load(Ordering::Acquire)}))
    }
    pub fn status(&self) -> Result<Value> {
        let db = self.db(true)?;
        let mut counts = serde_json::Map::new();
        for r in rows(
            &db,
            "SELECT state,count(*) AS count FROM jobs GROUP BY state",
            &[],
        )? {
            counts.insert(string(&r, "state")?.to_owned(), r["count"].clone());
        }
        let meta = first(&db, "SELECT value FROM metadata WHERE key='coverage'", &[])?;
        let mut coverage: Value = serde_json::from_str(string(&meta, "value")?)?;
        coverage["workspots"] = json!(db.query_row(
            "SELECT count(*) FROM catalogue.workspots",
            [],
            |r| r.get::<_, i64>(0)
        )?);
        let times = rows(
            &db,
            "SELECT started,finished FROM jobs WHERE state='complete' ORDER BY finished DESC LIMIT 30",
            &[],
        )?;
        let rate = if times.is_empty() {
            None
        } else {
            Some(
                times
                    .iter()
                    .map(|v| {
                        v["finished"].as_f64().unwrap_or_default()
                            - v["started"].as_f64().unwrap_or_default()
                    })
                    .sum::<f64>()
                    / times.len() as f64,
            )
        };
        Ok(
            json!({"backend":"rust","counts":counts,"coverage":coverage,"profiles":rows(&db,"SELECT id,name,rig,config FROM profiles",&[])?,"recent":rows(&db,"SELECT j.id,j.clip,c.animation,p.name AS character,j.state,j.error,j.finished FROM jobs j JOIN clips c ON c.id=j.clip JOIN profiles p ON p.id=j.profile ORDER BY coalesce(j.finished,j.started,j.created) DESC LIMIT 12",&[])?,"current":*self.current.lock().unwrap_or_else(|e|e.into_inner()),"error":*self.error.lock().unwrap_or_else(|e|e.into_inner()),"paused":self.paused.load(Ordering::Acquire),"settings":self.settings,"seconds_per_clip":rate,"eta_seconds":rate.map(|r|r*counts.get("queued").and_then(Value::as_f64).unwrap_or_default())}),
        )
    }
    pub fn detail(&self, id: &str) -> Result<Value> {
        let db = self.db(true)?;
        let mut item = first(&db, "SELECT * FROM clips WHERE id=?", &[&id])?;
        item["plan"] = serde_json::from_str(string(&item, "plan")?)?;
        let caption = rows(
            &db,
            "SELECT caption_json,review_status FROM captions.captions WHERE id=?",
            &[&id],
        )?;
        item["caption"] = if let Some(c) = caption.first() {
            serde_json::from_str(string(c, "caption_json")?)?
        } else {
            Value::Null
        };
        item["review_status"] = caption
            .first()
            .map_or(json!("uncaptioned"), |c| c["review_status"].clone());
        let mut renders = rows(
            &db,
            "SELECT j.*,p.name AS character FROM jobs j JOIN profiles p ON p.id=j.profile WHERE clip=? ORDER BY j.id",
            &[&id],
        )?;
        for r in &mut renders {
            if let Some(raw) = r["receipt"].as_str() {
                r["receipt"] = serde_json::from_str(raw)?;
            }
            r["video"] = if r["state"] == "complete" {
                json!(format!(
                    "/media/renders/{}/{id}/preview.mp4",
                    string(r, "profile")?
                ))
            } else {
                Value::Null
            };
        }
        item["renders"] = json!(renders);
        item["original_video"] = if self
            .catalogue
            .join("previews")
            .join(id)
            .join("preview.mp4")
            .is_file()
        {
            json!(format!("/media/original/{id}/preview.mp4"))
        } else {
            Value::Null
        };
        Ok(item)
    }
    pub fn search(&self, q: &str, rig: &str, state: &str, page: i64, kind: &str) -> Result<Value> {
        let db = self.db(true)?;
        let page = page.clamp(0, 10000);
        let size = 35;
        let mut params = Vec::<String>::new();
        let mut conditions = Vec::<String>::new();
        if kind == "workspots" {
            if !q.is_empty() {
                conditions.push("(w.depot_path LIKE ? OR w.label LIKE ?)".into());
                params.extend([format!("%{q}%"), format!("%{q}%")]);
            }
            let clause = if conditions.is_empty() {
                String::new()
            } else {
                format!("WHERE {}", conditions.join(" AND "))
            };
            let args = params.iter().map(|s| s as &dyn ToSql).collect::<Vec<_>>();
            let count = first(
                &db,
                &format!("SELECT count(*) AS n FROM catalogue.workspots w {clause}"),
                &args,
            )?["n"]
                .clone();
            params.extend([size.to_string(), (page * size).to_string()]);
            let args = params.iter().map(|s| s as &dyn ToSql).collect::<Vec<_>>();
            let items = rows(
                &db,
                &format!(
                    "SELECT w.depot_path,w.label,w.animation_count,w.review_status,(SELECT count(DISTINCT clip) FROM source_uses WHERE workspot=w.depot_path) AS resolved_clips FROM catalogue.workspots w {clause} ORDER BY w.depot_path LIMIT ? OFFSET ?"
                ),
                &args,
            )?;
            return Ok(json!({"items":items,"total":count,"page":page,"size":size}));
        }
        if !q.is_empty() {
            let terms = q
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .filter(|s| !s.is_empty())
                .take(20)
                .map(|s| format!("\"{s}\"*"))
                .collect::<Vec<_>>();
            if !terms.is_empty() {
                conditions.push("(c.id IN(SELECT clip_id FROM captions.caption_search WHERE caption_search MATCH ?) OR c.animation LIKE ? OR c.id=? OR c.id IN(SELECT clip FROM source_uses WHERE workspot=?))".into());
                params.extend([
                    terms.join(" AND "),
                    format!("%{q}%"),
                    q.to_string(),
                    q.to_string(),
                ]);
            }
        }
        if !rig.is_empty() {
            conditions.push("c.rig=?".into());
            params.push(rig.into());
        }
        if state == "unrendered" {
            conditions.push(
                "NOT EXISTS(SELECT 1 FROM jobs j WHERE j.clip=c.id AND j.state='complete')".into(),
            );
        } else if !state.is_empty() {
            conditions.push("EXISTS(SELECT 1 FROM jobs j WHERE j.clip=c.id AND j.state=?)".into());
            params.push(state.into());
        }
        let clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };
        let args = params.iter().map(|s| s as &dyn ToSql).collect::<Vec<_>>();
        let count = first(
            &db,
            &format!("SELECT count(*) AS n FROM clips c {clause}"),
            &args,
        )?["n"]
            .clone();
        params.extend([size.to_string(), (page * size).to_string()]);
        let args = params.iter().map(|s| s as &dyn ToSql).collect::<Vec<_>>();
        let items = rows(
            &db,
            &format!(
                "SELECT c.id,c.animation,c.rig,json_array_length(c.plan,'$.uses') AS uses,(SELECT state FROM jobs j WHERE j.clip=c.id ORDER BY state='complete' DESC,id LIMIT 1) AS state FROM clips c {clause} ORDER BY c.animation,c.id LIMIT ? OFFSET ?"
            ),
            &args,
        )?;
        Ok(json!({"items":items,"total":count,"page":page,"size":size}))
    }
}
