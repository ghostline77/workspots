"""Queue identity, recovery, and local HTTP boundary regression tests."""

from contextlib import closing
import json
from pathlib import Path
import sqlite3
import tempfile
import threading
import unittest
import urllib.error
import urllib.request
from unittest.mock import patch
from browser_preview import BrowserPreviews
from archive_discovery import ArchiveDiscovery
from io import BytesIO

from server import Store, handler, ThreadingHTTPServer, archive_preparation, queue_lease


class QueueTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        cat = self.root / "catalogue"
        (cat / "full-batch").mkdir(parents=True)
        (cat / "caption-results").mkdir()
        jobs = [
            {
                "id": "a",
                "animation_name": "phone",
                "rig": "man_base.rig",
                "resolved_animset": "phone.anims",
                "uses": [
                    {
                        "workspot": "phone.workspot",
                        "role": "clip",
                        "entry_id": 1,
                        "field": "animName",
                    }
                ],
            },
            {
                "id": "b",
                "animation_name": "sit",
                "rig": "woman_base.rig",
                "resolved_animset": "sit.anims",
                "uses": [],
            },
        ]
        (cat / "full-batch/plan.json").write_text(
            json.dumps({"jobs": jobs, "unresolved": [{}], "skipped": [{}]})
        )
        with closing(sqlite3.connect(cat / "workspots.sqlite3")) as db:
            db.execute(
                "CREATE TABLE workspots(depot_path TEXT,label TEXT,animation_count INTEGER,review_status TEXT)"
            )
            db.execute(
                "INSERT INTO workspots VALUES('phone.workspot','Phone',1,'unreviewed')"
            )
            db.commit()
        with closing(
            sqlite3.connect(cat / "caption-results/caption-search.sqlite3")
        ) as db:
            db.executescript("""CREATE TABLE captions(id TEXT,caption_json TEXT,review_status TEXT);
                CREATE VIRTUAL TABLE caption_search USING fts5(clip_id UNINDEXED,animation,workspots,description);""")
            db.execute("INSERT INTO captions VALUES('a','{}','unreviewed')")
            db.execute(
                "INSERT INTO caption_search VALUES('a','phone','phone.workspot','Talking on a phone')"
            )
            db.commit()
        exe = self.root / "worker.exe"
        exe.write_bytes(b"fixture")
        self.config = {
            "worker": str(exe),
            "characters": [
                {
                    "name": "Male",
                    "rig": "male",
                    "entity": "a.ent",
                    "appearance": "default",
                    "overlays": [],
                },
                {
                    "name": "Female",
                    "rig": "female",
                    "entity": "b.ent",
                    "appearance": "default",
                    "overlays": [],
                },
            ],
        }
        self.cat = cat
        self.work = self.root / "work"
        self.store = Store(cat, self.config, self.work)

    def tearDown(self):
        self.temp.cleanup()

    def test_preparation_restart_preserves_media_and_old_assets(self):
        output = self.work / "renders" / "character"
        prepared = output / "prepared"
        prepared.mkdir(parents=True)
        (prepared / "mesh.bin").write_bytes(b"mesh")
        clip = output / "clip"
        clip.mkdir()
        (clip / "preview.mp4").write_bytes(b"video")
        archive_preparation(output, self.work)
        self.assertFalse(prepared.exists())
        self.assertEqual(
            next((output / "preparation-history").glob("*/mesh.bin")).read_bytes(),
            b"mesh",
        )
        self.assertEqual((clip / "preview.mp4").read_bytes(), b"video")
        with self.assertRaisesRegex(ValueError, "escaped"):
            self._outside_preparation()

    def _outside_preparation(self):
        outside = self.root / "outside"
        (outside / "prepared").mkdir(parents=True)
        archive_preparation(outside, self.work)

    def test_queue_owner_is_exclusive(self):
        lease = queue_lease(self.work)
        try:
            with self.assertRaisesRegex(RuntimeError, "already owns"):
                queue_lease(self.work)
        finally:
            lease.close()
        queue_lease(self.work).close()

    def test_archive_upload_is_hashed_deduplicated_and_cleans_interrupted_upload(self):
        inspector = ArchiveDiscovery(self.store)
        data = b"RDAR" + bytes(80)
        first = inspector.upload(BytesIO(data), len(data), "character.archive")
        second = inspector.upload(BytesIO(data), len(data), "another.archive")
        self.assertEqual(first["archive"], second["archive"])
        self.assertEqual(Path(first["archive"]).read_bytes(), data)
        with self.assertRaisesRegex(ValueError, "interrupted"):
            inspector.upload(BytesIO(data), 200, "bad.archive")
        with self.assertRaisesRegex(ValueError, "not a Cyberpunk"):
            inspector.upload(BytesIO(b"nope"), 4, "bad.archive")
        with self.assertRaises(ValueError):
            inspector.upload(BytesIO(data), len(data), "../bad.archive")
        self.assertFalse(list((self.work / "uploaded-archives").glob("*.part")))

    def test_archive_discovery_cache_and_exact_appearance_selection(self):
        archive = self.root / "character.archive"
        archive.write_bytes(b"RDAR" + bytes(80))
        (self.root / "workspot_archive.exe").write_bytes(b"inspector fixture")
        schema = self.root / "schema.json"
        schema.write_text("{}")
        self.config.update(game=str(self.root), schema=str(schema))
        inspector = ArchiveDiscovery(self.store)
        catalog = {
            "entities": [{"resource": "a.ent", "appearances": [{"name": "default"}]}],
            "warnings": [],
        }
        with patch.object(inspector, "run", return_value=catalog) as run:
            a = inspector.inspect(archive)
            b = inspector.inspect(archive)
            self.assertEqual(a["archive_sha256"], b["archive_sha256"])
            self.assertEqual(run.call_count, 1)
        with self.assertRaisesRegex(ValueError, "Choose an entity"):
            inspector.resolve(archive, a["archive_sha256"], "a.ent", "wrong")
        with self.assertRaisesRegex(ValueError, "Archive changed"):
            inspector.resolve(archive, "wrong", "a.ent", "default")
        with patch.object(inspector, "run", return_value={"rig": "male"}):
            self.assertEqual(
                inspector.resolve(archive, a["archive_sha256"], "a.ent", "default")[
                    "rig"
                ],
                "male",
            )

    def test_archive_registration_rejects_stale_hash_and_accepts_native_entity_hash(
        self,
    ):
        archive = self.root / "character.archive"
        archive.write_bytes(b"RDAR")
        profile = dict(
            self.config["characters"][0],
            overlays=[str(archive)],
            archive_sha256="stale",
        )
        with self.assertRaisesRegex(ValueError, "Archive changed"):
            self.store.register(profile)
        profile.pop("archive_sha256")
        profile["entity"] = "4946349661399716641"
        self.assertTrue(self.store.register(profile))

    def test_browser_previews_deduplicate_validate_rigs_and_repair_changed_cache(self):
        import hashlib

        (self.root / "workspot_browser.exe").write_bytes(b"export fixture")
        with patch.object(BrowserPreviews, "run", lambda p: p.closed.wait()):
            previews = BrowserPreviews(self.store)
            try:
                a = previews.request("a", self.store.defaults[0])
                self.assertEqual(
                    previews.request("a", self.store.defaults[0])["key"], a["key"]
                )
                with self.assertRaisesRegex(ValueError, "rigs must match"):
                    previews.request("a", self.store.defaults[1])
                folder = self.work / "browser" / a["key"]
                folder.mkdir(parents=True)
                (folder / "scene.glb").write_bytes(b"fixture")
                with self.store.db() as db:
                    db.execute(
                        "UPDATE browser_previews SET state='ready',receipt=? WHERE key=?",
                        (
                            json.dumps(
                                {"glb_sha256": hashlib.sha256(b"fixture").hexdigest()}
                            ),
                            a["key"],
                        ),
                    )
                self.assertEqual(
                    previews.request("a", self.store.defaults[0])["state"], "ready"
                )
                (folder / "scene.glb").write_bytes(b"changed")
                self.assertEqual(
                    previews.request("a", self.store.defaults[0])["state"], "queued"
                )
                with self.store.db() as db:
                    self.assertEqual(
                        db.execute("SELECT count(*) FROM jobs").fetchone()[0], 0
                    )
            finally:
                previews.closed.set()
                previews.thread.join()

    def test_bulk_and_priority_are_idempotent(self):
        s = self.store
        s.bulk()
        s.bulk()
        s.enqueue("a", s.defaults[0])
        s.enqueue("a", s.defaults[0])
        with s.db() as db:
            self.assertEqual(db.execute("SELECT count(*) FROM jobs").fetchone()[0], 2)
            self.assertEqual(
                db.execute("SELECT priority FROM jobs WHERE clip='a'").fetchone()[0],
                100,
            )
        with self.assertRaisesRegex(ValueError, "rigs must match"):
            s.enqueue("a", s.defaults[1])

    def test_recovery_requeues_interrupted_and_missing_outputs(self):
        s = self.store
        s.bulk()
        with s.db() as db:
            db.execute("UPDATE jobs SET state='running' WHERE clip='a'")
            db.execute("UPDATE jobs SET state='complete',receipt='{}' WHERE clip='b'")
        recovered = Store(self.cat, self.config, self.work)
        self.assertEqual(recovered.status()["counts"], {"queued": 2})

    def test_changed_plan_cannot_reuse_results(self):
        p = self.cat / "full-batch/plan.json"
        p.write_text(p.read_text() + " ")
        with self.assertRaisesRegex(ValueError, "Canonical plan changed"):
            Store(self.cat, self.config, self.work)

    def test_refitted_manifest_and_invalid_depot_rejected(self):
        profile = dict(self.config["characters"][0])
        project = self.root / "character.json"
        project.write_text('{"refit_clothing":true}')
        profile["project"] = str(project)
        with self.assertRaisesRegex(ValueError, "Refitted"):
            self.store.register(profile)
        profile.pop("project")
        profile["entity"] = "../../a.ent"
        with self.assertRaisesRegex(ValueError, "depot path"):
            self.store.register(profile)

    def test_search_and_source_workspots_include_resolved_plan(self):
        self.assertEqual(self.store.search("talking", "", "", 0)["total"], 1)
        self.assertEqual(self.store.search("phone.workspot", "", "", 0)["total"], 1)
        row = self.store.search("", "", "", 0, "workspots")["items"][0]
        self.assertEqual(row["resolved_clips"], 1)
        self.assertEqual(
            self.store.detail("a")["plan"]["uses"][0]["workspot"], "phone.workspot"
        )

    def test_http_ranges_and_cross_origin_rejection(self):
        directory = self.work / "renders" / "p" / "a"
        directory.mkdir(parents=True)
        (directory / "preview.mp4").write_bytes(b"0123456789")
        http = ThreadingHTTPServer(("127.0.0.1", 0), handler(self.store))
        thread = threading.Thread(target=http.serve_forever, daemon=True)
        thread.start()
        url = f"http://127.0.0.1:{http.server_port}"
        try:
            request = urllib.request.Request(
                url + "/media/renders/p/a/preview.mp4", headers={"Range": "bytes=2-5"}
            )
            with urllib.request.urlopen(request) as response:
                self.assertEqual(response.status, 206)
                self.assertEqual(response.read(), b"2345")
            request = urllib.request.Request(
                url + "/api/render",
                data=b"{}",
                headers={"Origin": "https://example.com"},
            )
            with self.assertRaises(urllib.error.HTTPError) as error:
                urllib.request.urlopen(request)
            self.assertEqual(error.exception.code, 403)
            with self.assertRaises(urllib.error.HTTPError) as error:
                urllib.request.urlopen(url + "/media/renders/../../config.json")
            self.assertEqual(error.exception.code, 404)
        finally:
            http.shutdown()
            http.server_close()
            thread.join()


if __name__ == "__main__":
    unittest.main()
