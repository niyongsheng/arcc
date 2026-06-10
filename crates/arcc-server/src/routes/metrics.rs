use axum::{http::StatusCode, response::IntoResponse};
use metrics_exporter_prometheus::PrometheusHandle;

/// GET /metrics — Prometheus text-format endpoint.
///
/// The `PrometheusHandle` is injected via axum state.
pub async fn handler(
    axum::extract::State(handle): axum::extract::State<PrometheusHandle>,
) -> impl IntoResponse {
    let body = handle.render();
    (StatusCode::OK, body)
}
