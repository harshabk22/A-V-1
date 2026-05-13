use crate::System;
use axum::{Router, routing::post, extract::Json};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct SpotlightRequest {
    pub query: String,
}

#[derive(Serialize)]
pub struct SpotlightResponse {
    pub answer: String,
}

pub fn router() -> Router {
    Router::new().route("/spotlight/query", post(handle_spotlight_query))
}

async fn handle_spotlight_query(
    Json(req): Json<SpotlightRequest>,
) -> Json<SpotlightResponse> {
    let result = tokio::task::spawn_blocking(move || {
        let mut system = match System::new_spotlight() {
            Ok(s) => s,
            Err(e) => return SpotlightResponse { answer: format!("Error: {}", e) },
        };
        let rt = tokio::runtime::Runtime::new()
            .expect("spotlight local runtime");
        match rt.block_on(system.process_spotlight_query(&req.query)) {
            Ok(answer) => SpotlightResponse { answer },
            Err(e) => SpotlightResponse { answer: format!("Error: {}", e) },
        }
    })
    .await;

    match result {
        Ok(response) => Json(response),
        Err(e) => Json(SpotlightResponse { answer: format!("Task error: {}", e) }),
    }
}
