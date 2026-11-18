//! API errors and their JSON shape: `{ "error": code, "message": … }`, with
//! `path` for validation errors and `current` for version conflicts (as in
//! Board), so the UI and MCP clients handle them the same way.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0} not found")]
    NotFound(&'static str),
    /// Not found, saying exactly what (Clients' own words, for a brand).
    #[error("{0}")]
    Missing(String),
    #[error("changed since you loaded it")]
    Conflict { current: Value },
    #[error("{message}")]
    State { code: &'static str, message: String },
    #[error("{message}")]
    Invalid { path: String, message: String },
    #[error("{0}")]
    BadRequest(String),
    #[error("not signed in")]
    Unauthorized,
    #[error("{0}")]
    Forbidden(&'static str),
    #[error("{0}")]
    Unavailable(String),
    #[error("{0}")]
    TooLarge(String),
    #[error("too many requests: try again in a minute")]
    TooMany,
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

pub type AppResult<T> = Result<T, AppError>;

impl AppError {
    pub fn invalid(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Invalid {
            path: path.into(),
            message: message.into(),
        }
    }

    fn status(&self) -> StatusCode {
        match self {
            Self::NotFound(_) | Self::Missing(_) => StatusCode::NOT_FOUND,
            Self::Conflict { .. } | Self::State { .. } => StatusCode::CONFLICT,
            Self::Invalid { .. } => StatusCode::UNPROCESSABLE_ENTITY,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::TooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            Self::TooMany => StatusCode::TOO_MANY_REQUESTS,
            Self::Database(_) | Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn body(&self) -> Value {
        match self {
            Self::NotFound(_) | Self::Missing(_) => {
                json!({ "error": "not_found", "message": self.to_string() })
            }
            Self::Conflict { current } => {
                json!({ "error": "conflict", "message": self.to_string(), "current": current })
            }
            Self::State { code, message } => json!({ "error": code, "message": message }),
            Self::Invalid { path, message } => {
                json!({ "error": "invalid", "path": path, "message": message })
            }
            Self::BadRequest(m) => json!({ "error": "bad_request", "message": m }),
            Self::Unauthorized => json!({ "error": "unauthorized", "message": self.to_string() }),
            Self::Forbidden(m) => json!({ "error": "forbidden", "message": m }),
            Self::Unavailable(m) => json!({ "error": "unavailable", "message": m }),
            Self::TooLarge(m) => json!({ "error": "too_large", "message": m }),
            Self::TooMany => json!({ "error": "rate_limited", "message": self.to_string() }),
            Self::Database(_) | Self::Internal(_) => {
                json!({ "error": "internal", "message": "internal error" })
            }
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        if status.is_server_error() {
            tracing::error!(error = ?self, "request failed");
        }
        (status, Json(self.body())).into_response()
    }
}
