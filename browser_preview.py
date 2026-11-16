"""Bounded CPU preparation of cached animated GLBs for the browser viewport."""

import hashlib
import json
import os
from pathlib import Path
import queue
import subprocess
import threading
import time


class BrowserPreviews:
    def __init__(self, store):
        self.store = store
        self.exe = Path(
            store.config.get(
                "browser_worker",
                Path(store.config["worker"]).with_name("workspot_browser.exe"),
            )
        ).resolve()
        self.identity = self.hash_file(self.exe) if self.exe.is_file() else None
        self.process = None
        self.profile = None
        self.handled = 0
        self.closed = threading.Event()
        self.wake = threading.Event()
        self.lock = threading.Lock()
        with store.db() as db:
            db.execute("""CREATE TABLE IF NOT EXISTS browser_previews(
                key TEXT PRIMARY KEY,clip TEXT,profile TEXT,state TEXT,error TEXT,receipt TEXT,created REAL)""")
            db.execute(
                "UPDATE browser_previews SET state='queued' WHERE state='preparing'"
            )
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    @staticmethod
    def hash_file(path):
        h = hashlib.sha256()
        with open(path, "rb") as f:
            for block in iter(lambda: f.read(1024 * 1024), b""):
                h.update(block)
        return h.hexdigest()

    def request(self, clip, profile, retry=False):
        if not self.identity:
            raise ValueError(
                "Build the workspot_browser Rust example to enable interactive previews"
            )
        with self.lock, self.store.db() as db:
            a = db.execute("SELECT rig,plan FROM clips WHERE id=?", (clip,)).fetchone()
            b = db.execute("SELECT rig FROM profiles WHERE id=?", (profile,)).fetchone()
            if not a or not b or a["rig"] != b["rig"]:
                raise ValueError("Character and animation rigs must match")
            key = hashlib.sha256(
                json.dumps(
                    [clip, profile, self.identity, a["plan"]], sort_keys=True
                ).encode()
            ).hexdigest()[:32]
            row = db.execute(
                "SELECT * FROM browser_previews WHERE key=?", (key,)
            ).fetchone()
            if not row:
                if (
                    db.execute(
                        "SELECT count(*) FROM browser_previews WHERE state IN ('queued','preparing')"
                    ).fetchone()[0]
                    >= 4
                ):
                    raise ValueError(
                        "Four previews are already pending; wait for a preparation to finish"
                    )
                db.execute(
                    "INSERT INTO browser_previews VALUES(?,?,?,'queued',NULL,NULL,?)",
                    (key, clip, profile, time.time()),
                )
            elif retry and row["state"] == "failed":
                db.execute(
                    "UPDATE browser_previews SET state='queued',error=NULL WHERE key=?",
                    (key,),
                )
            elif row["state"] == "ready":
                receipt = json.loads(row["receipt"])
                path = self.store.work / "browser" / key / "scene.glb"
                if not path.is_file() or self.hash_file(path) != receipt["glb_sha256"]:
                    db.execute(
                        "UPDATE browser_previews SET state='queued',receipt=NULL WHERE key=?",
                        (key,),
                    )
        self.wake.set()
        return self.get(key)

    def get(self, key):
        with self.store.db() as db:
            row = db.execute(
                "SELECT * FROM browser_previews WHERE key=?", (key,)
            ).fetchone()
        if not row:
            raise ValueError("Unknown browser preview")
        result = dict(row)
        result["receipt"] = json.loads(result["receipt"]) if result["receipt"] else None
        result["scene"] = (
            f"/media/browser/{key}/scene.glb" if result["state"] == "ready" else None
        )
        return result

    def stop(self):
        if self.process:
            self.process.stdin.close()
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
        self.process = None
        self.profile = None
        self.handled = 0

    def message(self):
        try:
            line = self.messages.get(timeout=180)
        except queue.Empty:
            self.process.kill()
            self.process.wait()
            raise RuntimeError("Browser scene preparation timed out")
        if not line:
            code = self.process.wait(timeout=5)
            raise RuntimeError(
                f"Browser scene preparer exited ({code}); see browser-worker.log"
            )
        return json.loads(line)

    def start(self, profile_id):
        self.stop()
        s = self.store
        with s.db() as db:
            p = json.loads(
                db.execute(
                    "SELECT config FROM profiles WHERE id=?", (profile_id,)
                ).fetchone()[0]
            )
        for path, expected in zip(p["overlays"], p["overlay_hashes"]):
            if self.hash_file(path) != expected:
                raise ValueError(
                    "Character archive changed; register a new character version"
                )
        self.output = s.work / "browser-workers" / profile_id / str(time.time_ns())
        args = [
            str(self.exe),
            "--game",
            s.config["game"],
            "--schema",
            s.config["schema"],
            "--resource",
            p["entity"],
            "--appearance",
            p["appearance"],
            "--out",
            str(self.output),
            "--fps",
            "30",
            "--seconds",
            "8",
        ]
        for archive in p["overlays"]:
            args += ["--overlay", archive]
        for anchor in p.get("origin_anchors", []):
            args += ["--origin-anchor", anchor]
        if p.get("rigid_hair"):
            args += ["--rigid-hair", p["rigid_hair"]]
        with open(s.work / "browser-worker.log", "a", encoding="utf8") as log:
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

        def read(process, messages):
            for line in process.stdout:
                if line.lstrip().startswith("{"):
                    messages.put(line)
            messages.put("")

        threading.Thread(
            target=read, args=(self.process, self.messages), daemon=True
        ).start()
        if not self.message().get("ready"):
            raise RuntimeError("Browser scene preparer did not become ready")
        self.profile = profile_id

    def prepare(self, job):
        if (
            self.profile != job["profile"]
            or not self.process
            or self.process.poll() is not None
            or self.handled >= 4
        ):
            self.start(job["profile"])
        self.handled += 1
        with self.store.db() as db:
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
        result = self.message()
        if not result.get("ok"):
            raise RuntimeError(result.get("error", "Browser export failed"))
        receipt = result["result"]
        file = self.output / job["clip"] / "scene.glb"
        if self.hash_file(file) != receipt["glb_sha256"]:
            raise RuntimeError("Browser GLB hash validation failed")
        with open(file, "rb") as f:
            header = f.read(12)
        if (
            header[:4] != b"glTF"
            or int.from_bytes(header[8:12], "little") != file.stat().st_size
        ):
            raise RuntimeError("Invalid browser GLB container")
        destination = self.store.work / "browser" / job["key"]
        destination.mkdir(parents=True, exist_ok=True)
        os.replace(file, destination / "scene.glb")
        receipt.update(
            {
                "clip": job["clip"],
                "profile": job["profile"],
                "exporter_sha256": self.identity,
                "finished": time.time(),
            }
        )
        (destination / "receipt.json").write_text(
            json.dumps(receipt, indent=2), encoding="utf8"
        )
        return receipt

    def run(self):
        try:
            while not self.closed.is_set():
                with self.store.db() as db:
                    row = db.execute(
                        "SELECT * FROM browser_previews WHERE state='queued' ORDER BY created LIMIT 1"
                    ).fetchone()
                    if row:
                        db.execute(
                            "UPDATE browser_previews SET state='preparing' WHERE key=?",
                            (row["key"],),
                        )
                if not row:
                    self.wake.clear()
                    if not self.wake.wait(10):
                        self.stop()
                    continue
                try:
                    receipt = self.prepare(dict(row))
                    with self.store.db() as db:
                        db.execute(
                            "UPDATE browser_previews SET state='ready',receipt=?,error=NULL WHERE key=?",
                            (json.dumps(receipt), row["key"]),
                        )
                except Exception as error:
                    self.stop()
                    with self.store.db() as db:
                        db.execute(
                            "UPDATE browser_previews SET state='failed',error=? WHERE key=?",
                            (str(error), row["key"]),
                        )
        finally:
            self.stop()
