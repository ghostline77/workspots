# Ghostline Workspots

Search native animation and workspot records, inspect source mappings, and preview
animations on registered characters. The local browser supports interactive 3D
playback and a resumable queue of rendered videos.

## Start

Use Rust 1.92+, Node 20+, FFmpeg and FFprobe. Keep the sibling
`ghostline-red`, `renderer` and `ui-core` repositories available.
The service needs an existing workspot catalogue containing its plan, databases
and source previews. See [setup and commands](docs/usage.md#run).

1. Build the renderer workers, service and frontend.
2. Copy [config.example.json](config.example.json) to `work/config.json`.
3. Set the game path and character archive/entity/appearance paths in that copy.
4. Start from this repository:

```powershell
./target/release/ghostline-workspots.exe --catalogue path/to/catalogue --config work/config.json --bulk
```

Open <http://127.0.0.1:8820/>. Omit `--bulk` for on-demand rendering.
Character archives are read as overlays; the service does not install them.

## Documentation

- [Build, configuration and queue startup](docs/usage.md#run)
- [Search, character registration and playback](docs/usage.md#browse-and-render)
- [Generated files, resuming and preview limitations](docs/usage.md#outputs-and-provenance)
- [Generated motion imports and retargeting](docs/usage.md#generated-motion-previews)
- [Development checks](docs/usage.md#check)

Run one service per work directory. Stop it before changing native worker binaries
or catalogue inputs. Completed clips retain source and renderer receipts; failed
clips remain failed until retried. Previewing an animation does not validate its
complete workspot tree, props, partner actors or runtime game behaviour.
