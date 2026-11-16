//! Build the frontend from the dependencies locked inside this repository.
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    time::SystemTime,
};

fn authored(path: &Path, files: &mut Vec<PathBuf>) {
    if path.is_file() {
        files.push(path.to_owned());
    } else if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .file_name()
                .is_some_and(|name| name == "node_modules" || name == "dist")
            {
                continue;
            }
            authored(&path, files);
        }
    }
}

fn modified(path: &Path) -> SystemTime {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

fn npm(root: &Path, arguments: &[&str]) {
    let mut command = if cfg!(windows) {
        let mut command = Command::new("cmd");
        command.args(["/C", "npm"]);
        command
    } else {
        Command::new("npm")
    };
    let status = command
        .arg("--workspaces=false")
        .args(arguments)
        .current_dir(root)
        .status()
        .expect("could not start npm; install Node.js to build the frontend");
    assert!(
        status.success(),
        "frontend build failed; run npm ci and npm run build in ui"
    );
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let ui = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest directory"))
        .join("ui");
    let mut files = Vec::new();
    authored(&ui, &mut files);
    for path in &files {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    let lock = ui.join("package-lock.json");
    if !ui.join("node_modules/.package-lock.json").is_file()
        || modified(&lock) > modified(&ui.join("node_modules/.package-lock.json"))
    {
        npm(&ui, &["ci"]);
    }
    let newest = files
        .iter()
        .map(|p| modified(p))
        .max()
        .unwrap_or(SystemTime::UNIX_EPOCH);
    if modified(&ui.join("dist/index.html")) < newest {
        npm(&ui, &["run", "build"]);
    }
}
