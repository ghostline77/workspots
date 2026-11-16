use crate::store::{Store, atomic_json, file_hash, first, hash, now, rows, string};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, atomic::Ordering, mpsc},
    thread,
    time::{Duration, Instant},
};

pub(crate) fn schema_args(config: &Value) -> Vec<&str> {
    config["schema"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|path| vec!["--schema", path])
        .unwrap_or_default()
}

pub fn command(exe: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut c = Command::new(exe);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c
}
pub fn checked(c: &mut Command) -> Result<std::process::Output> {
    let result = c.output()?;
    ensure!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(result)
}

struct Process {
    child: Child,
    input: Option<ChildStdin>,
    messages: mpsc::Receiver<String>,
    profile: String,
    handled: usize,
    out: PathBuf,
    browser: bool,
}
impl Drop for Process {
    fn drop(&mut self) {
        drop(self.input.take());
        let start = Instant::now();
        loop {
            if self.child.try_wait().ok().flatten().is_some() {
                break;
            }
            if start.elapsed() > Duration::from_secs(3) {
                let _ = self.child.kill();
                let _ = self.child.wait();
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
    }
}
impl Process {
    fn message(&mut self) -> Result<Value> {
        match self
            .messages
            .recv_timeout(Duration::from_secs(if self.browser { 180 } else { 900 }))
        {
            Ok(line) if !line.is_empty() => Ok(serde_json::from_str(&line)?),
            Ok(_) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                let code = self.child.wait()?;
                anyhow::bail!(
                    "Native {} worker exited ({code}); see its log",
                    if self.browser { "browser" } else { "render" }
                );
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.child.kill()?;
                self.child.wait()?;
                anyhow::bail!("Native worker exceeded its preparation/clip timeout");
            }
        }
    }
    fn start(app: &Store, profile: &str, browser: bool) -> Result<Self> {
        let p = app.profile(profile)?;
        for (path, expected) in p["overlays"].as_array().context("No overlays")?.iter().zip(
            p["overlay_hashes"]
                .as_array()
                .context("No overlay hashes")?,
        ) {
            ensure!(
                file_hash(path.as_str().context("Bad overlay path")?)?
                    == expected.as_str().context("Bad overlay hash")?,
                "Character archive changed; register a new version"
            );
        }
        let out = if browser {
            app.work
                .join("browser-workers")
                .join(profile)
                .join(format!("{}", (now() * 1e6) as u128))
        } else {
            app.work.join("renders").join(profile)
        };
        if !browser && out.join("prepared").exists() {
            let prepared = out.join("prepared").canonicalize()?;
            ensure!(
                prepared.starts_with(app.work.join("renders").canonicalize()?),
                "Preparation escaped the workspace"
            );
            let history = out.join("preparation-history");
            fs::create_dir_all(&history)?;
            fs::rename(prepared, history.join(format!("{}", (now() * 1e6) as u128)))?;
        }
        let exe = if browser {
            app.helper("browser_worker", "workspot_browser.exe")?
        } else {
            PathBuf::from(string(&app.config, "worker")?)
        };
        let mut c = command(exe);
        c.args([
            "--game",
            string(&app.config, "game")?,
            "--resource",
            string(&p, "entity")?,
            "--appearance",
            string(&p, "appearance")?,
            "--out",
        ])
        .arg(&out)
        .args(schema_args(&app.config))
        .args(["--fps", "30", "--seconds", "8", "--size", "512"]);
        for path in p["overlays"].as_array().context("No overlays")? {
            c.arg("--overlay")
                .arg(path.as_str().context("Bad overlay")?);
        }
        if let Some(anchors) = p["origin_anchors"].as_array() {
            for anchor in anchors {
                c.arg("--origin-anchor")
                    .arg(anchor.as_str().context("Bad origin anchor")?);
            }
        }
        if let Some(hair) = p["rigid_hair"].as_str().filter(|s| !s.is_empty()) {
            c.arg("--rigid-hair").arg(hair);
        }
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(app.work.join(if browser {
                "browser-worker.log"
            } else {
                "worker.log"
            }))?;
        let mut child = c
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(log)
            .spawn()?;
        let input = child.stdin.take().context("Native stdin absent")?;
        let output = child.stdout.take().context("Native stdout absent")?;
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                let Ok(line) = line else {
                    break;
                };
                if line.trim_start().starts_with('{') && tx.send(line).is_err() {
                    break;
                }
            }
            let _ = tx.send(String::new());
        });
        let mut process = Self {
            child,
            input: Some(input),
            messages: rx,
            profile: profile.into(),
            handled: 0,
            out,
            browser,
        };
        ensure!(
            process.message()?["ready"] == true,
            "Native worker did not become ready"
        );
        Ok(process)
    }
    fn request(&mut self, plan: &Value, clip: &str) -> Result<Value> {
        ensure!(
            !clip.is_empty() && clip.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
            "Invalid clip label"
        );
        let request = json!({"label":clip,"animset":plan["resolved_animset"],"animation":plan["animation_name"]});
        let input = self.input.as_mut().context("Native worker closed")?;
        writeln!(input, "{request}")?;
        input.flush()?;
        self.handled += 1;
        let result = self.message()?;
        ensure!(
            result["ok"] == true,
            "{}",
            result["error"].as_str().unwrap_or("Native request failed")
        );
        Ok(result["result"].clone())
    }
}

fn encode_video(app: &Store, job: &Value, result: Value) -> Result<Value> {
    let clip = string(job, "clip")?;
    let profile = string(job, "profile")?;
    let dir = app.work.join("renders").join(profile).join(clip);
    let mut frames = fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().and_then(|s| s.to_str()).is_some_and(|n| {
                n.len() == 10 && n.ends_with(".png") && n[..6].bytes().all(|c| c.is_ascii_digit())
            })
        })
        .collect::<Vec<_>>();
    frames.sort();
    ensure!(!frames.is_empty(), "No native frames were produced");
    let temporary = dir.join("preview.partial.mp4");
    checked(
        command("ffmpeg")
            .args(["-v", "error", "-y", "-framerate", "30", "-i"])
            .arg(dir.join("%06d.png"))
            .args([
                "-vf",
                "format=rgb24",
                "-c:v",
                "libx264",
                "-crf",
                "20",
                "-preset",
                "fast",
                "-pix_fmt",
                "yuv420p",
                "-movflags",
                "+faststart",
            ])
            .arg(&temporary),
    )?;
    let probe: Value = serde_json::from_slice(
        &checked(
            command("ffprobe")
                .args([
                    "-v",
                    "error",
                    "-count_frames",
                    "-select_streams",
                    "v:0",
                    "-show_entries",
                    "stream=avg_frame_rate,nb_read_frames,duration,width,height",
                    "-of",
                    "json",
                ])
                .arg(&temporary),
        )?
        .stdout,
    )?;
    let stream = &probe["streams"][0];
    ensure!(
        stream["avg_frame_rate"] == "30/1"
            && stream["nb_read_frames"]
                .as_str()
                .context("No decoded frame count")?
                .parse::<usize>()?
                == frames.len(),
        "Video failed FPS/frame-count validation"
    );
    checked(
        command("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&temporary)
            .args(["-f", "null", "-"]),
    )?;
    let video = dir.join("preview.mp4");
    fs::rename(&temporary, &video)?;
    fs::copy(&frames[0], dir.join("poster.png"))?;
    let db = app.db(false)?;
    let plan = first(&db, "SELECT plan FROM clips WHERE id=?", &[&clip])?;
    let receipt = json!({"receipt_version":"rust-v1","clip":clip,"profile":profile,"settings":app.settings,"video_sha256":file_hash(&video)?,"probe":stream,"source_plan_sha256":hash(string(&plan,"plan")?.as_bytes()),"render":result,"finished":now()});
    atomic_json(&dir.join("receipt.json"), &receipt)?;
    let canonical = dir.canonicalize()?;
    ensure!(
        canonical.starts_with(app.work.join("renders").canonicalize()?),
        "Frame directory escaped workspace"
    );
    for path in frames {
        ensure!(
            path.canonicalize()?.parent() == Some(canonical.as_path()),
            "Unexpected frame path"
        );
        fs::remove_file(path)?;
    }
    Ok(receipt)
}

fn prepare_browser(
    app: &Store,
    job: &Value,
    process: &mut Process,
    result: Value,
) -> Result<Value> {
    let clip = string(job, "clip")?;
    let key = string(job, "key")?;
    let file = process.out.join(clip).join("scene.glb");
    let bytes = fs::read(&file)?;
    ensure!(
        bytes.len() >= 12
            && &bytes[..4] == b"glTF"
            && u32::from_le_bytes(bytes[8..12].try_into()?) as usize == bytes.len(),
        "Invalid GLB container"
    );
    ensure!(
        file_hash(&file)? == string(&result, "glb_sha256")?,
        "GLB failed hash validation"
    );
    let dir = app.work.join("browser").join(key);
    fs::create_dir_all(&dir)?;
    fs::rename(file, dir.join("scene.glb"))?;
    let mut receipt = result;
    receipt["clip"] = json!(clip);
    receipt["profile"] = job["profile"].clone();
    receipt["exporter_sha256"] = json!(file_hash(
        app.helper("browser_worker", "workspot_browser.exe")?
    )?);
    receipt["finished"] = json!(now());
    atomic_json(&dir.join("receipt.json"), &receipt)?;
    Ok(receipt)
}

pub fn spawn(app: Arc<Store>, browser: bool) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut process: Option<Process> = None;
        let mut idle = Instant::now();
        while !app.closed.load(Ordering::Acquire) {
            if !browser && app.paused.load(Ordering::Acquire) {
                drop(process.take());
                thread::sleep(Duration::from_millis(250));
                continue;
            }
            let next = (|| -> Result<Option<Value>> {
                let db = app.db(false)?;
                let jobs = if browser {
                    rows(
                        &db,
                        "SELECT * FROM browser_previews WHERE state='queued' ORDER BY created LIMIT 1",
                        &[],
                    )?
                } else {
                    rows(
                        &db,
                        "SELECT * FROM jobs WHERE state='queued' ORDER BY priority DESC,profile=? DESC,profile,id LIMIT 1",
                        &[&process.as_ref().map_or("", |p| p.profile.as_str())],
                    )?
                };
                let Some(job) = jobs.into_iter().next() else {
                    return Ok(None);
                };
                if browser {
                    db.execute(
                        "UPDATE browser_previews SET state='preparing' WHERE key=?",
                        [string(&job, "key")?],
                    )?;
                } else {
                    db.execute(
                        "UPDATE jobs SET state='running',started=? WHERE id=?",
                        rusqlite::params![now(), job["id"].as_i64()],
                    )?;
                }
                Ok(Some(job))
            })();
            let job = match next {
                Ok(Some(job)) => job,
                Ok(None) => {
                    if !browser || idle.elapsed() > Duration::from_secs(10) {
                        drop(process.take());
                    }
                    if !browser
                        && !app.work.join("completion.json").exists()
                        && let Ok(status) = app.status()
                    {
                        let counts = &status["counts"];
                        if counts["queued"].as_i64().unwrap_or(0) == 0
                            && counts["running"].as_i64().unwrap_or(0) == 0
                            && counts.as_object().is_some_and(|m| !m.is_empty())
                        {
                            let _ = atomic_json(
                                &app.work.join("completion.json"),
                                &json!({"finished":now(),"counts":counts,"coverage":status["coverage"],"settings":app.settings,"gpu_worker_exited":true,"backend":"rust"}),
                            );
                        }
                    }
                    thread::sleep(Duration::from_millis(500));
                    continue;
                }
                Err(error) => {
                    *app.error.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(format!("{error:#}"));
                    app.paused.store(true, Ordering::Release);
                    thread::sleep(Duration::from_secs(1));
                    continue;
                }
            };
            idle = Instant::now();
            if !browser {
                *app.current.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some(json!({"clip":job["clip"],"profile":job["profile"],"started":now()}));
            }
            let result = (|| -> Result<Value> {
                let profile = string(&job, "profile")?;
                if process.as_mut().is_none_or(|p| {
                    p.profile != profile
                        || p.handled >= if browser { 4 } else { 8 }
                        || p.child.try_wait().ok().flatten().is_some()
                }) {
                    drop(process.take());
                    process = Some(Process::start(&app, profile, browser)?);
                }
                let db = app.db(false)?;
                let plan = first(
                    &db,
                    "SELECT plan FROM clips WHERE id=?",
                    &[&string(&job, "clip")?],
                )?;
                let plan: Value = serde_json::from_str(string(&plan, "plan")?)?;
                let p = process.as_mut().context("Worker not started")?;
                let result = p.request(&plan, string(&job, "clip")?)?;
                if browser {
                    prepare_browser(&app, &job, p, result)
                } else {
                    encode_video(&app, &job, result)
                }
            })();
            let update = (|| -> Result<()> {
                let db = app.db(false)?;
                match result {
                    Ok(receipt) => {
                        if browser {
                            db.execute("UPDATE browser_previews SET state='ready',receipt=?,error=NULL WHERE key=?",[receipt.to_string(),string(&job,"key")?.to_owned()])?;
                        } else {
                            db.execute("UPDATE jobs SET state='complete',finished=?,receipt=?,error=NULL WHERE id=?",rusqlite::params![now(),receipt.to_string(),job["id"].as_i64()])?;
                        }
                    }
                    Err(error) => {
                        let text = format!("{error:#}");
                        if browser {
                            db.execute(
                                "UPDATE browser_previews SET state='failed',error=? WHERE key=?",
                                [&text, string(&job, "key")?],
                            )?;
                            drop(process.take());
                        } else {
                            db.execute(
                                "UPDATE jobs SET state='failed',finished=?,error=? WHERE id=?",
                                rusqlite::params![now(), text, job["id"].as_i64()],
                            )?;
                            let mut log = fs::OpenOptions::new()
                                .create(true)
                                .append(true)
                                .open(app.work.join("queue.log"))?;
                            writeln!(
                                log,
                                "{}",
                                json!({"job":job["id"],"clip":job["clip"],"error":text,"time":now(),"backend":"rust"})
                            )?;
                            if process
                                .as_mut()
                                .is_none_or(|p| p.child.try_wait().ok().flatten().is_some())
                            {
                                *app.error.lock().unwrap_or_else(|e| e.into_inner()) = Some(text);
                                app.paused.store(true, Ordering::Release);
                                drop(process.take());
                            }
                        }
                    }
                }
                Ok(())
            })();
            if let Err(error) = update {
                *app.error.lock().unwrap_or_else(|e| e.into_inner()) = Some(format!("{error:#}"));
                app.paused.store(true, Ordering::Release);
            }
            if !browser {
                *app.current.lock().unwrap_or_else(|e| e.into_inner()) = None;
            }
        }
    })
}

impl Store {
    pub fn browser_get(&self, key: &str) -> Result<Value> {
        let db = self.db(false)?;
        let mut r = first(&db, "SELECT * FROM browser_previews WHERE key=?", &[&key])?;
        if let Some(raw) = r["receipt"].as_str() {
            r["receipt"] = serde_json::from_str(raw)?;
        }
        r["scene"] = if r["state"] == "ready" {
            json!(format!("/media/browser/{key}/scene.glb"))
        } else {
            Value::Null
        };
        Ok(r)
    }
    pub fn browser_request(&self, clip: &str, profile: &str, retry: bool) -> Result<Value> {
        let _guard = self.browser_lock.lock().unwrap_or_else(|e| e.into_inner());
        let db = self.db(false)?;
        let c = first(&db, "SELECT rig,plan FROM clips WHERE id=?", &[&clip])?;
        let p = first(&db, "SELECT rig FROM profiles WHERE id=?", &[&profile])?;
        ensure!(
            c["rig"] == p["rig"],
            "Character and animation rigs must match"
        );
        let identity = file_hash(self.helper("browser_worker", "workspot_browser.exe")?)?;
        for old in rows(
            &db,
            "SELECT * FROM browser_previews WHERE clip=? AND profile=? ORDER BY created DESC",
            &[&clip, &profile],
        )? {
            let state = old["state"].as_str().unwrap_or_default();
            if matches!(state, "queued" | "preparing") {
                return self.browser_get(string(&old, "key")?);
            }
            if state == "ready" {
                let receipt: Value = serde_json::from_str(string(&old, "receipt")?)?;
                let path = self
                    .work
                    .join("browser")
                    .join(string(&old, "key")?)
                    .join("scene.glb");
                if receipt["exporter_sha256"] == identity
                    && path.is_file()
                    && file_hash(path)? == string(&receipt, "glb_sha256")?
                {
                    return self.browser_get(string(&old, "key")?);
                }
            }
        }
        let key = hash(
            json!([clip, profile, identity, c["plan"]])
                .to_string()
                .as_bytes(),
        )[..32]
            .to_owned();
        let found = rows(
            &db,
            "SELECT state FROM browser_previews WHERE key=?",
            &[&key],
        )?;
        if found.is_empty() {
            let count: i64 = db.query_row(
                "SELECT count(*) FROM browser_previews WHERE state IN('queued','preparing')",
                [],
                |r| r.get(0),
            )?;
            ensure!(
                count < 4,
                "Four previews are pending; wait for one to finish"
            );
            db.execute(
                "INSERT INTO browser_previews VALUES(?,?,?,'queued',NULL,NULL,?)",
                rusqlite::params![key, clip, profile, now()],
            )?;
        } else if (retry && found[0]["state"] == "failed") || found[0]["state"] == "ready" {
            db.execute(
                "UPDATE browser_previews SET state='queued',error=NULL,receipt=NULL WHERE key=?",
                [&key],
            )?;
        }
        self.browser_get(&key)
    }
}
