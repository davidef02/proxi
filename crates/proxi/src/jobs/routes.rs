//! The `/api/jobs`, `/mcp/job` and graph extract routes (port of `mountJobs` in
//! `app/server/jobs/index.ts` and `mountExtract` in `app/server/graph/extract.ts`).

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::graph::extract::{list_sources, needs_mapping};
use crate::jobs::{JobManager, McpRefusal};
use crate::js;

pub fn router<S: Clone + Send + Sync + 'static>(jobs: Arc<JobManager>) -> Router<S> {
    Router::new()
        .route("/api/jobs", get(list_jobs).fallback(crate::server::not_found))
        .route(
            "/api/jobs/{id}",
            get(get_job).fallback(crate::server::not_found),
        )
        .route(
            "/api/jobs/{id}/cancel",
            post(cancel_job).fallback(crate::server::not_found),
        )
        .route(
            "/mcp/job/{id}",
            post(job_mcp)
                .layer(DefaultBodyLimit::max(100 * 1024 * 1024))
                .fallback(crate::server::not_found),
        )
        .route(
            "/api/graph/sources",
            get(graph_sources).fallback(crate::server::not_found),
        )
        .route(
            "/api/graph/extract",
            post(graph_extract).fallback(crate::server::not_found),
        )
        .with_state(jobs)
}

async fn list_jobs(State(jobs): State<Arc<JobManager>>) -> Json<Value> {
    Json(Value::Array(
        jobs.list()
            .into_iter()
            .filter_map(|record| record.as_object().map(super::slim).map(Value::Object))
            .collect(),
    ))
}

async fn get_job(State(jobs): State<Arc<JobManager>>, Path(id): Path<String>) -> Response {
    jobs.get(&id)
        .map(Json)
        .map(IntoResponse::into_response)
        .unwrap_or_else(|| crate::server::json_error(StatusCode::NOT_FOUND, "Job not found"))
}

async fn cancel_job(State(jobs): State<Arc<JobManager>>, Path(id): Path<String>) -> Response {
    if jobs.get(&id).is_none() {
        return crate::server::json_error(StatusCode::NOT_FOUND, "Job not found");
    }
    jobs.cancel(&id);
    Json(json!({ "ok": true })).into_response()
}

async fn job_mcp(
    State(jobs): State<Arc<JobManager>>,
    Path(id): Path<String>,
    body: Bytes,
) -> Response {
    let (host, tools) = match jobs.mcp_host(&id) {
        Ok(value) => value,
        Err(McpRefusal::NotFound) => {
            return crate::server::json_error(StatusCode::NOT_FOUND, "Job not found");
        }
        Err(McpRefusal::NotRunning) => {
            return crate::server::json_error(StatusCode::CONFLICT, "Job is not running");
        }
    };
    let body = match js::parse(&String::from_utf8_lossy(&body)) {
        Ok(body) => body,
        Err(_) => return crate::server::json_error(StatusCode::BAD_REQUEST, "Invalid JSON body"),
    };
    match crate::mcp::handle_as(&body, &tools, &host, "job").await {
        Some(value) => Json(value).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

async fn graph_sources(State(jobs): State<Arc<JobManager>>) -> Response {
    let data_root = jobs.opts.data_root.clone();
    let graph = jobs.opts.graph.clone();
    match tokio::task::spawn_blocking(move || list_sources(&data_root, &graph)).await {
        Ok(sources) => Json(sources).into_response(),
        Err(_) => crate::server::json_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal error"),
    }
}

async fn graph_extract(State(jobs): State<Arc<JobManager>>, body: Bytes) -> Response {
    let parsed = js::parse(&String::from_utf8_lossy(&body)).unwrap_or_else(|_| json!({}));
    if parsed.is_null() {
        return crate::server::json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Cannot read properties of null (reading 'sources')",
        );
    }
    let res = tokio::task::spawn_blocking(move || {
        let sources = parsed.as_object().and_then(|object| object.get("sources"));
        let all = list_sources(&jobs.opts.data_root, &jobs.opts.graph);
        let (picked_ids, force) = if let Some(ids) = sources.and_then(Value::as_array) {
            let ids: Vec<String> = ids.iter().filter_map(Value::as_str).map(str::to_string).collect();
            let bad: Vec<&str> = ids
                .iter()
                .map(String::as_str)
                .filter(|id| !all.iter().any(|source| source["id"].as_str() == Some(id)))
                .collect();
            if !bad.is_empty() {
                return Err((
                    StatusCode::BAD_REQUEST,
                    format!("unknown source: {}", bad.join(", ")),
                ));
            }
            (
                all.iter()
                    .filter(|source| source["id"].as_str().is_some_and(|id| ids.iter().any(|i| i == id)))
                    .filter_map(|source| source["id"].as_str().map(str::to_string))
                    .collect::<Vec<_>>(),
                true,
            )
        } else {
            (
                all.iter()
                    .filter(|source| needs_mapping(source))
                    .filter_map(|source| source["id"].as_str().map(str::to_string))
                    .collect(),
                false,
            )
        };

        let mut started = Vec::new();
        for id in picked_ids {
            match jobs.start_map_from_all(&all, &id, None, force) {
                Ok(Some(record)) => started.push(record),
                Ok(None) => {}
                Err(message) => return Err((StatusCode::INTERNAL_SERVER_ERROR, message)),
            }
        }
        Ok(started)
    })
    .await;

    match res {
        Ok(Ok(started)) => Json(json!({ "jobs": started })).into_response(),
        Ok(Err((status, message))) => crate::server::json_error(status, &message),
        Err(_) => crate::server::json_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal error"),
    }
}
