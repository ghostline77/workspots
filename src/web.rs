use crate::store::{Store, string};
use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, Request, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    sync::{Arc, atomic::Ordering},
};
use tower_http::services::ServeDir;

type App = Arc<Store>;
pub struct ApiError(anyhow::Error);
impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self(e)
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":format!("{:#}",self.0)})),
        )
            .into_response()
    }
}
type Reply = std::result::Result<Json<Value>, ApiError>;
async fn run(app: App, action: impl FnOnce(&Store) -> Result<Value> + Send + 'static) -> Reply {
    let result = tokio::task::spawn_blocking(move || action(&app))
        .await
        .map_err(|e| ApiError(anyhow::anyhow!(e)))??;
    Ok(Json(result))
}

async fn local_request(State(app): State<App>, request: Request<Body>, next: Next) -> Response {
    if request.method() != axum::http::Method::GET && request.method() != axum::http::Method::HEAD {
        let host = request
            .headers()
            .get(header::HOST)
            .and_then(|s| s.to_str().ok())
            .unwrap_or_default();
        let local =
            host == format!("127.0.0.1:{}", app.port) || host == format!("localhost:{}", app.port);
        let origin = request
            .headers()
            .get(header::ORIGIN)
            .and_then(|s| s.to_str().ok());
        let valid_origin = origin.is_none_or(|o| o == format!("http://{host}"));
        if !local || !valid_origin {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"error":"Cross-origin editor request rejected"})),
            )
                .into_response();
        }
    }
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        "x-ghostline-backend",
        axum::http::HeaderValue::from_static("rust"),
    );
    response
}
async fn status(State(app): State<App>) -> Reply {
    run(app, Store::status).await
}
#[derive(Deserialize, Default)]
struct Search {
    #[serde(default)]
    q: String,
    #[serde(default)]
    rig: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    page: i64,
    #[serde(default)]
    kind: String,
}
async fn search(State(app): State<App>, Query(q): Query<Search>) -> Reply {
    run(app, move |s| {
        s.search(&q.q, &q.rig, &q.state, q.page, &q.kind)
    })
    .await
}
async fn detail(State(app): State<App>, Path(id): Path<String>) -> Reply {
    run(app, move |s| s.detail(&id)).await
}
async fn register(State(app): State<App>, Json(value): Json<Value>) -> Reply {
    run(app, move |s| Ok(json!({"id":s.register(value)?}))).await
}
async fn render(State(app): State<App>, Json(value): Json<Value>) -> Reply {
    run(app, move |s| {
        s.enqueue(
            string(&value, "clip")?,
            string(&value, "profile")?,
            value["retry"] == true,
        )
    })
    .await
}
async fn queue(State(app): State<App>, Json(value): Json<Value>) -> Reply {
    run(app, move |s| s.control(string(&value, "action")?)).await
}
async fn browser(State(app): State<App>, Json(value): Json<Value>) -> Reply {
    run(app, move |s| {
        s.browser_request(
            string(&value, "clip")?,
            string(&value, "profile")?,
            value["retry"] == true,
        )
    })
    .await
}
async fn browser_get(State(app): State<App>, Path(key): Path<String>) -> Reply {
    run(app, move |s| s.browser_get(&key)).await
}
async fn discover(State(app): State<App>, Json(value): Json<Value>) -> Reply {
    run(app, move |s| s.inspect_archive(string(&value, "archive")?)).await
}
async fn resolve(State(app): State<App>, Json(value): Json<Value>) -> Reply {
    run(app, move |s| s.resolve_archive(&value)).await
}
async fn shutdown(State(app): State<App>, Json(_value): Json<Value>) -> Reply {
    app.closed.store(true, Ordering::Release);
    app.shutdown.notify_one();
    Ok(Json(json!({"stopping":true})))
}
async fn generated_list(State(app): State<App>) -> Reply {
    run(app, Store::generated_list).await
}
async fn generated_retarget(State(app): State<App>, Json(value): Json<Value>) -> Reply {
    run(app, move |s| {
        s.generated_retarget(
            string(&value, "motion")?,
            string(&value, "character")?,
            value["hand_pose"].as_str().unwrap_or("generated"),
        )
    })
    .await
}
async fn generated_detail(State(app): State<App>, Path(id): Path<String>) -> Reply {
    run(app, move |s| s.generated_detail(&id)).await
}
async fn generated_import(State(app): State<App>, Json(value): Json<Value>) -> Reply {
    run(app, move |s| {
        s.import_generated(std::path::Path::new(string(&value, "path")?))
    })
    .await
}
async fn generated_characters(State(app): State<App>) -> Reply {
    run(app, Store::generated_characters).await
}
async fn generated_character(State(app): State<App>, Json(value): Json<Value>) -> Reply {
    run(app, move |s| {
        s.generated_character(string(&value, "profile")?)
    })
    .await
}
async fn generated_character_get(State(app): State<App>, Path(key): Path<String>) -> Reply {
    run(app, move |s| s.generated_character_get(&key)).await
}
#[derive(Deserialize)]
struct Upload {
    filename: String,
}
async fn upload(
    State(app): State<App>,
    Query(query): Query<Upload>,
    headers: HeaderMap,
    body: Body,
) -> Reply {
    async fn receive(app: App, name: String, headers: HeaderMap, body: Body) -> Result<Value> {
        let length: u64 = headers
            .get(header::CONTENT_LENGTH)
            .context("Content-Length required")?
            .to_str()?
            .parse()?;
        ensure!(
            length > 0 && length <= 2 * 1024 * 1024 * 1024,
            "Uploads must be under 2 GiB; use a local archive path for larger files"
        );
        ensure!(
            name.to_ascii_lowercase().ends_with(".archive")
                && !name.contains('/')
                && !name.contains('\\'),
            "Expected an .archive filename"
        );
        let directory = app.work.join("uploaded-archives");
        fs::create_dir_all(&directory)?;
        let mut file = tempfile::NamedTempFile::new_in(&directory)?;
        let mut sum = Sha256::new();
        let mut written = 0u64;
        let mut stream = body.into_data_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            written = written
                .checked_add(chunk.len() as u64)
                .context("Upload length overflow")?;
            ensure!(written <= length, "Upload exceeds declared length");
            file.write_all(&chunk)?;
            sum.update(&chunk);
        }
        ensure!(written == length, "Upload was interrupted");
        file.flush()?;
        file.seek(SeekFrom::Start(0))?;
        let mut magic = [0; 4];
        file.read_exact(&mut magic)?;
        ensure!(&magic == b"RDAR", "Not a Cyberpunk .archive");
        let identity = format!("{:x}", sum.finalize());
        let destination = directory.join(format!("{identity}.archive"));
        file.persist(&destination).map_err(|e| e.error)?;
        Ok(
            json!({"archive":destination.to_string_lossy(),"archive_sha256":identity,"filename":name,"bytes":written}),
        )
    }
    Ok(Json(receive(app, query.filename, headers, body).await?))
}

pub fn router(app: App) -> Router {
    Router::new()
        .route("/api/status", get(status))
        .route("/api/generated", get(generated_list))
        .route("/api/generated/import", post(generated_import))
        .route("/api/generated/retarget", post(generated_retarget))
        .route("/api/generated/characters", get(generated_characters))
        .route("/api/generated/character", post(generated_character))
        .route(
            "/api/generated/character/{key}",
            get(generated_character_get),
        )
        .route("/api/generated/{id}", get(generated_detail))
        .route("/api/clips", get(search))
        .route("/api/clips/{id}", get(detail))
        .route("/api/characters", post(register))
        .route("/api/render", post(render))
        .route("/api/queue", post(queue))
        .route("/api/browser", post(browser))
        .route("/api/browser/{key}", get(browser_get))
        .route("/api/archive-discover", post(discover))
        .route("/api/archive-resolve", post(resolve))
        .route("/api/archive-upload", post(upload))
        .route("/api/shutdown", post(shutdown))
        .nest_service("/media/renders", ServeDir::new(app.work.join("renders")))
        .nest_service("/media/browser", ServeDir::new(app.work.join("browser")))
        .nest_service(
            "/media/generated",
            ServeDir::new(app.work.join("generated")),
        )
        .nest_service(
            "/media/original",
            ServeDir::new(app.catalogue.join("previews")),
        )
        .route(
            "/",
            get(|| async { axum::response::Html(include_str!("../ui/dist/index.html")) }),
        )
        .route(
            "/app.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript")],
                    include_str!("../ui/dist/app.js"),
                )
            }),
        )
        .route(
            "/style.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css")],
                    include_str!("../ui/dist/style.css"),
                )
            }),
        )
        .layer(DefaultBodyLimit::max(65536))
        .layer(middleware::from_fn_with_state(app.clone(), local_request))
        .with_state(app)
}
