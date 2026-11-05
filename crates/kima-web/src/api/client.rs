//! The typed fetch client: `T -> JsJson` request bodies, `JsJson -> T` responses, non-2xx mapped to
//! the backend's `{"error": ...}` message, a bearer token attached from the session.
//!
//! Only the pure parts (request building, response decoding) are unit-tested natively: vertigo's fetch
//! mock (`ApiFetch::set_mock_handler`) is `#[cfg(test)]` inside the vertigo crate and not reachable from
//! here. The async `send` path is exercised for real against kima-server (README, "Verified").

use std::future::Future;
use std::rc::Rc;

use kima_api_types::error::ApiErrorBody;
use serde::Serialize;
use serde::de::DeserializeOwned;

use vertigo::{Computed, FetchMethod, JsJson, RequestBuilder, Resource, Value, get_driver};

use super::codec;

/// A failed call. `status` is `None` for a network failure (no response at all).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub status: Option<u32>,
    pub message: String,
}

impl ApiError {
    pub fn is_unauthorized(&self) -> bool {
        self.status == Some(401)
    }
}

/// Turns a response into `T` (2xx) or the backend's error message (anything else).
pub fn decode_response<T: DeserializeOwned>(status: u32, body: JsJson) -> Result<T, ApiError> {
    if (200..300).contains(&status) {
        return codec::decode(body).map_err(|message| ApiError {
            status: Some(status),
            message,
        });
    }
    let message = match codec::decode::<ApiErrorBody>(body) {
        Ok(err) => err.error,
        Err(_) => format!("Request failed ({status})"),
    };
    Err(ApiError {
        status: Some(status),
        message,
    })
}

/// The request as it goes over the wire (test-only mirror of what `send` builds, to pin headers/body): JSON body, `Authorization: Bearer <token>` when signed in.
#[cfg(test)]
pub fn build_request<B: Serialize>(
    method: FetchMethod,
    path: &str,
    body: Option<&B>,
    token: Option<String>,
) -> Result<vertigo::dev::SsrFetchRequest, String> {
    let mut builder = RequestBuilder::new(method, path);
    if let Some(body) = body {
        builder = builder.body_json(codec::encode(body)?);
    }
    Ok(builder.to_request(token))
}

/// Same-origin API client (`/api/...`): in dev the vertigo server proxies it to kima-server.
#[derive(Clone)]
pub struct ApiClient {
    token: Computed<Option<String>>,
    on_unauthorized: Rc<dyn Fn()>,
}

impl ApiClient {
    pub fn new(token: Computed<Option<String>>, on_unauthorized: impl Fn() + 'static) -> Self {
        Self {
            token,
            on_unauthorized: Rc::new(on_unauthorized),
        }
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        self.send::<(), T>(FetchMethod::GET, path, None).await
    }

    pub async fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, ApiError> {
        self.send(FetchMethod::POST, path, Some(body)).await
    }

    /// A POST without a body.
    pub async fn post_empty<T: DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        self.send::<(), T>(FetchMethod::POST, path, None).await
    }

    async fn send<B: Serialize, T: DeserializeOwned>(
        &self,
        method: FetchMethod,
        path: &str,
        body: Option<&B>,
    ) -> Result<T, ApiError> {
        let mut builder = RequestBuilder::new(method, path).bearer_auth(self.token.clone());
        if let Some(body) = body {
            builder = builder.body_json(codec::encode(body).map_err(|message| ApiError {
                status: None,
                message,
            })?);
        }
        let response = builder.call().await;
        let (status, json) = response
            .into(|status, body| match body {
                vertigo::RequestBody::Json(json) => Some(Ok((status, json))),
            })
            .map_err(|message| ApiError {
                status: None,
                message,
            })?;
        let result = decode_response(status, json);
        if let Err(err) = &result
            && err.is_unauthorized()
        {
            (self.on_unauthorized)();
        }
        result
    }
}

/// Runs `fut` in the browser and exposes it as a loading/ready/error `Resource`. On the server (SSR)
/// nothing is fetched: the page stays in `Loading` and the browser takes over.
pub fn load<T: PartialEq + 'static>(
    fut: impl Future<Output = Result<T, ApiError>> + 'static,
) -> Computed<Resource<Rc<T>>> {
    let state = Value::new(Resource::Loading);
    if get_driver().is_browser() {
        get_driver().spawn({
            let state = state.clone();
            async move {
                state.set(match fut.await {
                    Ok(data) => Resource::Ready(Rc::new(data)),
                    Err(err) => Resource::Error(err.message),
                });
            }
        });
    }
    state.to_computed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kima_api_types::auth::Credentials;
    use kima_api_types::library::ArtistsResponse;
    use vertigo::dev::SsrFetchRequestBody;

    fn json(text: &str) -> JsJson {
        codec::value_to_js(serde_json::from_str(text).unwrap())
    }

    #[test]
    fn success_decodes_into_the_dto() {
        let fixture =
            include_str!("../../../kima-api-types/tests/fixtures/library_artists_response.json");
        let got: ArtistsResponse = decode_response(200, json(fixture)).unwrap();
        assert_eq!(got.artists[0].name, "Disc Band");
    }

    #[test]
    fn error_status_surfaces_the_backend_message() {
        let fixture = include_str!("../../../kima-api-types/tests/fixtures/error_response.json");
        let message = serde_json::from_str::<ApiErrorBody>(fixture).unwrap().error;
        let err = decode_response::<ArtistsResponse>(401, json(fixture)).unwrap_err();
        assert_eq!(err.message, message);
        assert!(err.is_unauthorized());
    }

    #[test]
    fn rate_limit_message_is_shown_verbatim() {
        let err = decode_response::<ArtistsResponse>(
            429,
            json(r#"{"error":"Too many failed attempts. Try again in 42 seconds."}"#),
        )
        .unwrap_err();
        assert_eq!(err.status, Some(429));
        assert_eq!(
            err.message,
            "Too many failed attempts. Try again in 42 seconds."
        );
    }

    #[test]
    fn error_without_a_known_body_falls_back_to_the_status() {
        let err = decode_response::<ArtistsResponse>(502, json(r#"{"x":1}"#)).unwrap_err();
        assert_eq!(err.message, "Request failed (502)");
    }

    #[test]
    fn bearer_token_is_attached_only_when_set() {
        let creds = Credentials {
            username: "a".into(),
            password: "b".into(),
        };
        let signed_in = build_request(
            FetchMethod::POST,
            "/api/auth/login",
            Some(&creds),
            Some("tok".into()),
        )
        .unwrap();
        assert_eq!(signed_in.headers["Authorization"], "Bearer tok");
        assert_eq!(
            signed_in.headers["Content-Type"],
            "application/json;charset=UTF-8"
        );
        assert!(matches!(signed_in.body, SsrFetchRequestBody::Data { .. }));

        let anon = build_request::<()>(FetchMethod::GET, "/api/x", None, None).unwrap();
        assert!(!anon.headers.contains_key("Authorization"));
        assert!(matches!(anon.body, SsrFetchRequestBody::None));
    }
}
