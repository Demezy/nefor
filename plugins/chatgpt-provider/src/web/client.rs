use std::time::Duration;

use reqwest::header::{HeaderValue, ACCEPT, CONTENT_TYPE};
use tokio_util::sync::CancellationToken;

use crate::auth::AuthSnapshot;
use crate::error::ChatgptError;
use crate::responses::headers;

use super::request::{SearchEndpointResponse, SearchRequest, SearchResponse};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_ERROR_BODY_CHARS: usize = 4_096;

#[derive(Debug, thiserror::Error)]
pub enum WebError {
    #[error(transparent)]
    ProviderHttp(#[from] nefor_provider_http::ProviderHttpError),
    #[error("could not build web request headers: {0}")]
    Headers(#[source] ChatgptError),
    #[error("web request is missing a stable session identity")]
    MissingSessionIdentity,
    #[error("web request is missing an acknowledged model")]
    MissingModel,
    #[error("failed to encode web request: {0}")]
    Encode(#[source] serde_json::Error),
    #[error("web HTTP transport error: {0}")]
    Transport(#[source] reqwest::Error),
    #[error("web endpoint returned {status}: {body}")]
    Endpoint { status: u16, body: String },
    #[error("web endpoint provider error: {diagnostic}")]
    Provider { diagnostic: String },
    #[error("failed to decode web response: {0}")]
    Decode(#[source] serde_json::Error),
    #[error("web request cancelled")]
    Cancelled,
}

/// Standalone client for `POST {provider-base}/alpha/search`.
///
/// Authentication is supplied per call by the provider dispatcher. The client
/// owns no token store or invocation registry and can therefore share the same
/// refreshed auth snapshot and cancellation lifecycle as its caller.
#[derive(Debug, Clone)]
pub struct WebClient {
    http: reqwest::Client,
    base_url: String,
    installation_id: String,
    originator: String,
}

impl WebClient {
    pub fn new(
        base_url: String,
        installation_id: String,
        originator: String,
    ) -> Result<Self, WebError> {
        let (builder, roots) = nefor_provider_http::client_builder()?;
        tracing::info!(
            native_roots = roots.loaded,
            rejected_native_roots = roots.rejected,
            "web HTTPS trust initialized"
        );
        let http = builder
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(WebError::Transport)?;
        Ok(Self::with_http(http, base_url, installation_id, originator))
    }

    pub fn with_http(
        http: reqwest::Client,
        base_url: String,
        installation_id: String,
        originator: String,
    ) -> Self {
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            installation_id,
            originator,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn installation_id(&self) -> &str {
        &self.installation_id
    }

    pub fn originator(&self) -> &str {
        &self.originator
    }

    pub async fn execute(
        &self,
        request: &SearchRequest,
        auth: &AuthSnapshot,
        cancellation: &CancellationToken,
    ) -> Result<SearchResponse, WebError> {
        if request.id.trim().is_empty() {
            return Err(WebError::MissingSessionIdentity);
        }
        if request.model.trim().is_empty() {
            return Err(WebError::MissingModel);
        }

        let url = format!("{}/alpha/search", self.base_url);
        let body = serde_json::to_vec(request).map_err(WebError::Encode)?;
        let mut request_headers =
            headers::build_headers(auth, &self.installation_id, &self.originator)
                .map_err(WebError::Headers)?;
        request_headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        request_headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let send = self
            .http
            .post(url)
            .headers(request_headers)
            .body(body)
            .timeout(REQUEST_TIMEOUT)
            .send();
        let response = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(WebError::Cancelled),
            response = send => response.map_err(WebError::Transport)?,
        };

        let status = response.status();
        let bytes = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(WebError::Cancelled),
            bytes = response.bytes() => bytes.map_err(WebError::Transport)?,
        };

        if !status.is_success() {
            return Err(WebError::Endpoint {
                status: status.as_u16(),
                body: bounded_diagnostic(&bytes),
            });
        }

        match serde_json::from_slice(&bytes).map_err(WebError::Decode)? {
            SearchEndpointResponse::Success(response) => Ok(response),
            SearchEndpointResponse::ProviderError(envelope) => Err(WebError::Provider {
                diagnostic: bounded_json_diagnostic(&envelope.error),
            }),
        }
    }
}

fn bounded_json_diagnostic(value: &serde_json::Value) -> String {
    value
        .to_string()
        .chars()
        .take(MAX_ERROR_BODY_CHARS)
        .collect()
}

fn bounded_diagnostic(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .take(MAX_ERROR_BODY_CHARS)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;

    use serde_json::{json, Value};

    use crate::auth::store::{AccessToken, ChatgptAccountId, RefreshToken, TokenData};
    use crate::auth::{AuthSnapshot, AuthState, TokenSource};

    use super::*;
    use crate::web::request::{SearchQuery, WebCommand};

    struct CapturedRequest {
        url: String,
        headers: Vec<(String, String)>,
        body: Value,
    }

    fn auth() -> AuthSnapshot {
        AuthSnapshot {
            tokens: Some(TokenData {
                id_token: "header.payload.signature".into(),
                access_token: AccessToken("fixture-access-token".into()),
                refresh_token: RefreshToken("fixture-refresh-token".into()),
                account_id: Some(ChatgptAccountId("acct-fixture".into())),
            }),
            state: AuthState::Connected,
            source: Some(TokenSource::Oauth),
        }
    }

    fn test_client(base_url: String) -> WebClient {
        WebClient::with_http(
            reqwest::Client::new(),
            base_url,
            "installation-fixture".into(),
            "originator-fixture".into(),
        )
    }

    fn spawn_server(
        status: u16,
        response_body: String,
    ) -> (
        String,
        mpsc::Receiver<CapturedRequest>,
        thread::JoinHandle<()>,
    ) {
        let server = tiny_http::Server::http("127.0.0.1:0").expect("bind server");
        let addr = server.server_addr().to_ip().expect("IP server address");
        let (captured_tx, captured_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let mut request = server.recv().expect("receive request");
            let mut raw_body = String::new();
            request
                .as_reader()
                .read_to_string(&mut raw_body)
                .expect("read request body");
            let captured = CapturedRequest {
                url: request.url().to_string(),
                headers: request
                    .headers()
                    .iter()
                    .map(|header| (header.field.to_string(), header.value.to_string()))
                    .collect(),
                body: serde_json::from_str(&raw_body).expect("JSON request body"),
            };
            captured_tx.send(captured).expect("capture request");
            request
                .respond(
                    tiny_http::Response::from_string(response_body)
                        .with_status_code(status)
                        .with_header(
                            "content-type: application/json"
                                .parse::<tiny_http::Header>()
                                .expect("response header"),
                        ),
                )
                .expect("respond");
        });
        (format!("http://{addr}/provider/"), captured_rx, handle)
    }

    fn header<'a>(request: &'a CapturedRequest, name: &str) -> Option<&'a str> {
        request
            .headers
            .iter()
            .find(|(field, _)| field.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    #[tokio::test]
    async fn posts_alpha_search_with_shared_headers_and_preserves_opaque_results() {
        let (base_url, captured_rx, server) = spawn_server(
            200,
            r#"{
                "encrypted_output":"opaque-ciphertext",
                "output":"exact endpoint text",
                "results":[{
                    "type":"future_result",
                    "ref_id":"turn0search0",
                    "unknown":{"nested":[1,true,null]}
                }]
            }"#
            .into(),
        );
        let client = test_client(base_url);
        let request = SearchRequest::new(
            "stable-conversation",
            "gpt-test",
            WebCommand::ImageSearch(SearchQuery {
                q: "waterfalls".into(),
                recency: Some(2),
                domains: Some(vec!["example.com".into()]),
            }),
        );

        let response = client
            .execute(&request, &auth(), &CancellationToken::new())
            .await
            .expect("web response");
        assert_eq!(response.output, "exact endpoint text");
        assert_eq!(
            response.encrypted_output.as_deref(),
            Some("opaque-ciphertext")
        );
        assert_eq!(
            response.results,
            Some(vec![json!({
                "type": "future_result",
                "ref_id": "turn0search0",
                "unknown": {"nested": [1, true, null]}
            })])
        );

        let captured = captured_rx.recv().expect("captured request");
        assert_eq!(captured.url, "/provider/alpha/search");
        assert_eq!(
            header(&captured, "authorization"),
            Some("Bearer fixture-access-token")
        );
        assert_eq!(
            header(&captured, "chatgpt-account-id"),
            Some("acct-fixture")
        );
        assert_eq!(
            header(&captured, "x-codex-installation-id"),
            Some("installation-fixture")
        );
        assert_eq!(header(&captured, "originator"), Some("originator-fixture"));
        assert_eq!(header(&captured, "session-id"), None);
        assert_eq!(header(&captured, "thread-id"), None);
        assert_eq!(header(&captured, "x-codex-turn-state"), None);
        assert_eq!(header(&captured, "accept"), Some("application/json"));
        assert_eq!(
            captured.body,
            json!({
                "id": "stable-conversation",
                "model": "gpt-test",
                "commands": {
                    "image_query": [{
                        "q": "waterfalls",
                        "recency": 2,
                        "domains": ["example.com"]
                    }]
                },
                "settings": {
                    "allowed_callers": ["direct"],
                    "external_web_access": true
                }
            })
        );
        assert!(captured.body.get("tools").is_none());
        server.join().expect("server thread");
    }

    #[tokio::test]
    async fn reports_non_success_status_with_bounded_body() {
        let long_body = "denied ".repeat(1_000);
        let (base_url, _captured_rx, server) = spawn_server(403, long_body);
        let client = test_client(base_url);
        let request = SearchRequest::new(
            "stable-conversation",
            "gpt-test",
            WebCommand::Search(SearchQuery {
                q: "query".into(),
                recency: None,
                domains: None,
            }),
        );

        let error = client
            .execute(&request, &auth(), &CancellationToken::new())
            .await
            .expect_err("403 should fail");
        match error {
            WebError::Endpoint { status, body } => {
                assert_eq!(status, 403);
                assert_eq!(body.chars().count(), MAX_ERROR_BODY_CHARS);
                assert!(body.starts_with("denied "));
            }
            other => panic!("unexpected error: {other}"),
        }
        server.join().expect("server thread");
    }

    #[tokio::test]
    async fn explicit_provider_error_envelope_is_not_reported_as_a_decode_failure() {
        let (base_url, _captured_rx, server) = spawn_server(
            200,
            include_str!("../../tests/fixtures/web/provider-error-envelope.json").into(),
        );
        let client = test_client(base_url);
        let request = SearchRequest::new(
            "stable-conversation",
            "gpt-test",
            WebCommand::Search(SearchQuery {
                q: "query".into(),
                recency: None,
                domains: None,
            }),
        );

        let error = client
            .execute(&request, &auth(), &CancellationToken::new())
            .await
            .expect_err("provider envelope should fail");
        match error {
            WebError::Provider { diagnostic } => {
                assert!(diagnostic.contains("invalid_reference"));
                assert!(diagnostic.contains("invalid web reference"));
            }
            other => panic!("unexpected error: {other}"),
        }
        server.join().expect("server thread");
    }

    #[tokio::test]
    async fn reports_malformed_success_json_as_decode_error() {
        let (base_url, _captured_rx, server) = spawn_server(200, "not-json".into());
        let client = test_client(base_url);
        let request = SearchRequest::new(
            "stable-conversation",
            "gpt-test",
            WebCommand::Search(SearchQuery {
                q: "query".into(),
                recency: None,
                domains: None,
            }),
        );

        assert!(matches!(
            client
                .execute(&request, &auth(), &CancellationToken::new())
                .await,
            Err(WebError::Decode(_))
        ));
        server.join().expect("server thread");
    }

    #[tokio::test]
    async fn missing_model_and_session_identity_fail_before_http() {
        let client = test_client("http://127.0.0.1:1".into());
        let mut request = SearchRequest::new(
            "stable-conversation",
            "",
            WebCommand::Search(SearchQuery {
                q: "query".into(),
                recency: None,
                domains: None,
            }),
        );
        assert!(matches!(
            client
                .execute(&request, &auth(), &CancellationToken::new())
                .await,
            Err(WebError::MissingModel)
        ));

        request.id.clear();
        request.model = "gpt-test".into();
        assert!(matches!(
            client
                .execute(&request, &auth(), &CancellationToken::new())
                .await,
            Err(WebError::MissingSessionIdentity)
        ));
    }

    #[tokio::test]
    async fn independent_requests_can_settle_out_of_order() {
        fn delayed_server(
            delay: Duration,
            output: &'static str,
        ) -> (String, thread::JoinHandle<()>) {
            let server = tiny_http::Server::http("127.0.0.1:0").expect("bind server");
            let addr = server.server_addr().to_ip().expect("IP server address");
            let handle = thread::spawn(move || {
                let request = server.recv().expect("receive request");
                thread::sleep(delay);
                request
                    .respond(tiny_http::Response::from_string(format!(
                        r#"{{"encrypted_output":null,"output":"{output}"}}"#
                    )))
                    .expect("respond");
            });
            (format!("http://{addr}"), handle)
        }

        let (slow_url, slow_server) = delayed_server(Duration::from_millis(100), "slow");
        let (fast_url, fast_server) = delayed_server(Duration::from_millis(5), "fast");
        let request = SearchRequest::new(
            "stable-conversation",
            "gpt-test",
            WebCommand::Search(SearchQuery {
                q: "query".into(),
                recency: None,
                domains: None,
            }),
        );
        let slow_client = test_client(slow_url);
        let fast_client = test_client(fast_url);
        let cancellation = CancellationToken::new();
        let auth = auth();

        let (slow, fast) = tokio::join!(
            slow_client.execute(&request, &auth, &cancellation),
            fast_client.execute(&request, &auth, &cancellation),
        );
        assert_eq!(slow.expect("slow response").output, "slow");
        assert_eq!(fast.expect("fast response").output, "fast");
        slow_server.join().expect("slow server thread");
        fast_server.join().expect("fast server thread");
    }

    #[tokio::test]
    async fn cancellation_interrupts_an_in_flight_response() {
        let server = tiny_http::Server::http("127.0.0.1:0").expect("bind server");
        let addr = server.server_addr().to_ip().expect("IP server address");
        let (started_tx, started_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let request = server.recv().expect("receive request");
            started_tx.send(()).expect("request started");
            thread::sleep(Duration::from_millis(200));
            let _ = request.respond(tiny_http::Response::from_string(
                r#"{"encrypted_output":null,"output":"late"}"#,
            ));
        });
        let client = test_client(format!("http://{addr}"));
        let request = SearchRequest::new(
            "stable-conversation",
            "gpt-test",
            WebCommand::Search(SearchQuery {
                q: "query".into(),
                recency: None,
                domains: None,
            }),
        );
        let cancellation = CancellationToken::new();
        let cancel_from_thread = cancellation.clone();
        let canceller = thread::spawn(move || {
            started_rx.recv().expect("request observed");
            cancel_from_thread.cancel();
        });

        assert!(matches!(
            client.execute(&request, &auth(), &cancellation).await,
            Err(WebError::Cancelled)
        ));
        canceller.join().expect("canceller thread");
        handle.join().expect("server thread");
    }
}
