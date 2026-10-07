//! HTTP helpers shared by the online lyrics providers.

use anyhow::{bail, Context as _};
use serde::de::DeserializeOwned;
use std::time::Duration;

/// Largest response body read. Real lyrics answers are far smaller, and a
/// server that sends more is not the service it claims to be.
pub(crate) const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// A client that sends [`crate::USER_AGENT`] and gives up on a request after
/// `timeout`. Without `use_system_proxy` it ignores the machine's proxy
/// settings, for tests against a local server.
pub(crate) fn client(
    timeout: Duration,
    use_system_proxy: bool,
) -> reqwest::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .user_agent(crate::USER_AGENT)
        .timeout(timeout);
    if !use_system_proxy {
        builder = builder.no_proxy();
    }
    builder.build()
}

/// GETs `url` with `params` and decodes the JSON body. Every status other
/// than a success is an error. `what` names the request in errors, such as
/// `NetEase /api/song/lyric`; errors never include the URL, so a key sent
/// as a parameter stays out of logs.
pub(crate) async fn get_json<T: DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
    params: &[(&str, &str)],
    what: &str,
) -> anyhow::Result<T> {
    let response = client
        .get(url)
        .query(params)
        .send()
        .await
        .map_err(reqwest::Error::without_url)
        .with_context(|| format!("{what} request failed"))?;
    let status = response.status();
    if !status.is_success() {
        bail!("{what} answered HTTP {status}");
    }
    let body = read_body(response, what).await?;
    serde_json::from_slice(&body).with_context(|| format!("{what} sent an unexpected response"))
}

/// Reads a response body of at most [`MAX_BODY_BYTES`]. `what` names the
/// request in errors, which never include the URL.
pub(crate) async fn read_body(
    mut response: reqwest::Response,
    what: &str,
) -> anyhow::Result<Vec<u8>> {
    let too_big = || anyhow::anyhow!("{what} sent more than {MAX_BODY_BYTES} bytes");
    if response
        .content_length()
        .is_some_and(|len| len > MAX_BODY_BYTES as u64)
    {
        return Err(too_big());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(reqwest::Error::without_url)
        .with_context(|| format!("could not read the {what} response"))?
    {
        if body.len().saturating_add(chunk.len()) > MAX_BODY_BYTES {
            return Err(too_big());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
