"""Local workspot catalogue and durable, single-GPU render queue."""

from __future__ import annotations

import argparse
from contextlib import contextmanager
import hashlib
import json
import mimetypes
import os
from pathlib import Path
import re
import queue
import sqlite3
import subprocess
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, unquote, urlparse
from browser_preview import BrowserPreviews
from archive_discovery import ArchiveDiscovery

ROOT = Path(__file__).resolve().parent


def digest(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def atomic_json(path, value):
    path = Path(path)
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(value, indent=2), encoding="utf8")
    os.replace(temporary, path)


def queue_lease(work):
    """Hold an OS lock for the lifetime of the queue owner, including on Windows."""
    work = Path(work).resolve()
    work.mkdir(parents=True, exist_ok=True)
    lease = open(work / "queue.lock", "a+b")
    lease.seek(0)
    if os.fstat(lease.fileno()).st_size == 0:
        lease.write(b"0")
        lease.flush()
    lease.seek(0)
    try:
        if os.name == "nt":
            import msvcrt

            msvcrt.locking(lease.fileno(), msvcrt.LK_NBLCK, 1)
        else:
            import fcntl

            fcntl.flock(lease.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        lease.close()
        raise RuntimeError("Another service already owns this work directory") from None
    return lease


def archive_preparation(output, work):
    """Preserve prior generated preparation while giving the renderer a fresh directory."""
    prepared = (Path(output) / "prepared").resolve()
    if not prepared.exists():
        return
    destination = (Path(output) / "preparation-history" / str(time.time_ns())).resolve()
    root = (Path(work) / "renders").resolve()
    if not prepared.is_relative_to(root) or not destination.is_relative_to(root):
        raise ValueError("Preparation paths escaped the render workspace")
    destination.parent.mkdir(parents=True, exist_ok=True)
    os.replace(prepared, destination)


class Store:
    def __init__(self, catalogue, config, work):
        self.catalogue, self.config, self.work = (
            Path(catalogue).resolve(),
            config,
            Path(work).resolve(),
        )
        self.work.mkdir(parents=True, exist_ok=True)
        self.db_path = self.work / "workspots.sqlite3"
        self.worker_hash = digest(config["worker"])
        self.settings = {
            "fps": 30,
            "seconds": 8,
            "size": 512,
            "worker_sha256": self.worker_hash,
        }
        with self.db() as db:
            db.executescript("""
                PRAGMA journal_mode=WAL;
                CREATE TABLE IF NOT EXISTS clips(id TEXT PRIMARY KEY,animation TEXT,rig TEXT,plan TEXT);
                CREATE TABLE IF NOT EXISTS profiles(id TEXT PRIMARY KEY,name TEXT,rig TEXT,config TEXT);
                CREATE TABLE IF NOT EXISTS jobs(id INTEGER PRIMARY KEY,clip TEXT,profile TEXT,state TEXT,
                  priority INTEGER DEFAULT 0,created REAL,started REAL,finished REAL,error TEXT,
                  receipt TEXT,UNIQUE(clip,profile));
                CREATE INDEX IF NOT EXISTS job_queue ON jobs(state,priority,profile,id);
                CREATE TABLE IF NOT EXISTS metadata(key TEXT PRIMARY KEY,value TEXT);
                CREATE TABLE IF NOT EXISTS source_uses(clip TEXT,workspot TEXT,role TEXT,entry_id INTEGER,field TEXT);
                CREATE INDEX IF NOT EXISTS source_workspot ON source_uses(workspot);
            """)
            plan_path = self.catalogue / "full-batch/plan.json"
            identity = digest(plan_path)
            old = db.execute(
                "SELECT value FROM metadata WHERE key='plan_sha256'"
            ).fetchone()
            if old and old[0] != identity:
                raise ValueError(
                    "Canonical plan changed; use a new work directory to preserve render provenance"
                )
            plan = json.loads(plan_path.read_text(encoding="utf8"))
            db.executemany(
                "INSERT OR IGNORE INTO clips VALUES(?,?,?,?)",
                [
                    (
                        j["id"],
                        j["animation_name"],
                        "female" if "woman_base" in j["rig"] else "male",
                        json.dumps(j),
                    )
                    for j in plan["jobs"]
                ],
            )
            if not db.execute("SELECT 1 FROM source_uses LIMIT 1").fetchone():
                db.executemany(
                    "INSERT INTO source_uses VALUES(?,?,?,?,?)",
                    [
                        (j["id"], u["workspot"], u["role"], u["entry_id"], u["field"])
                        for j in plan["jobs"]
                        for u in j["uses"]
                    ],
                )
            coverage = {
                "resolved": len(plan["jobs"]),
                "unresolved": len(plan["unresolved"]),
                "skipped": len(plan["skipped"]),
                "workspots": 3881,
            }
            db.execute(
                "INSERT OR REPLACE INTO metadata VALUES(?,?)", ("plan_sha256", identity)
            )
            db.execute(
                "INSERT OR REPLACE INTO metadata VALUES(?,?)",
                ("coverage", json.dumps(coverage)),
            )
            # A process exit leaves a resumable job, never an invented success.
            db.execute(
                "UPDATE jobs SET state='queued',started=NULL WHERE state='running'"
            )
            for job in db.execute(
                "SELECT id,clip,profile,receipt FROM jobs WHERE state='complete'"
            ).fetchall():
                video = (
                    self.work / "renders" / job["profile"] / job["clip"] / "preview.mp4"
                )
                receipt = json.loads(job["receipt"]) if job["receipt"] else {}
                if not video.is_file() or digest(video) != receipt.get("video_sha256"):
                    db.execute(
                        "UPDATE jobs SET state='queued',receipt=NULL,error='Missing or changed video; re-rendering' WHERE id=?",
                        (job["id"],),
                    )
        self.defaults = [self.register(p) for p in config["characters"]]
        self.current = None
        self.health_error = None
        self.paused = threading.Event()
        self.thread = None
        self.control_lock = threading.Lock()

    def control(self, action):
        with self.control_lock:
            if action == "pause":
                self.paused.set()
            elif action == "resume":
                self.paused.clear()
                self.health_error = None
                if not self.thread or not self.thread.is_alive():
                    self.thread = threading.Thread(target=self.worker.run, daemon=True)
                    self.thread.start()
            else:
                raise ValueError("Expected pause or resume")
        return {"paused": self.paused.is_set()}

    @contextmanager
    def db(self, sources=False):
        db = sqlite3.connect(self.db_path, timeout=30, uri=True)
        db.row_factory = sqlite3.Row
        try:
            if sources:
                for name, path in [
                    (
                        "captions",
                        self.catalogue / "caption-results/caption-search.sqlite3",
                    ),
                    ("catalogue", self.catalogue / "workspots.sqlite3"),
                ]:
                    db.execute(
                        f"ATTACH DATABASE ? AS {name}", (path.as_uri() + "?mode=ro",)
                    )
            with db:
                yield db
        finally:
            db.close()

    def register(self, profile):
        profile = dict(profile)
        expected_archive_hash = profile.pop("archive_sha256", None)
        if profile.get("rig") not in ("male", "female"):
            raise ValueError("Choose the character’s male or female source rig")
        if not profile.get("name", "").strip() or len(profile["name"]) > 100:
            raise ValueError("A character name is required (up to 100 characters)")
        for field in ("entity", "appearance"):
            if not isinstance(profile.get(field), str) or not profile[field].strip():
                raise ValueError(f"{field} is required")
        entity_hash = (
            profile["entity"].isascii()
            and profile["entity"].isdigit()
            and 0 < int(profile["entity"]) < 2**64
        )
        if not entity_hash and not profile["entity"].replace(
            "\\", "/"
        ).lower().endswith(".ent"):
            raise ValueError(
                "Entity must be a game depot .ent resource or discovered native hash"
            )
        if ".." in profile["entity"] or ":" in profile["entity"]:
            raise ValueError("Entity must be a depot path, not a filesystem path")
        overlays = profile.get("overlays", [])
        if not isinstance(overlays, list) or len(overlays) > 8:
            raise ValueError("At most eight explicit archive overlays are supported")
        profile["overlays"] = [str(Path(p).resolve()) for p in overlays]
        profile["overlay_hashes"] = [digest(p) for p in profile["overlays"]]
        if expected_archive_hash and profile["overlay_hashes"] != [
            expected_archive_hash
        ]:
            raise ValueError(
                "Archive changed after discovery; inspect it again before registering"
            )
        project = profile.get("project")
        if project:
            project = Path(project).resolve()
            authored = json.loads(project.read_text(encoding="utf8"))
            if authored.get("refit_clothing", False):
                raise ValueError(
                    "Refitted character rejected: rebuild with refit_clothing=false"
                )
            profile["project"] = str(project)
            profile["project_sha256"] = digest(project)
        profile["refit_clothing"] = False
        profile["settings"] = self.settings
        identity = hashlib.sha256(
            json.dumps(profile, sort_keys=True).encode()
        ).hexdigest()[:24]
        with self.db() as db:
            db.execute(
                "INSERT OR IGNORE INTO profiles VALUES(?,?,?,?)",
                (identity, profile["name"], profile["rig"], json.dumps(profile)),
            )
        return identity

    def bulk(self):
        with self.db() as db:
            for identity in self.defaults:
                rig = db.execute(
                    "SELECT rig FROM profiles WHERE id=?", (identity,)
                ).fetchone()[0]
                db.execute(
                    """INSERT OR IGNORE INTO jobs(clip,profile,state,priority,created)
                    SELECT id,?,'queued',0,? FROM clips WHERE rig=?""",
                    (identity, time.time(), rig),
                )

    def enqueue(self, clip, profile, retry=False):
        with self.db() as db:
            a = db.execute("SELECT rig FROM clips WHERE id=?", (clip,)).fetchone()
            b = db.execute("SELECT rig FROM profiles WHERE id=?", (profile,)).fetchone()
            if not a or not b or a[0] != b[0]:
                raise ValueError("Character and animation rigs must match")
            db.execute(
                "INSERT OR IGNORE INTO jobs(clip,profile,state,priority,created) VALUES(?,?,'queued',100,?)",
                (clip, profile, time.time()),
            )
            db.execute(
                "UPDATE jobs SET priority=100 WHERE clip=? AND profile=?",
                (clip, profile),
            )
            if retry:
                db.execute(
                    "UPDATE jobs SET state='queued',error=NULL WHERE clip=? AND profile=? AND state='failed'",
                    (clip, profile),
                )
            return dict(
                db.execute(
                    "SELECT * FROM jobs WHERE clip=? AND profile=?", (clip, profile)
                ).fetchone()
            )

    def status(self):
        with self.db(sources=True) as db:
            counts = dict(
                db.execute("SELECT state,count(*) FROM jobs GROUP BY state").fetchall()
            )
            coverage = json.loads(
                db.execute(
                    "SELECT value FROM metadata WHERE key='coverage'"
                ).fetchone()[0]
            )
            profiles = [
                dict(p) for p in db.execute("SELECT id,name,rig,config FROM profiles")
            ]
            recent = [
                dict(j)
                for j in db.execute("""SELECT j.id,j.clip,c.animation,p.name AS character,
                j.state,j.error,j.finished FROM jobs j JOIN clips c ON c.id=j.clip
                JOIN profiles p ON p.id=j.profile ORDER BY coalesce(j.finished,j.started,j.created) DESC LIMIT 12""")
            ]
            timings = db.execute(
                "SELECT started,finished FROM jobs WHERE state='complete' ORDER BY finished DESC LIMIT 30"
            ).fetchall()
            rate = (
                sum(t["finished"] - t["started"] for t in timings) / len(timings)
                if timings
                else None
            )
            coverage["workspots"] = db.execute(
                "SELECT count(*) FROM catalogue.workspots"
            ).fetchone()[0]
        return {
            "counts": counts,
            "coverage": coverage,
            "profiles": profiles,
            "recent": recent,
            "current": self.current,
            "seconds_per_clip": rate,
            "eta_seconds": rate * counts.get("queued", 0) if rate else None,
            "settings": self.settings,
            "error": self.health_error,
            "paused": self.paused.is_set(),
        }

    def search(self, query, rig, state, page, kind="clips"):
        page = max(0, min(page, 10000))
        size = 35
        with self.db(sources=True) as db:
            if kind == "workspots":
                where, params = (
                    (
                        "WHERE w.depot_path LIKE ? OR w.label LIKE ?",
                        ["%" + query + "%"] * 2,
                    )
                    if query
                    else ("", [])
                )
                total = db.execute(
                    f"SELECT count(*) FROM catalogue.workspots w {where}", params
                ).fetchone()[0]
                rows = [
                    dict(r)
                    for r in db.execute(
                        f"""SELECT w.depot_path,w.label,w.animation_count,w.review_status,
                    (SELECT count(DISTINCT a.clip) FROM source_uses a WHERE a.workspot=w.depot_path) AS resolved_clips
                    FROM catalogue.workspots w {where} ORDER BY w.depot_path LIMIT ? OFFSET ?""",
                        params + [size, page * size],
                    )
                ]
                return {"items": rows, "total": total, "page": page, "size": size}
            where, params = [], []
            if query:
                tokens = re.findall(r"[\w]+", query, flags=re.UNICODE)[:20]
                if tokens:
                    fts = " AND ".join('"' + t + '"*' for t in tokens)
                    where.append("""(c.id IN (SELECT clip_id FROM captions.caption_search WHERE caption_search MATCH ?)
                        OR c.animation LIKE ? OR c.id=? OR c.id IN
                        (SELECT clip FROM source_uses WHERE workspot=?))""")
                    params += [fts, "%" + query + "%", query, query]
            if rig:
                where.append("c.rig=?")
                params.append(rig)
            if state:
                if state == "unrendered":
                    where.append(
                        "NOT EXISTS(SELECT 1 FROM jobs j WHERE j.clip=c.id AND j.state='complete')"
                    )
                else:
                    where.append(
                        "EXISTS(SELECT 1 FROM jobs j WHERE j.clip=c.id AND j.state=?)"
                    )
                    params.append(state)
            clause = "WHERE " + " AND ".join(where) if where else ""
            total = db.execute(
                f"SELECT count(*) FROM clips c {clause}", params
            ).fetchone()[0]
            rows = [
                dict(r)
                for r in db.execute(
                    f"""SELECT c.id,c.animation,c.rig,
                json_array_length(c.plan,'$.uses') AS uses,
                (SELECT state FROM jobs j WHERE j.clip=c.id ORDER BY state='complete' DESC,id LIMIT 1) AS state
                FROM clips c {clause} ORDER BY c.animation,c.id LIMIT ? OFFSET ?""",
                    params + [size, page * size],
                )
            ]
            return {"items": rows, "total": total, "page": page, "size": size}

    def detail(self, clip):
        with self.db(sources=True) as db:
            row = db.execute("SELECT * FROM clips WHERE id=?", (clip,)).fetchone()
            if not row:
                raise ValueError("Unknown clip")
            result = dict(row)
            result["plan"] = json.loads(result["plan"])
            caption = db.execute(
                "SELECT caption_json,review_status FROM captions.captions WHERE id=?",
                (clip,),
            ).fetchone()
            result["caption"] = json.loads(caption[0]) if caption else None
            result["review_status"] = caption[1] if caption else "uncaptioned"
            result["renders"] = [
                dict(r)
                for r in db.execute(
                    """SELECT j.*,p.name AS character FROM jobs j
                JOIN profiles p ON p.id=j.profile WHERE clip=? ORDER BY j.id""",
                    (clip,),
                )
            ]
            for r in result["renders"]:
                r["receipt"] = json.loads(r["receipt"]) if r["receipt"] else None
                r["video"] = (
                    f"/media/renders/{r['profile']}/{clip}/preview.mp4"
                    if r["state"] == "complete"
                    else None
                )
            result["original_video"] = (
                f"/media/original/{clip}/preview.mp4"
                if (self.catalogue / "previews" / clip / "preview.mp4").is_file()
                else None
            )
            return result


class Worker:
    def __init__(self, store):
        self.store, self.process, self.profile = store, None, None
        self.closed = threading.Event()
        self.handled = 0

    def stop_process(self):
        if self.process:
            self.process.stdin.close()
            try:
                self.process.wait(timeout=30)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
            self.process = None
        self.profile = None

    def read_message(self):
        while True:
            try:
                line = self.messages.get(timeout=900)
            except queue.Empty:
                self.process.kill()
                self.process.wait()
                raise RuntimeError(
                    "Renderer exceeded the 15-minute startup/clip timeout; queue paused"
                )
            if not line:
                code = self.process.wait(timeout=5)
                log = self.store.work / "worker.log"
                with open(log, "rb") as f:
                    f.seek(max(0, log.stat().st_size - 1800))
                    tail = f.read().decode("utf8", errors="replace")
                raise RuntimeError(f"Render worker exited ({code}): {tail}")
            try:
                return json.loads(line)
            except json.JSONDecodeError:
                continue

    def start(self, profile_id):
        self.stop_process()
        s = self.store
        with s.db() as db:
            profile = json.loads(
                db.execute(
                    "SELECT config FROM profiles WHERE id=?", (profile_id,)
                ).fetchone()[0]
            )
        for path, expected in zip(profile["overlays"], profile["overlay_hashes"]):
            if digest(path) != expected:
                raise ValueError(
                    "Character overlay changed since registration; register a new version"
                )
        output = s.work / "renders" / profile_id
        archive_preparation(output, s.work)
        args = [
            s.config["worker"],
            "--game",
            s.config["game"],
            "--schema",
            s.config["schema"],
            "--resource",
            profile["entity"],
            "--appearance",
            profile["appearance"],
            "--out",
            str(output),
            "--fps",
            "30",
            "--seconds",
            "8",
            "--size",
            "512",
        ]
        for archive in profile["overlays"]:
            args += ["--overlay", archive]
        for anchor in profile.get("origin_anchors", []):
            args += ["--origin-anchor", anchor]
        if profile.get("rigid_hair"):
            args += ["--rigid-hair", profile["rigid_hair"]]
        with open(s.work / "worker.log", "a", encoding="utf8") as log:
            self.process = subprocess.Popen(
                args,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=log,
                text=True,
                encoding="utf8",
                bufsize=1,
            )
        self.messages = queue.Queue()

        def read_output(process, messages):
            for line in process.stdout:
                messages.put(line)
            messages.put("")

        threading.Thread(
            target=read_output, args=(self.process, self.messages), daemon=True
        ).start()
        message = self.read_message()
        if not message.get("ready"):
            raise RuntimeError(str(message))
        self.profile = profile_id
        self.handled = 0

    def render(self, job):
        s = self.store
        if (
            self.profile != job["profile"]
            or self.process.poll() is not None
            or self.handled >= 8
        ):
            self.start(job["profile"])
        self.handled += 1
        with s.db() as db:
            plan = json.loads(
                db.execute(
                    "SELECT plan FROM clips WHERE id=?", (job["clip"],)
                ).fetchone()[0]
            )
        self.process.stdin.write(
            json.dumps(
                {
                    "label": job["clip"],
                    "animset": plan["resolved_animset"],
                    "animation": plan["animation_name"],
                }
            )
            + "\n"
        )
        self.process.stdin.flush()
        result = self.read_message()
        if not result.get("ok"):
            raise RuntimeError(result.get("error", str(result)))
        directory = s.work / "renders" / job["profile"] / job["clip"]
        frames = sorted(directory.glob("[0-9][0-9][0-9][0-9][0-9][0-9].png"))
        if not frames:
            raise RuntimeError("Renderer produced no frames")
        temporary = directory / "preview.partial.mp4"
        subprocess.run(
            [
                "ffmpeg",
                "-v",
                "error",
                "-y",
                "-framerate",
                "30",
                "-i",
                str(directory / "%06d.png"),
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
                str(temporary),
            ],
            check=True,
            timeout=300,
        )
        probe = json.loads(
            subprocess.check_output(
                [
                    "ffprobe",
                    "-v",
                    "error",
                    "-count_frames",
                    "-select_streams",
                    "v:0",
                    "-show_entries",
                    "stream=avg_frame_rate,nb_read_frames,duration,width,height",
                    "-of",
                    "json",
                    str(temporary),
                ],
                text=True,
            )
        )["streams"][0]
        if probe["avg_frame_rate"] != "30/1" or int(probe["nb_read_frames"]) != len(
            frames
        ):
            raise RuntimeError("Encoded video failed FPS/frame-count validation")
        # Decode the encoded result as well as checking its metadata before deleting frame PNGs.
        subprocess.run(
            ["ffmpeg", "-v", "error", "-i", str(temporary), "-f", "null", "-"],
            check=True,
            timeout=300,
        )
        video = directory / "preview.mp4"
        os.replace(temporary, video)
        from shutil import copyfile

        copyfile(frames[0], directory / "poster.png")
        receipt = {
            "clip": job["clip"],
            "profile": job["profile"],
            "settings": s.settings,
            "video_sha256": digest(video),
            "probe": probe,
            "source_plan_sha256": hashlib.sha256(
                json.dumps(plan, sort_keys=True).encode()
            ).hexdigest(),
            "render": result["result"],
            "finished": time.time(),
        }
        atomic_json(directory / "receipt.json", receipt)
        target = directory.resolve()
        if not target.is_relative_to((s.work / "renders").resolve()):
            raise RuntimeError("Unexpected frame directory")
        for frame in frames:
            if frame.resolve().parent != target:
                raise RuntimeError("Unexpected frame path")
            frame.unlink()
        return receipt

    def run(self):
        s = self.store
        try:
            while not self.closed.is_set():
                if s.paused.is_set():
                    self.stop_process()
                    self.closed.wait(1)
                    continue
                with s.db() as db:
                    row = db.execute(
                        """SELECT * FROM jobs WHERE state='queued'
                        ORDER BY priority DESC,profile=? DESC,profile,id LIMIT 1""",
                        (self.profile or "",),
                    ).fetchone()
                    if row:
                        job = dict(row)
                        db.execute(
                            "UPDATE jobs SET state='running',started=? WHERE id=?",
                            (time.time(), job["id"]),
                        )
                if not row:
                    self.stop_process()
                    if not (s.work / "completion.json").exists():
                        with s.db() as db:
                            counts = dict(
                                db.execute(
                                    "SELECT state,count(*) FROM jobs GROUP BY state"
                                )
                            )
                        if (
                            counts
                            and not counts.get("queued")
                            and not counts.get("running")
                        ):
                            atomic_json(
                                s.work / "completion.json",
                                {
                                    "finished": time.time(),
                                    "counts": counts,
                                    "coverage": s.status()["coverage"],
                                    "settings": s.settings,
                                    "gpu_worker_exited": True,
                                },
                            )
                    self.closed.wait(2)
                    continue
                s.current = {
                    "clip": job["clip"],
                    "profile": job["profile"],
                    "started": time.time(),
                }
                try:
                    receipt = self.render(job)
                    with s.db() as db:
                        db.execute(
                            "UPDATE jobs SET state='complete',finished=?,receipt=?,error=NULL WHERE id=?",
                            (time.time(), json.dumps(receipt), job["id"]),
                        )
                    s.health_error = None
                except Exception as error:
                    with s.db() as db:
                        db.execute(
                            "UPDATE jobs SET state='failed',finished=?,error=? WHERE id=?",
                            (time.time(), str(error), job["id"]),
                        )
                    with open(s.work / "queue.log", "a", encoding="utf8") as log:
                        log.write(
                            json.dumps(
                                {
                                    "job": job["id"],
                                    "clip": job["clip"],
                                    "error": str(error),
                                    "time": time.time(),
                                }
                            )
                            + "\n"
                        )
                    # An engine/startup failure affects every clip: pause instead of burning through the queue.
                    if (
                        not self.process
                        or self.process.poll() is not None
                        or self.profile is None
                    ):
                        s.health_error = str(error)
                        self.stop_process()
                        break
                finally:
                    s.current = None
        finally:
            self.stop_process()


def handler(store):
    class Handler(BaseHTTPRequestHandler):
        def json(self, value, status=200):
            data = json.dumps(value).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self):
            parsed = urlparse(self.path)
            path = unquote(parsed.path)
            q = parse_qs(parsed.query)
            try:
                if path == "/api/status":
                    return self.json(store.status())
                if path == "/api/clips":
                    return self.json(
                        store.search(
                            q.get("q", [""])[0],
                            q.get("rig", [""])[0],
                            q.get("state", [""])[0],
                            int(q.get("page", ["0"])[0]),
                            q.get("kind", ["clips"])[0],
                        )
                    )
                if path.startswith("/api/clips/"):
                    return self.json(store.detail(path.rsplit("/", 1)[-1]))
                if path.startswith("/api/browser/"):
                    return self.json(store.previews.get(path.rsplit("/", 1)[-1]))
                if path.startswith("/media/browser/"):
                    base = store.work / "browser"
                    relative = path[len("/media/browser/") :]
                elif path.startswith("/media/renders/"):
                    base = store.work / "renders"
                    relative = path[len("/media/renders/") :]
                elif path.startswith("/media/original/"):
                    base = store.catalogue / "previews"
                    relative = path[len("/media/original/") :]
                else:
                    base = ROOT / "ui/dist"
                    relative = path.lstrip("/") or "index.html"
                file = (base / relative).resolve()
                if not file.is_relative_to(base.resolve()) or not file.is_file():
                    return self.json({"error": "Not found"}, 404)
                size = file.stat().st_size
                start = 0
                end = size - 1
                partial = False
                match = re.fullmatch(
                    r"bytes=(\d+)-(\d*)", self.headers.get("Range", "")
                )
                if match:
                    start = int(match[1])
                    end = min(int(match[2]) if match[2] else size - 1, size - 1)
                    partial = True
                    if start > end or start >= size:
                        self.send_response(416)
                        self.send_header("Content-Range", f"bytes */{size}")
                        self.end_headers()
                        return
                self.send_response(206 if partial else 200)
                self.send_header(
                    "Content-Type",
                    mimetypes.guess_type(str(file))[0] or "application/octet-stream",
                )
                self.send_header("Content-Length", str(end - start + 1))
                self.send_header("Accept-Ranges", "bytes")
                if partial:
                    self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
                self.end_headers()
                with open(file, "rb") as f:
                    f.seek(start)
                    remaining = end - start + 1
                    while remaining:
                        block = f.read(min(1024 * 1024, remaining))
                        if not block:
                            break
                        self.wfile.write(block)
                        remaining -= len(block)
            except (BrokenPipeError, ConnectionResetError):
                pass
            except (ValueError, sqlite3.Error) as e:
                self.json({"error": str(e)}, 400)

        def do_POST(self):
            # Loopback binding plus strict Origin/Host checks prevent another website queuing local renders.
            origin = self.headers.get("Origin")
            if self.headers.get("Host") not in (
                f"127.0.0.1:{self.server.server_port}",
                f"localhost:{self.server.server_port}",
            ) or (origin and urlparse(origin).netloc != self.headers.get("Host")):
                return self.json({"error": "Cross-origin request rejected"}, 403)
            try:
                length = int(self.headers.get("Content-Length", "0"))
                if urlparse(self.path).path == "/api/archive-upload":
                    self.connection.settimeout(90)
                    name = parse_qs(urlparse(self.path).query).get("filename", [""])[0]
                    return self.json(store.discovery.upload(self.rfile, length, name))
                if length < 1 or length > 65536:
                    raise ValueError("Expected a JSON request under 64 KiB")
                data = json.loads(self.rfile.read(length))
                if self.path == "/api/characters":
                    return self.json({"id": store.register(data)})
                if self.path == "/api/archive-discover":
                    return self.json(store.discovery.inspect(data["archive"]))
                if self.path == "/api/archive-resolve":
                    return self.json(
                        store.discovery.resolve(
                            data["archive"],
                            data["archive_sha256"],
                            data["entity"],
                            data["appearance"],
                        )
                    )
                if self.path == "/api/render":
                    return self.json(
                        store.enqueue(
                            data["clip"], data["profile"], bool(data.get("retry"))
                        )
                    )
                if self.path == "/api/queue":
                    return self.json(store.control(data["action"]))
                if self.path == "/api/browser":
                    return self.json(
                        store.previews.request(
                            data["clip"], data["profile"], bool(data.get("retry"))
                        )
                    )
                return self.json({"error": "Not found"}, 404)
            except (ValueError, KeyError, OSError) as e:
                self.json({"error": str(e)}, 400)

        def log_message(self, *_):
            pass

    return Handler


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--catalogue", required=True, type=Path)
    parser.add_argument("--config", required=True, type=Path)
    parser.add_argument("--work", type=Path, default=ROOT / "work")
    parser.add_argument("--port", type=int, default=8820)
    parser.add_argument("--bulk", action="store_true")
    args = parser.parse_args()
    config = json.loads(args.config.read_text(encoding="utf8"))
    lease = queue_lease(args.work)
    # Bind first: a second service instance must never reset running jobs or own another GPU queue.
    server = ThreadingHTTPServer(("127.0.0.1", args.port), BaseHTTPRequestHandler)
    store = Store(args.catalogue, config, args.work)
    if args.bulk:
        store.bulk()
    server.RequestHandlerClass = handler(store)
    store.discovery = ArchiveDiscovery(store)
    store.previews = BrowserPreviews(store)
    worker = Worker(store)
    store.worker = worker
    store.control("resume")
    print(f"Workspots: http://127.0.0.1:{args.port}", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        worker.closed.set()
        server.server_close()
        store.thread.join(timeout=300)
        store.previews.closed.set()
        store.previews.wake.set()
        store.previews.thread.join(timeout=180)
        lease.close()


if __name__ == "__main__":
    main()
