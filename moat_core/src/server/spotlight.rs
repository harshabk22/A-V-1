use crate::System;
use axum::{
    Router, routing::post, extract::Json,
    http::{Request, StatusCode},
    middleware::{self, Next},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tower_http::limit::RequestBodyLimitLayer;

const BODY_LIMIT: usize = 64 * 1024;

#[derive(Deserialize)]
pub struct SpotlightRequest {
    pub query: String,
}

#[derive(Serialize)]
pub struct SpotlightResponse {
    pub answer: String,
}

fn load_or_generate_token() -> String {
    let home = dirs::home_dir().expect("Cannot determine home directory");
    let ageex_dir = home.join(".ageex");
    std::fs::create_dir_all(&ageex_dir).ok();

    let token_path = ageex_dir.join("spotlight.token");

    if token_path.exists() {
        if let Ok(token) = std::fs::read_to_string(&token_path) {
            let trimmed = token.trim().to_string();
            if !trimmed.is_empty() {
                return trimmed;
            }
        }
    }

    let token = uuid::Uuid::new_v4().to_string();
    let _ = std::fs::write(&token_path, &token);
    eprintln!("[Spotlight] Generated token at {}", token_path.display());
    token
}

pub fn router() -> Router {
    let token = load_or_generate_token();
    eprintln!("[Spotlight] Server token: {}", token);

    let expected_auth = Arc::new(format!("Bearer {}", token));

    Router::new()
        .route("/spotlight/query", post(handle_spotlight_query))
        .route_layer(middleware::from_fn(move |req: Request<axum::body::Body>, next: Next| {
            let expected = Arc::clone(&expected_auth);
            async move {
                let auth_header = req
                    .headers()
                    .get("Authorization")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");

                if auth_header != *expected {
                    return Err((
                        StatusCode::UNAUTHORIZED,
                        Json(SpotlightResponse {
                            answer: "Unauthorized: invalid or missing token".into(),
                        }),
                    ));
                }

                Ok(next.run(req).await)
            }
        }))
        .layer(RequestBodyLimitLayer::new(BODY_LIMIT))
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
