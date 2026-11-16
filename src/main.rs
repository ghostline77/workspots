//! Rust HTTP backend and durable single-GPU queue for the motion library.
mod discovery;
mod generated;
mod native;
mod retarget;
mod store;
#[cfg(test)]
mod tests;
mod web;

use anyhow::{Context, Result};
use clap::Parser;
use std::{fs, net::SocketAddr, path::PathBuf, sync::Arc};

#[derive(Parser)]
struct Arguments {
    #[arg(long)]
    catalogue: PathBuf,
    #[arg(long)]
    config: PathBuf,
    #[arg(long, default_value = "work")]
    work: PathBuf,
    #[arg(long, default_value_t = 8820)]
    port: u16,
    #[arg(long)]
    bulk: bool,
    #[arg(long)]
    paused: bool,
    /// Serve the API without worker supervisors, for migration/API validation.
    #[arg(long)]
    no_workers: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Arguments::parse();
    fs::create_dir_all(&args.work)?;
    let lease = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(args.work.join("queue.lock"))?;
    lease.try_lock().context(
        "Another service owns this work directory; stop it before starting another backend",
    )?;
    // Acquire ownership and the port before recovering any interrupted jobs.
    let address = SocketAddr::from(([127, 0, 0, 1], args.port));
    let listener = tokio::net::TcpListener::bind(address).await?;
    let app = Arc::new(store::Store::open(
        &args.catalogue,
        &args.config,
        &args.work,
        args.bulk,
        args.port,
    )?);
    app.paused
        .store(args.paused, std::sync::atomic::Ordering::Release);
    let render = (!args.no_workers).then(|| native::spawn(app.clone(), false));
    let browser = (!args.no_workers).then(|| native::spawn(app.clone(), true));
    println!("Ghostline Motion library (Rust): http://{address}");
    let shutdown = app.clone();
    axum::serve(listener, web::router(app.clone()))
        .with_graceful_shutdown(async move {
            tokio::select! {_=tokio::signal::ctrl_c()=>{},_=shutdown.shutdown.notified()=>{}}
            shutdown
                .closed
                .store(true, std::sync::atomic::Ordering::Release);
        })
        .await?;
    app.closed.store(true, std::sync::atomic::Ordering::Release);
    if let Some(render) = render {
        render
            .join()
            .map_err(|_| anyhow::anyhow!("Render supervisor panicked"))?;
    }
    if let Some(browser) = browser {
        browser
            .join()
            .map_err(|_| anyhow::anyhow!("Browser supervisor panicked"))?;
    }
    drop(lease);
    Ok(())
}
