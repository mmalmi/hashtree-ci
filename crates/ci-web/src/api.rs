//! REST API for CI results.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::get,
    Router,
};
use ci_core::JobResultIndex;
use ci_store::CiStore;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub fn router<S: CiStore + 'static>(store: Arc<S>) -> Router {
    Router::new()
        .route("/api/results/repo/:repo_hash", get(get_repo_results::<S>))
        .route("/api/results/commit/:repo_hash/:commit", get(get_commit_results::<S>))
        .route("/api/results/runner/:npub", get(get_runner_results::<S>))
        .route("/api/result/:hash", get(get_result::<S>))
        .route("/api/logs/:hash", get(get_logs::<S>))
        .route("/api/badge/:repo_hash", get(get_badge::<S>))
        .with_state(store)
}

#[derive(Deserialize)]
pub struct PaginationParams {
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    50
}

async fn get_repo_results<S: CiStore>(
    State(store): State<Arc<S>>,
    Path(repo_hash): Path<String>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<Vec<JobResultIndex>>, StatusCode> {
    store
        .query_by_repo(&repo_hash, params.limit)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn get_commit_results<S: CiStore>(
    State(store): State<Arc<S>>,
    Path((repo_hash, commit)): Path<(String, String)>,
) -> Result<Json<Vec<JobResultIndex>>, StatusCode> {
    store
        .query_by_commit(&repo_hash, &commit)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn get_runner_results<S: CiStore>(
    State(store): State<Arc<S>>,
    Path(npub): Path<String>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<Vec<JobResultIndex>>, StatusCode> {
    store
        .query_by_runner(&npub, params.limit)
        .await
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn get_result<S: CiStore>(
    State(store): State<Arc<S>>,
    Path(hash): Path<String>,
) -> Result<Json<ci_core::JobResult>, StatusCode> {
    match store.get_result(&hash).await {
        Ok(Some(result)) => Ok(Json(result)),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn get_logs<S: CiStore>(
    State(store): State<Arc<S>>,
    Path(hash): Path<String>,
) -> Result<Vec<u8>, StatusCode> {
    match store.get_logs(&hash).await {
        Ok(Some(logs)) => Ok(logs),
        Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

#[derive(Serialize)]
struct Badge {
    schema_version: u8,
    label: String,
    message: String,
    color: String,
}

async fn get_badge<S: CiStore>(
    State(store): State<Arc<S>>,
    Path(repo_hash): Path<String>,
) -> Result<Json<Badge>, StatusCode> {
    let results = store.query_by_repo(&repo_hash, 1).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let (message, color) = match results.first() {
        Some(r) => match r.status {
            ci_core::JobStatus::Success => ("passing", "brightgreen"),
            ci_core::JobStatus::Failure => ("failing", "red"),
            ci_core::JobStatus::Running => ("running", "yellow"),
            _ => ("unknown", "lightgrey"),
        },
        None => ("no builds", "lightgrey"),
    };

    Ok(Json(Badge {
        schema_version: 1,
        label: "CI".to_string(),
        message: message.to_string(),
        color: color.to_string(),
    }))
}
