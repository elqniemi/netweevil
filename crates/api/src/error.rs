use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use netweevil_query::{AnalysisDiagnostic, analysis_failure};
use serde::Serialize;

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    diagnostics: Vec<AnalysisDiagnostic>,
}

#[derive(Debug)]
pub(crate) struct ApiError {
    status: StatusCode,
    message: String,
    diagnostics: Vec<AnalysisDiagnostic>,
}

impl ApiError {
    pub(crate) fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
            diagnostics: Vec::new(),
        }
    }

    pub(crate) fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
            diagnostics: Vec::new(),
        }
    }

    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
            diagnostics: Vec::new(),
        }
    }

    pub(crate) fn from_execution_error(error: anyhow::Error) -> Self {
        if let Some(failure) = analysis_failure(&error) {
            Self {
                status: StatusCode::BAD_REQUEST,
                message: failure.message.clone(),
                diagnostics: failure.diagnostics.clone(),
            }
        } else {
            Self::bad_request(error.to_string())
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorResponse {
                error: self.message,
                diagnostics: self.diagnostics,
            }),
        )
            .into_response()
    }
}
