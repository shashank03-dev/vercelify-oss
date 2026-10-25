//! JSON bodies, in and out, on plain serde_json. axum's own `Json` runs
//! every request type through `serde_path_to_error`, which compiles a second
//! copy of each deserializer; this one keeps the agent inside its size
//! budget (design §16) and answers the same statuses: 415 without a JSON
//! content type, 400 for broken JSON, 422 for the wrong shape.

use crate::error::AppError;
use axum::extract::{FromRequest, OptionalFromRequest, Request};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use serde::de::DeserializeOwned;
use serde::Serialize;

#[derive(Debug, Clone, Copy, Default)]
pub struct Json<T>(pub T);

fn is_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap_or("").trim().to_ascii_lowercase())
        .is_some_and(|m| m == "application/json" || (m.starts_with("application/") && m.ends_with("+json")))
}

/// Not generic, so it is compiled once.
fn rejection(e: serde_json::Error) -> AppError {
    let status = match e.classify() {
        serde_json::error::Category::Data => StatusCode::UNPROCESSABLE_ENTITY,
        _ => StatusCode::BAD_REQUEST,
    };
    AppError {
        status,
        message: format!("invalid JSON body: {e}"),
    }
}

fn unsupported() -> AppError {
    AppError {
        status: StatusCode::UNSUPPORTED_MEDIA_TYPE,
        message: "expected a JSON body (Content-Type: application/json)".into(),
    }
}

async fn read<S: Send + Sync>(req: Request, state: &S) -> Result<Bytes, AppError> {
    Bytes::from_request(req, state).await.map_err(|e| AppError {
        status: e.status(),
        message: e.body_text(),
    })
}

impl<T: DeserializeOwned> Json<T> {
    pub fn from_bytes(b: &[u8]) -> Result<Self, AppError> {
        serde_json::from_slice(b).map(Json).map_err(rejection)
    }
}

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for Json<T> {
    type Rejection = AppError;

    async fn from_request(req: Request, state: &S) -> Result<Self, AppError> {
        if !is_json(req.headers()) {
            return Err(unsupported());
        }
        Self::from_bytes(&read(req, state).await?)
    }
}

/// `Option<Json<T>>`: no Content-Type means no body.
impl<T: DeserializeOwned, S: Send + Sync> OptionalFromRequest<S> for Json<T> {
    type Rejection = AppError;

    async fn from_request(req: Request, state: &S) -> Result<Option<Self>, AppError> {
        if req.headers().get(header::CONTENT_TYPE).is_none() {
            return Ok(None);
        }
        if !is_json(req.headers()) {
            return Err(unsupported());
        }
        Self::from_bytes(&read(req, state).await?).map(Some)
    }
}

impl<T: Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        match serde_json::to_vec(&self.0) {
            Ok(body) => (
                [(header::CONTENT_TYPE, HeaderValue::from_static("application/json"))],
                body,
            )
                .into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses() {
        let e = Json::<Vec<u32>>::from_bytes(b"[1,").unwrap_err();
        assert_eq!(e.status, StatusCode::BAD_REQUEST);
        let e = Json::<Vec<u32>>::from_bytes(br#"["a"]"#).unwrap_err();
        assert_eq!(e.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(Json::<Vec<u32>>::from_bytes(b"[1, 2]").unwrap().0, [1, 2]);
        let mut h = HeaderMap::new();
        assert!(!is_json(&h));
        h.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
        assert!(is_json(&h));
        h.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/merge-patch+json"),
        );
        assert!(is_json(&h));
        h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
        assert!(!is_json(&h));
    }
}
