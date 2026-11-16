"""Read-only native archive discovery and content-addressed local archive uploads."""

import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading


class ArchiveDiscovery:
    def __init__(self, store):
        self.store = store
        self.exe = Path(
            store.config.get(
                "archive_inspector",
                Path(store.config["worker"]).with_name("workspot_archive.exe"),
            )
        ).resolve()
        self.lock = threading.Lock()

    @staticmethod
    def digest(path):
        h = hashlib.sha256()
        with open(path, "rb") as f:
            for block in iter(lambda: f.read(1024 * 1024), b""):
                h.update(block)
        return h.hexdigest()

    def path(self, path):
        path = Path(path).resolve()
        if path.suffix.lower() != ".archive" or not path.is_file():
            raise ValueError("Choose an existing .archive file")
        if not self.exe.is_file():
            raise ValueError("Build the workspot_archive Rust example first")
        return path

    def run(self, path, *extra):
        try:
            result = subprocess.run(
                [
                    str(self.exe),
                    "--game",
                    self.store.config["game"],
                    "--schema",
                    self.store.config["schema"],
                    "--archive",
                    str(path),
                    *extra,
                ],
                capture_output=True,
                text=True,
                encoding="utf8",
                timeout=120,
            )
        except subprocess.TimeoutExpired:
            raise ValueError(
                "Archive inspection exceeded two minutes; narrow this to a character archive"
            ) from None
        if result.returncode:
            raise ValueError(
                result.stderr.strip()[-2500:] or "Native archive inspection failed"
            )
        return json.loads(result.stdout)

    def inspect(self, path):
        path = self.path(path)
        with self.lock:
            identity = self.digest(path)
            cache_key = hashlib.sha256(
                json.dumps(
                    [
                        identity,
                        self.digest(self.exe),
                        self.digest(self.store.config["schema"]),
                    ]
                ).encode()
            ).hexdigest()
            cache = self.store.work / "character-discovery" / f"{cache_key}.json"
            if cache.is_file():
                result = json.loads(cache.read_text(encoding="utf8"))
            else:
                result = self.run(path)
                if self.digest(path) != identity:
                    raise ValueError(
                        "Archive changed during inspection; inspect it again"
                    )
                cache.parent.mkdir(parents=True, exist_ok=True)
                cache.write_text(json.dumps(result, indent=2), encoding="utf8")
            result.update({"archive": str(path), "archive_sha256": identity})
            return result

    def resolve(self, path, identity, entity, appearance):
        catalog = self.inspect(path)
        if catalog["archive_sha256"] != identity:
            raise ValueError(
                "Archive changed; inspect it again before choosing a character"
            )
        selected = next(
            (e for e in catalog["entities"] if e["resource"] == entity), None
        )
        if not selected or not any(
            a["name"] == appearance for a in selected["appearances"]
        ):
            raise ValueError(
                "Choose an entity and appearance discovered in this archive"
            )
        with self.lock:
            result = self.run(
                self.path(path), "--entity", entity, "--appearance", appearance
            )
        if self.digest(path) != identity:
            raise ValueError("Archive changed during entity resolution")
        with self.store.db() as db:
            for row in db.execute("SELECT id,name,config FROM profiles"):
                p = json.loads(row["config"])
                if (
                    p["entity"].replace("\\", "/") == entity.replace("\\", "/")
                    and p["appearance"] == appearance
                    and p.get("overlay_hashes") == [identity]
                ):
                    usable = all(
                        Path(file).is_file() and self.digest(file) == expected
                        for file, expected in zip(p["overlays"], p["overlay_hashes"])
                    )
                    result["existing_recipe"] = {
                        "id": row["id"] if usable else "",
                        "name": row["name"],
                        "origin_anchors": p.get("origin_anchors", []),
                        "rigid_hair": p.get("rigid_hair"),
                    }
                    break
        return result

    def upload(self, stream, length, name):
        if length < 1 or length > 2 * 1024**3:
            raise ValueError(
                "File uploads must be under 2 GiB; use a local archive path for larger files"
            )
        if (
            not isinstance(name, str)
            or not name.lower().endswith(".archive")
            or "/" in name
            or "\\" in name
        ):
            raise ValueError("Expected an .archive filename")
        directory = self.store.work / "uploaded-archives"
        directory.mkdir(parents=True, exist_ok=True)
        handle = tempfile.NamedTemporaryFile(
            dir=directory, suffix=".part", delete=False
        )
        temporary = Path(handle.name)
        try:
            h = hashlib.sha256()
            remaining = length
            with handle:
                while remaining:
                    block = stream.read(min(1024 * 1024, remaining))
                    if not block:
                        raise ValueError("Archive upload was interrupted")
                    handle.write(block)
                    h.update(block)
                    remaining -= len(block)
            identity = h.hexdigest()
            destination = directory / f"{identity}.archive"
            with open(temporary, "rb") as f:
                if f.read(4) != b"RDAR":
                    raise ValueError("The selected file is not a Cyberpunk .archive")
            os.replace(temporary, destination)
            return {
                "archive": str(destination),
                "archive_sha256": identity,
                "filename": name,
                "bytes": length,
            }
        finally:
            if temporary.exists():
                temporary.unlink()
