//! Small blocking HTTPS helpers for background threads.

#[cfg(feature = "https")]
fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(60)))
        .build()
        .into()
}

#[cfg(feature = "https")]
const USER_AGENT: &str = concat!(
    "Ferramp/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/SpankulatorX/ferramp)"
);

/// GETs `url` and returns at most `max` bytes of the body.
#[cfg(feature = "https")]
pub fn get(url: &str, max: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let resp = agent()
        .get(url)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| e.to_string())?;
    let mut data = Vec::new();
    resp.into_body()
        .into_reader()
        .take(max)
        .read_to_end(&mut data)
        .map_err(|e| e.to_string())?;
    Ok(data)
}

/// POSTs a JSON body and returns the parsed JSON reply.
#[cfg(feature = "https")]
pub fn post_json(url: &str, body: &serde_json::Value) -> Result<serde_json::Value, String> {
    use std::io::Read;
    let resp = agent()
        .post(url)
        .header("User-Agent", USER_AGENT)
        .header("Content-Type", "application/json")
        .send(body.to_string())
        .map_err(|e| e.to_string())?;
    let mut data = Vec::new();
    resp.into_body()
        .into_reader()
        .take(4 << 20)
        .read_to_end(&mut data)
        .map_err(|e| e.to_string())?;
    serde_json::from_slice(&data).map_err(|e| e.to_string())
}

#[cfg(not(feature = "https"))]
pub fn get(_url: &str, _max: u64) -> Result<Vec<u8>, String> {
    Err("this build has no https support".into())
}

#[cfg(not(feature = "https"))]
pub fn post_json(_url: &str, _body: &serde_json::Value) -> Result<serde_json::Value, String> {
    Err("this build has no https support".into())
}
