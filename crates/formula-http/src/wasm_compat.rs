//! Platform-specific HTTP settings; the formula protocol remains shared.
use std::time::Duration;

/// Configures native connection limits while browser Fetch owns its connection policy.
pub(super) fn http_client(
    timeout: Duration,
) -> Result<reqwest::Client, reqwest::Error> {
    let client = reqwest::Client::builder();
    #[cfg(not(target_arch = "wasm32"))]
    let client = client
        .connect_timeout(timeout.min(Duration::from_secs(10)))
        // Do not forward formula pixels to a redirect target chosen by another service.
        .redirect(reqwest::redirect::Policy::none());
    #[cfg(target_arch = "wasm32")]
    let _ = timeout;
    client.build()
}

/// Sends one request without attaching browser cookies or HTTP authentication state.
pub(super) async fn send(
    request: reqwest::RequestBuilder,
) -> Result<reqwest::Response, reqwest::Error> {
    #[cfg(target_arch = "wasm32")]
    let request = request.fetch_credentials_omit();
    request.send().await?.error_for_status()
}
