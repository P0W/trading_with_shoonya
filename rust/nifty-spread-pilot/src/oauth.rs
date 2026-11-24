use std::fs::File;
use std::io::{Cursor, Read};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use data_encoding::BASE32_NOPAD;
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::connection;

const QUICK_AUTH_URL: &str = "https://api.shoonya.com/NorenWClientAPI/QuickAuth";
const WEB_AUTH_URL: &str = "https://api.shoonya.com/NorenWClientWeb/QuickAuth";
const APP_KEY_URL: &str = "https://api.shoonya.com/NorenWClientWeb/GetAppKeyData";
const AUTH_CODE_URL: &str = "https://api.shoonya.com/NorenWClientAPI/GetAuthCode";
const TOKEN_URL: &str = "https://api.shoonya.com/NorenWClientAPI/GenAcsTok";
const MAX_CONFIG_BYTES: u64 = 65_536;

struct Config {
    user_id: String,
    client_id: String,
    secret: Zeroizing<String>,
    password: Zeroizing<String>,
    totp_key: Zeroizing<Vec<u8>>,
}

struct Endpoints<'a> {
    quick_auth: &'a str,
    web_auth: &'a str,
    app_key: &'a str,
    auth_code: &'a str,
    token: &'a str,
}

#[derive(Debug, PartialEq, Eq)]
enum ExchangeError {
    InvalidVerifier,
    Other(String),
}

#[derive(Clone, Copy)]
enum LoginKind {
    Api,
    Web,
}

pub struct AuthenticatedSession {
    pub user_id: String,
    pub access_token: Zeroizing<String>,
}

pub fn login_session(config_path: &Path) -> Result<AuthenticatedSession, String> {
    let config = read_config(config_path)?;
    let agent = connection::agent();
    let endpoints = Endpoints {
        quick_auth: QUICK_AUTH_URL,
        web_auth: WEB_AUTH_URL,
        app_key: APP_KEY_URL,
        auth_code: AUTH_CODE_URL,
        token: TOKEN_URL,
    };
    let access_token = Zeroizing::new(authenticate(&agent, &endpoints, &config)?);
    Ok(AuthenticatedSession {
        user_id: config.user_id,
        access_token,
    })
}

pub fn default_config_path() -> std::path::PathBuf {
    std::env::var_os("SHOONYA_CONFIG_FILE")
        .or_else(|| std::env::var_os("NIFTY_CONFIG_FILE"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(".env"))
}

pub fn login_and_check() -> Result<String, String> {
    let cfg_path = default_config_path();
    let session = login_session(&cfg_path)?;
    let quote_token = crate::engine::resolve_config_var(&cfg_path, "NIFTY_QUOTE_TOKEN")
        .ok()
        .flatten()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "26000".to_owned());
    let agent = connection::agent();
    connection::check_authenticated(
        &agent,
        &session.access_token,
        &session.user_id,
        &quote_token,
    )
}

fn fresh_factor(key: &[u8]) -> Result<Zeroizing<String>, String> {
    let mut now = unix_time()?;
    let remaining = 30 - now % 30;
    if remaining <= 2 {
        std::thread::sleep(Duration::from_secs(remaining) + Duration::from_millis(100));
        now = unix_time()?;
    }
    Ok(Zeroizing::new(totp(key, now)?))
}

fn authenticate(
    agent: &ureq::Agent,
    endpoints: &Endpoints<'_>,
    config: &Config,
) -> Result<String, String> {
    let factor = fresh_factor(&config.totp_key)?;
    let session = Zeroizing::new(quick_auth(
        agent,
        endpoints.quick_auth,
        config,
        &factor,
        LoginKind::Api,
    )?);
    let code = Zeroizing::new(auth_code(
        agent,
        endpoints.auth_code,
        &config.client_id,
        &session,
    )?);
    match exchange(agent, endpoints.token, config, &config.secret, &code) {
        Ok(token) => Ok(token),
        Err(ExchangeError::Other(error)) => Err(error),
        Err(ExchangeError::InvalidVerifier) => {
            let portal_secret = portal_secret(agent, endpoints, config, &session)?;
            if portal_secret.as_str() == config.secret.as_str() {
                return Err(
                    "Shoonya rejected the verifier although the saved portal key matches .env."
                        .to_owned(),
                );
            }
            let code = Zeroizing::new(auth_code(
                agent,
                endpoints.auth_code,
                &config.client_id,
                &session,
            )?);
            match exchange(agent, endpoints.token, config, &portal_secret, &code) {
                Ok(token) => {
                    eprintln!(
                        "The saved Shoonya portal Secret Code differs from .env; the portal key was used for this login. Update .env securely."
                    );
                    Ok(token)
                }
                Err(ExchangeError::InvalidVerifier) => {
                    Err("Shoonya rejected the verifier using the saved portal key.".to_owned())
                }
                Err(ExchangeError::Other(error)) => Err(error),
            }
        }
    }
}

fn unix_time() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .map_err(|_| "System clock is before the Unix epoch.".to_owned())
}

fn read_config(path: &Path) -> Result<Config, String> {
    let content: Zeroizing<Vec<u8>> = match File::open(path) {
        Ok(file) => {
            let mut buf = Zeroizing::new(Vec::new());
            file.take(MAX_CONFIG_BYTES + 1)
                .read_to_end(&mut buf)
                .map_err(|_| "Cannot read Shoonya .env configuration.".to_owned())?;
            if buf.len() as u64 > MAX_CONFIG_BYTES {
                return Err("Shoonya .env configuration is too large.".to_owned());
            }
            buf
        }
        Err(_) => {
            // Allow process-env-only configuration when all 5 Shoonya keys are not in a .env file;
            // preserve the explicit open error if neither process env nor .env supplies credentials.
            let any_env = [
                "SHOONYA_USER_ID",
                "SHOONYA_CLIENT_ID",
                "SHOONYA_SECRET_CODE",
                "SHOONYA_PASSWORD",
                "SHOONYA_TOTP_SECRET",
            ]
            .iter()
            .any(|k| std::env::var_os(k).is_some());
            if !any_env {
                return Err("Cannot open Shoonya .env configuration.".to_owned());
            }
            Zeroizing::new(Vec::new())
        }
    };
    parse_config_with_env(&content, |k| std::env::var(k).ok())
}

#[cfg(test)]
fn parse_config(content: &[u8]) -> Result<Config, String> {
    parse_config_with_env(content, |_| None)
}

fn parse_config_with_env(
    content: &[u8],
    env_lookup: impl Fn(&str) -> Option<String>,
) -> Result<Config, String> {
    let mut user_id = None;
    let mut client_id = None;
    let mut secret = None;
    let mut password = None;
    let mut totp_secret = None;
    for pair in dotenvy::from_read_iter(Cursor::new(content)) {
        let (key, value) = pair.map_err(|_| "Invalid Shoonya .env configuration.")?;
        let key = Zeroizing::new(key);
        let value = Zeroizing::new(value);
        let slot = match key.as_str() {
            "SHOONYA_USER_ID" => &mut user_id,
            "SHOONYA_CLIENT_ID" => &mut client_id,
            "SHOONYA_SECRET_CODE" => &mut secret,
            "SHOONYA_PASSWORD" => &mut password,
            "SHOONYA_TOTP_SECRET" => &mut totp_secret,
            _ => continue,
        };
        if slot.replace(value).is_some() {
            return Err("Duplicate Shoonya configuration key.".to_owned());
        }
    }
    for (name, slot) in [
        ("SHOONYA_USER_ID", &mut user_id),
        ("SHOONYA_CLIENT_ID", &mut client_id),
        ("SHOONYA_SECRET_CODE", &mut secret),
        ("SHOONYA_PASSWORD", &mut password),
        ("SHOONYA_TOTP_SECRET", &mut totp_secret),
    ] {
        if let Some(env_val) = env_lookup(name) {
            let z = Zeroizing::new(env_val);
            if !z.is_empty() {
                *slot = Some(z);
            }
        }
    }
    let user_id = required(user_id, "SHOONYA_USER_ID")?;
    let client_id = required(client_id, "SHOONYA_CLIENT_ID")?;
    if !valid_identifier(&user_id, false) || !valid_identifier(&client_id, true) {
        return Err("Invalid Shoonya user or client ID in .env.".to_owned());
    }
    let secret = required(secret, "SHOONYA_SECRET_CODE")?;
    if secret.len() > 1024
        || !secret.bytes().all(|c| (33..=126).contains(&c) && c != b'*')
        || secret.as_str() == "your_secret_code"
    {
        return Err("Invalid or masked SHOONYA_SECRET_CODE in .env.".to_owned());
    }
    let password = required(password, "SHOONYA_PASSWORD")?;
    if password.len() > 4096 {
        return Err("Invalid SHOONYA_PASSWORD in .env.".to_owned());
    }
    let encoded = required(totp_secret, "SHOONYA_TOTP_SECRET")?;
    let totp_key = decode_totp_key(&encoded)?;
    Ok(Config {
        user_id: user_id.to_string(),
        client_id: client_id.to_string(),
        secret,
        password,
        totp_key,
    })
}

fn required(value: Option<Zeroizing<String>>, name: &str) -> Result<Zeroizing<String>, String> {
    value
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{name} is missing from .env."))
}

fn valid_identifier(value: &str, underscore: bool) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || (underscore && byte == b'_'))
}

fn decode_totp_key(value: &str) -> Result<Zeroizing<Vec<u8>>, String> {
    let mut uri_secret = None;
    let encoded = if value.to_ascii_lowercase().starts_with("otpauth://") {
        let query = value
            .split_once('?')
            .map(|(_, query)| query)
            .ok_or("TOTP URI has no secret parameter.")?;
        for (key, secret) in form_urlencoded::parse(query.as_bytes()) {
            if key == "secret"
                && uri_secret
                    .replace(Zeroizing::new(secret.into_owned()))
                    .is_some()
            {
                return Err("TOTP URI contains duplicate secrets.".to_owned());
            }
        }
        uri_secret
            .as_deref()
            .ok_or("TOTP URI has no secret parameter.")?
            .as_str()
    } else {
        value
    };
    let mut normalized = Zeroizing::new(String::with_capacity(encoded.len()));
    for c in encoded.chars() {
        if !c.is_ascii_whitespace() && c != '-' {
            normalized.push(c.to_ascii_uppercase());
        }
    }
    while normalized.ends_with('=') {
        normalized.pop();
    }
    if normalized.len() == 6 && normalized.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(
            "SHOONYA_TOTP_SECRET must be the permanent key, not a six-digit OTP.".to_owned(),
        );
    }
    if normalized.is_empty() || normalized.len() > 4096 {
        return Err("Invalid SHOONYA_TOTP_SECRET in .env.".to_owned());
    }
    let decoded = BASE32_NOPAD
        .decode(normalized.as_bytes())
        .map_err(|_| "Invalid Base32 SHOONYA_TOTP_SECRET in .env.")?;
    if decoded.is_empty() {
        return Err("Invalid SHOONYA_TOTP_SECRET in .env.".to_owned());
    }
    Ok(Zeroizing::new(decoded))
}

fn totp(key: &[u8], unix_seconds: u64) -> Result<String, String> {
    let mut mac = Hmac::<Sha1>::new_from_slice(key).map_err(|_| "Invalid TOTP key.")?;
    mac.update(&(unix_seconds / 30).to_be_bytes());
    let hash = mac.finalize().into_bytes();
    let offset = usize::from(hash[19] & 15);
    let value = u32::from_be_bytes([
        hash[offset],
        hash[offset + 1],
        hash[offset + 2],
        hash[offset + 3],
    ]);
    Ok(format!("{:06}", (value & 0x7fff_ffff) % 1_000_000))
}

fn quick_auth(
    agent: &ureq::Agent,
    endpoint: &str,
    config: &Config,
    factor: &str,
    kind: LoginKind,
) -> Result<String, String> {
    let password_hash = Zeroizing::new(format!("{:x}", Sha256::digest(config.password.as_bytes())));
    let app_key = format!(
        "{:x}",
        Sha256::digest(format!("{}|S3cur3!d", config.user_id).as_bytes())
    );
    let (source, vc) = match kind {
        LoginKind::Api => ("API", "NOREN_API"),
        LoginKind::Web => ("WEB", "NOREN_WEB"),
    };
    let mut payload = json!({
        "apkversion": "W2_20260827",
        "uid": config.user_id,
        "pwd": password_hash.as_str(),
        "factor2": factor,
        "appkey": app_key,
        "imei": uuid::Uuid::new_v4().to_string(),
        "addldivinf": "Mozilla/5.0",
        "source": source,
        "vc": vc,
    });
    if matches!(kind, LoginKind::Api) {
        payload["app_key"] = json!(config.client_id);
    }
    let body = Zeroizing::new(format!("jData={payload}"));
    let value = post_json(agent, endpoint, &body, "QuickAuth")?;
    if value.get("stat").and_then(Value::as_str) != Some("Ok") {
        return Err(
            "Shoonya QuickAuth rejected login; check credentials, TOTP and system clock."
                .to_owned(),
        );
    }
    value
        .get("susertoken")
        .and_then(Value::as_str)
        .filter(|token| {
            !token.is_empty()
                && token.len() <= 4096
                && token.bytes().all(|b| b.is_ascii_alphanumeric())
        })
        .map(str::to_owned)
        .ok_or_else(|| "Shoonya QuickAuth did not return a valid session token.".to_owned())
}

fn portal_secret(
    agent: &ureq::Agent,
    endpoints: &Endpoints<'_>,
    config: &Config,
    api_session: &str,
) -> Result<Zeroizing<String>, String> {
    let body = Zeroizing::new(format!(
        "jData={}&jKey={api_session}",
        json!({"app_key": config.client_id})
    ));
    let mut response = post(agent, endpoints.app_key, &body, "GetAppKeyData")?;
    if matches!(response.status().as_u16(), 401 | 403) {
        let factor = fresh_factor(&config.totp_key)?;
        let web_session = Zeroizing::new(quick_auth(
            agent,
            endpoints.web_auth,
            config,
            &factor,
            LoginKind::Web,
        )?);
        let body = Zeroizing::new(format!(
            "jData={}&jKey={}",
            json!({"app_key": config.client_id}),
            web_session.as_str()
        ));
        response = post(agent, endpoints.app_key, &body, "GetAppKeyData")?;
    }
    if !response.status().is_success() {
        return Err(broker_http_error(&mut response, "GetAppKeyData"));
    }
    let value = read_json(&mut response, "GetAppKeyData")?;
    if value
        .get("stat")
        .is_some_and(|stat| stat.as_str() != Some("Ok"))
    {
        return Err(
            "Shoonya did not return a saved API key. Verify it is saved in the portal.".to_owned(),
        );
    }
    if let Some(app_key) = value.get("app_key").and_then(Value::as_str)
        && app_key != config.client_id
    {
        return Err("Shoonya returned a different portal client ID.".to_owned());
    }
    let secret = value
        .get("sec_code")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|secret| {
            !secret.is_empty()
                && secret.len() <= 1024
                && secret
                    .bytes()
                    .all(|byte| (33..=126).contains(&byte) && byte != b'*')
        })
        .ok_or("Shoonya did not return a valid saved portal Secret Code.")?;
    Ok(Zeroizing::new(secret.to_owned()))
}

fn auth_code(
    agent: &ureq::Agent,
    endpoint: &str,
    client_id: &str,
    session: &str,
) -> Result<String, String> {
    let body = Zeroizing::new(format!(
        "jData={}&jKey={session}",
        json!({"app_key":client_id})
    ));
    let mut response = post(agent, endpoint, &body, "GetAuthCode")?;
    if !response.status().is_success() && !response.status().is_redirection() {
        return Err(broker_http_error(&mut response, "GetAuthCode"));
    }
    let code = if let Some(location) = response.headers().get("Location") {
        let location = location
            .to_str()
            .map_err(|_| "Invalid authorization redirect.")?;
        let query = location
            .split_once('?')
            .map(|(_, query)| query)
            .ok_or("Authorization redirect has no code.")?;
        let mut code = None;
        for (key, value) in form_urlencoded::parse(query.as_bytes()) {
            if key == "code" && code.replace(value.into_owned()).is_some() {
                return Err("Authorization redirect contains duplicate codes.".to_owned());
            }
        }
        code.ok_or("Authorization redirect has no code.")?
    } else {
        let value = read_json(&mut response, "GetAuthCode")?;
        value
            .get("code")
            .and_then(Value::as_str)
            .ok_or("Shoonya did not return an authorization code.")?
            .to_owned()
    };
    if code.is_empty()
        || code.len() > 1024
        || !code
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
    {
        return Err("Shoonya returned an invalid authorization code.".to_owned());
    }
    Ok(code)
}

fn exchange(
    agent: &ureq::Agent,
    endpoint: &str,
    config: &Config,
    secret: &str,
    code: &str,
) -> Result<String, ExchangeError> {
    let mut hasher = Sha256::new();
    hasher.update(config.client_id.as_bytes());
    hasher.update(secret.as_bytes());
    hasher.update(code.as_bytes());
    let checksum = format!("{:x}", hasher.finalize());
    let body = Zeroizing::new(format!(
        "jData={}",
        json!({"uid": config.user_id, "code": code, "checksum": checksum})
    ));
    let result = post_json(agent, endpoint, &body, "GenAcsTok").map_err(ExchangeError::Other)?;
    extract_token(&result, &config.user_id)
}

fn post(
    agent: &ureq::Agent,
    endpoint: &str,
    body: &str,
    stage: &str,
) -> Result<ureq::http::Response<ureq::Body>, String> {
    match agent
        .post(endpoint)
        .header("Content-Type", "text/plain")
        .send(body)
    {
        Ok(response) => Ok(response),
        Err(ureq::Error::StatusCode(code)) => Err(format!("Shoonya {stage} returned HTTP {code}.")),
        Err(_) => Err(format!(
            "Shoonya {stage} failed (network, TLS, or timeout)."
        )),
    }
}

fn read_json(
    response: &mut ureq::http::Response<ureq::Body>,
    stage: &str,
) -> Result<Value, String> {
    let body = Zeroizing::new(
        response
            .body_mut()
            .with_config()
            .limit(65_536)
            .read_to_string()
            .map_err(|_| format!("Cannot read Shoonya {stage} response."))?,
    );
    serde_json::from_str(&body).map_err(|_| format!("Invalid Shoonya {stage} response."))
}

fn post_json(
    agent: &ureq::Agent,
    endpoint: &str,
    body: &str,
    stage: &str,
) -> Result<Value, String> {
    let mut response = post(agent, endpoint, body, stage)?;
    if !response.status().is_success() {
        return Err(broker_http_error(&mut response, stage));
    }
    read_json(&mut response, stage)
}

fn broker_http_error(response: &mut ureq::http::Response<ureq::Body>, stage: &str) -> String {
    let status = response.status().as_u16();
    let detail = match read_json(response, stage) {
        Ok(body) => {
            let message = body.get("emsg").and_then(Value::as_str).unwrap_or("");
            let message = message.to_ascii_lowercase();
            if message.contains("session expired")
                || message.contains("invalid session")
                || message.contains("session_expired")
            {
                "session rejected or expired"
            } else if message.contains("invalid input") || message.contains("invalid_input") {
                "invalid input"
            } else {
                "unclassified broker rejection"
            }
        }
        Err(_) => "no readable JSON error detail",
    };
    format!("Shoonya {stage} returned HTTP {status} ({detail}).")
}

fn extract_token(result: &Value, user_id: &str) -> Result<String, ExchangeError> {
    if result
        .get("stat")
        .is_some_and(|stat| stat.as_str() != Some("Ok"))
    {
        let invalid_verifier = result
            .get("emsg")
            .and_then(Value::as_str)
            .is_some_and(|error| error.to_ascii_uppercase().contains("INVALID_VERIFIER"));
        return Err(if invalid_verifier {
            ExchangeError::InvalidVerifier
        } else {
            ExchangeError::Other("Shoonya rejected the token exchange.".to_owned())
        });
    }
    for field in ["USERID", "uid"] {
        if let Some(returned) = result.get(field).and_then(Value::as_str)
            && returned != user_id
        {
            return Err(ExchangeError::Other(
                "Shoonya returned a different user ID.".to_owned(),
            ));
        }
    }
    result
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .or_else(|| result.get("susertoken").and_then(Value::as_str))
        .filter(|token| {
            !token.is_empty()
                && token.len() <= 4096
                && token.bytes().all(|byte| (33..=126).contains(&byte))
        })
        .map(str::to_owned)
        .ok_or_else(|| {
            ExchangeError::Other("Shoonya did not return a valid access token.".to_owned())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread::{self, JoinHandle};

    fn mock_responses(responses: Vec<String>) -> (String, JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/api", listener.local_addr().unwrap());
        let worker = thread::spawn(move || {
            let mut requests = Vec::new();
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut input = Vec::new();
                let mut chunk = [0; 4096];
                let (header_end, length) = loop {
                    let count = stream.read(&mut chunk).unwrap();
                    assert_ne!(count, 0);
                    input.extend_from_slice(&chunk[..count]);
                    if let Some(position) = input.windows(4).position(|part| part == b"\r\n\r\n") {
                        let headers =
                            String::from_utf8_lossy(&input[..position]).to_ascii_lowercase();
                        let length: usize = headers
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length: "))
                            .unwrap()
                            .parse()
                            .unwrap();
                        break (position + 4, length);
                    }
                };
                while input.len() < header_end + length {
                    let count = stream.read(&mut chunk).unwrap();
                    assert_ne!(count, 0);
                    input.extend_from_slice(&chunk[..count]);
                }
                stream.write_all(response.as_bytes()).unwrap();
                requests.push(String::from_utf8(input).unwrap());
            }
            requests
        });
        (endpoint, worker)
    }

    fn mock_response(response: &str) -> (String, JoinHandle<Vec<String>>) {
        mock_responses(vec![response.to_owned()])
    }

    fn mock_agent() -> ureq::Agent {
        ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(5)))
            .build()
            .into()
    }

    fn config() -> Config {
        parse_config(b"SHOONYA_USER_ID=USER1\nSHOONYA_CLIENT_ID=USER1_U\nSHOONYA_SECRET_CODE=secret\nSHOONYA_PASSWORD=password\nSHOONYA_TOTP_SECRET=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ\n").unwrap()
    }

    #[test]
    fn totp_matches_rfc_6238_vector_and_accepts_uri() {
        let config = config();
        assert_eq!(totp(&config.totp_key, 59).unwrap(), "287082");
        assert_eq!(
            totp(
                &decode_totp_key("otpauth://totp/test?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ")
                    .unwrap(),
                59
            )
            .unwrap(),
            "287082"
        );
        assert!(decode_totp_key("123456").is_err());
        assert!(decode_totp_key("otpauth://totp/test?secret=A&secret=B").is_err());
    }

    #[test]
    fn config_rejects_missing_duplicate_and_masked_values_without_exposing_them() {
        assert!(
            !parse_config(b"SHOONYA_PASSWORD=secret\n")
                .err()
                .unwrap()
                .contains("secret")
        );
        assert!(parse_config(b"SHOONYA_USER_ID=A\nSHOONYA_USER_ID=B\n").is_err());
        assert!(parse_config(b"SHOONYA_USER_ID=A\nSHOONYA_CLIENT_ID=A_U\nSHOONYA_SECRET_CODE=***\nSHOONYA_PASSWORD=password\nSHOONYA_TOTP_SECRET=GEZDGNBV\n").is_err());
    }

    #[test]
    fn token_response_is_validated_without_echoing_secrets() {
        assert_eq!(
            extract_token(
                &json!({"stat":"Ok","uid":"USER1","access_token":"abc"}),
                "USER1"
            ),
            Ok("abc".to_owned())
        );
        assert_eq!(
            extract_token(&json!({"susertoken":"abc"}), "USER1"),
            Ok("abc".to_owned())
        );
        assert_eq!(
            extract_token(&json!({"access_token":"","susertoken":"abc"}), "USER1"),
            Ok("abc".to_owned())
        );
        for value in [
            json!({"stat":"Not_Ok","emsg":"INVALID_VERIFIER secret"}),
            json!({"uid":"OTHER","access_token":"secret"}),
            json!({"stat":"unexpected","access_token":"secret"}),
            json!({"access_token":"secret\n"}),
            json!({}),
        ] {
            assert!(
                !format!("{:?}", extract_token(&value, "USER1").unwrap_err()).contains("secret")
            );
        }
    }

    #[test]
    fn headless_exchange_uses_expected_payloads_and_no_redirect_following() {
        let agent = mock_agent();
        let config = config();
        let body = r#"{"stat":"Ok","susertoken":"ABC123"}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nSet-Cookie: broker_session=mock; Path=/; HttpOnly\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let (endpoint, worker) = mock_response(&response);
        assert_eq!(
            quick_auth(&agent, &endpoint, &config, "287082", LoginKind::Api).unwrap(),
            "ABC123"
        );
        let request = worker.join().unwrap().remove(0);
        let request_body = request.split_once("\r\n\r\n").unwrap().1;
        let payload: Value =
            serde_json::from_str(request_body.strip_prefix("jData=").unwrap()).unwrap();
        assert_eq!(payload["uid"], "USER1");
        assert_eq!(payload["app_key"], "USER1_U");
        assert_eq!(payload["factor2"], "287082");
        assert_ne!(payload["pwd"], "password");

        let response = "HTTP/1.1 302 Found\r\nLocation: http://localhost/callback?code=abcd-123\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let (endpoint, worker) = mock_response(response);
        assert_eq!(
            auth_code(&agent, &endpoint, "USER1_U", "ABC123").unwrap(),
            "abcd-123"
        );
        let request = worker.join().unwrap().remove(0);
        assert!(
            request
                .to_ascii_lowercase()
                .contains("cookie: broker_session=mock")
        );
        assert!(request.ends_with("&jKey=ABC123"));

        let body = r#"{"stat":"Ok","USERID":"USER1","access_token":"TOKEN"}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let (endpoint, worker) = mock_response(&response);
        assert_eq!(
            exchange(&agent, &endpoint, &config, "secret", "abcd-123").unwrap(),
            "TOKEN"
        );
        let request = worker.join().unwrap().remove(0);
        let request_body = request.split_once("\r\n\r\n").unwrap().1;
        let payload: Value =
            serde_json::from_str(request_body.strip_prefix("jData=").unwrap()).unwrap();
        assert_eq!(payload["uid"], "USER1");
        assert_eq!(payload["code"], "abcd-123");
        assert_eq!(
            payload["checksum"],
            "0f042a94c3a298984fe51e36597c456743842014e9f5867a10f66e85991c8093"
        );
    }

    #[test]
    fn broker_http_errors_report_a_safe_category_not_response_content() {
        let agent = mock_agent();
        let body = r#"{"stat":"Not_Ok","emsg":"Session Expired token-value"}"#;
        let response = format!(
            "HTTP/1.1 401 Unauthorized\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let (endpoint, worker) = mock_response(&response);
        let error = auth_code(&agent, &endpoint, "USER1_U", "ABC123").unwrap_err();
        worker.join().unwrap();
        assert!(error.contains("HTTP 401"));
        assert!(error.contains("session rejected or expired"));
        assert!(!error.contains("token-value"));
        assert!(!error.contains("ABC123"));

        // Verify remaining broker_http_error categories + parse_config_with_env precedence!
        for (raw_body, expected_cat) in [
            (
                r#"{"stat":"Not_Ok","emsg":"Invalid Input: secret"}"#,
                "invalid input",
            ),
            (
                r#"{"stat":"Not_Ok","emsg":"Other Error: secret"}"#,
                "unclassified broker rejection",
            ),
            ("plain text gateway error", "no readable JSON error detail"),
        ] {
            let resp = format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{raw_body}",
                raw_body.len()
            );
            let (ep, wk) = mock_response(&resp);
            let err = auth_code(&agent, &ep, "USER1_U", "ABC123").unwrap_err();
            wk.join().unwrap();
            assert!(err.contains(expected_cat), "{err}");
            assert!(!err.contains("secret"));
        }

        let overridden = parse_config_with_env(
            b"SHOONYA_USER_ID=FILEUSER\nSHOONYA_CLIENT_ID=FILEUSER_U\nSHOONYA_SECRET_CODE=file_sec\nSHOONYA_PASSWORD=file_pw\nSHOONYA_TOTP_SECRET=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ\n",
            |k| match k {
                "SHOONYA_USER_ID" => Some("ENVUSER1".to_owned()),
                "SHOONYA_CLIENT_ID" => Some("ENVUSER1_U".to_owned()),
                _ => None,
            },
        )
        .unwrap();
        assert_eq!(overridden.user_id, "ENVUSER1");
        assert!(
            parse_config_with_env(b"", |k| {
                (k == "SHOONYA_USER_ID").then(|| "   ".to_owned())
            })
            .is_err()
        );
        assert!(login_session(Path::new("nonexistent-shoonya-config.env")).is_err());
        let otp = fresh_factor(b"12345678901234567890").unwrap();
        assert_eq!(otp.len(), 6);
    }

    fn ok_json(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn redirect(code: &str) -> String {
        format!(
            "HTTP/1.1 302 Found\r\nLocation: http://localhost/callback?code={code}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
    }

    #[test]
    fn verifier_recovery_uses_saved_portal_key_and_a_fresh_authorization_code() {
        let (quick_url, quick_requests) =
            mock_responses(vec![ok_json(r#"{"stat":"Ok","susertoken":"APITOKEN"}"#)]);
        let (web_url, web_requests) =
            mock_responses(vec![ok_json(r#"{"stat":"Ok","susertoken":"WEBTOKEN"}"#)]);
        let (portal_url, portal_requests) = mock_responses(vec![
            "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .to_owned(),
            ok_json(r#"{"stat":"Ok","app_key":"USER1_U","sec_code":"portal-secret"}"#),
        ]);
        let (code_url, code_requests) = mock_responses(vec![redirect("c1"), redirect("c2")]);
        let (token_url, token_requests) = mock_responses(vec![
            ok_json(r#"{"stat":"Not_Ok","emsg":"INVALID_VERIFIER"}"#),
            ok_json(r#"{"stat":"Ok","access_token":"FINAL","USERID":"USER1"}"#),
        ]);
        let endpoints = Endpoints {
            quick_auth: &quick_url,
            web_auth: &web_url,
            app_key: &portal_url,
            auth_code: &code_url,
            token: &token_url,
        };
        assert_eq!(
            authenticate(&mock_agent(), &endpoints, &config()).unwrap(),
            "FINAL"
        );
        quick_requests.join().unwrap();
        let web = web_requests.join().unwrap().remove(0);
        let web_payload: Value = serde_json::from_str(
            web.split_once("\r\n\r\n")
                .unwrap()
                .1
                .strip_prefix("jData=")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(web_payload["source"], "WEB");
        assert_eq!(web_payload["vc"], "NOREN_WEB");
        assert!(web_payload.get("app_key").is_none());
        let portal = portal_requests.join().unwrap();
        assert!(portal[0].ends_with("&jKey=APITOKEN"));
        assert!(portal[1].ends_with("&jKey=WEBTOKEN"));
        assert_eq!(code_requests.join().unwrap().len(), 2);
        let tokens = token_requests.join().unwrap();
        for (request, code, checksum) in [
            (
                &tokens[0],
                "c1",
                "d94b98dd0a2139291cf45649a707bcc1e9d78dcf0937aeab0be7d96c01d33ef1",
            ),
            (
                &tokens[1],
                "c2",
                "7a485e70ef2738d38740757a44814a20dac831bff65cd5d655775295446a0cf9",
            ),
        ] {
            let body = request.split_once("\r\n\r\n").unwrap().1;
            let payload: Value =
                serde_json::from_str(body.strip_prefix("jData=").unwrap()).unwrap();
            assert_eq!(payload["code"], code);
            assert_eq!(payload["checksum"], checksum);
        }
    }

    #[test]
    fn portal_recovery_rejects_mismatched_client_and_missing_key() {
        for body in [
            r#"{"stat":"Ok","app_key":"OTHER","sec_code":"private-secret"}"#,
            r#"{"stat":"Not_Ok","emsg":"private-secret"}"#,
            r#"{"stat":"Ok","app_key":"USER1_U","sec_code":""}"#,
        ] {
            let (portal_url, requests) = mock_responses(vec![ok_json(body)]);
            let endpoints = Endpoints {
                quick_auth: "",
                web_auth: "",
                app_key: &portal_url,
                auth_code: "",
                token: "",
            };
            let error =
                portal_secret(&mock_agent(), &endpoints, &config(), "APITOKEN").unwrap_err();
            requests.join().unwrap();
            assert!(!error.contains("private-secret"));
        }

        // Verify read_config size/missing guards, login_session error path, and verifier recovery failure arms.
        let missing = std::env::temp_dir().join("nsp-missing-oauth-cfg.env");
        assert!(login_session(&missing).is_err());

        let big_path = std::env::temp_dir().join(format!(
            "nsp-big-oauth-{}.env",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::write(&big_path, vec![b'A'; 70_000]).unwrap();
        assert!(read_config(&big_path).is_err());

        let valid_env = "SHOONYA_USER_ID=USER1\nSHOONYA_CLIENT_ID=USER1_U\nSHOONYA_SECRET_CODE=wrong-secret\nSHOONYA_PASSWORD=password123\nSHOONYA_TOTP_SECRET=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ\n";
        std::fs::write(&big_path, valid_env).unwrap();
        assert!(read_config(&big_path).is_ok());
        let _ = std::fs::remove_file(&big_path);

        // Case A: portal secret matches .env -> still rejected without retrying token exchange!
        let (q1, qr1) = mock_responses(vec![ok_json(r#"{"stat":"Ok","susertoken":"APITOKEN"}"#)]);
        let (p1, pr1) = mock_responses(vec![ok_json(
            r#"{"stat":"Ok","app_key":"USER1_U","sec_code":"secret"}"#,
        )]);
        let (c1, cr1) = mock_responses(vec![redirect("c1")]);
        let (t1, tr1) = mock_responses(vec![ok_json(
            r#"{"stat":"Not_Ok","emsg":"INVALID_VERIFIER"}"#,
        )]);
        let ep1 = Endpoints {
            quick_auth: &q1,
            web_auth: "",
            app_key: &p1,
            auth_code: &c1,
            token: &t1,
        };
        assert!(
            authenticate(&mock_agent(), &ep1, &config())
                .unwrap_err()
                .contains("matches .env")
        );
        qr1.join().unwrap();
        pr1.join().unwrap();
        cr1.join().unwrap();
        tr1.join().unwrap();

        // Case B: portal secret differs, but second token exchange also returns INVALID_VERIFIER!
        let (q2, qr2) = mock_responses(vec![ok_json(r#"{"stat":"Ok","susertoken":"APITOKEN"}"#)]);
        let (p2, pr2) = mock_responses(vec![ok_json(
            r#"{"stat":"Ok","app_key":"USER1_U","sec_code":"portal-secret"}"#,
        )]);
        let (c2, cr2) = mock_responses(vec![redirect("c1"), redirect("c2")]);
        let (t2, tr2) = mock_responses(vec![
            ok_json(r#"{"stat":"Not_Ok","emsg":"INVALID_VERIFIER"}"#),
            ok_json(r#"{"stat":"Not_Ok","emsg":"INVALID_VERIFIER"}"#),
        ]);
        let ep2 = Endpoints {
            quick_auth: &q2,
            web_auth: "",
            app_key: &p2,
            auth_code: &c2,
            token: &t2,
        };
        assert!(
            authenticate(&mock_agent(), &ep2, &config())
                .unwrap_err()
                .contains("saved portal key")
        );
        qr2.join().unwrap();
        pr2.join().unwrap();
        cr2.join().unwrap();
        tr2.join().unwrap();

        // Case C: env_lookup override, 6-digit OTP guard, JSON/duplicate/invalid auth_code, QuickAuth Not_Ok, and transport error!
        let cfg_from_env = parse_config_with_env(b"", |k| match k {
            "SHOONYA_USER_ID" => Some("USER1".to_owned()),
            "SHOONYA_CLIENT_ID" => Some("USER1_U".to_owned()),
            "SHOONYA_SECRET_CODE" => Some("secret".to_owned()),
            "SHOONYA_PASSWORD" => Some("pass".to_owned()),
            "SHOONYA_TOTP_SECRET" => Some("JBSWY3DPEHPK3PXP".to_owned()),
            _ => None,
        })
        .unwrap();
        assert_eq!(cfg_from_env.user_id, "USER1");
        assert!(
            decode_totp_key("123456")
                .unwrap_err()
                .contains("six-digit OTP")
        );
        assert!(decode_totp_key("otpauth://totp/x?secret====").is_err());

        let (q_bad, qr_bad) = mock_responses(vec![ok_json(r#"{"stat":"Not_Ok"}"#)]);
        assert!(
            quick_auth(&mock_agent(), &q_bad, &config(), "123456", LoginKind::Api)
                .unwrap_err()
                .contains("QuickAuth rejected")
        );
        qr_bad.join().unwrap();

        let (ac_url, ac_srv) = mock_responses(vec![
            ok_json(r#"{"code":"json_code_123"}"#),
            "HTTP/1.1 302 Found\r\nLocation: https://example.invalid/cb?code=a&code=b\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
            ok_json(r#"{"code":"bad code!"}"#),
        ]);
        assert_eq!(
            auth_code(&mock_agent(), &ac_url, "USER1_U", "SESS").unwrap(),
            "json_code_123"
        );
        assert!(
            auth_code(&mock_agent(), &ac_url, "USER1_U", "SESS")
                .unwrap_err()
                .contains("duplicate")
        );
        assert!(
            auth_code(&mock_agent(), &ac_url, "USER1_U", "SESS")
                .unwrap_err()
                .contains("invalid authorization code")
        );
        ac_srv.join().unwrap();

        assert!(
            post(
                &mock_agent(),
                "http://127.0.0.1:1/QuickAuth",
                "jData={}",
                "QuickAuth"
            )
            .unwrap_err()
            .contains("failed (network, TLS, or timeout)")
        );
    }
}
