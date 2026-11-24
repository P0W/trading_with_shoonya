use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use zeroize::Zeroizing;

const QUOTE_URL: &str = "https://api.shoonya.com/NorenWClientAPI/GetQuotes";
const MAX_SESSION_BYTES: u64 = 65_536;

pub fn check(session_path: &Path, user_id: &str, instrument: &str) -> Result<String, String> {
    let token = read_token(session_path)?;
    check_authenticated(&agent(), &token, user_id, instrument)
}

pub fn agent() -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .https_only(true)
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::NativeTls)
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .proxy(None)
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(15)))
        .build();
    config.into()
}

pub fn check_authenticated(
    agent: &ureq::Agent,
    token: &str,
    user_id: &str,
    instrument: &str,
) -> Result<String, String> {
    if user_id.is_empty()
        || user_id.len() > 64
        || !user_id.bytes().all(|c| c.is_ascii_alphanumeric())
    {
        return Err("SHOONYA_USER_ID must contain only letters and digits.".to_owned());
    }
    if instrument.is_empty()
        || instrument.len() > 20
        || !instrument.bytes().all(|c| c.is_ascii_digit())
    {
        return Err("NIFTY_QUOTE_TOKEN must be a numeric NSE instrument token.".to_owned());
    }
    quote(agent, QUOTE_URL, token, user_id, instrument)
}

fn read_token(path: &Path) -> Result<Zeroizing<String>, String> {
    let file = File::open(path).map_err(|_| "Cannot open the Shoonya session file.".to_owned())?;
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(MAX_SESSION_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read the Shoonya session file.".to_owned())?;
    if bytes.len() as u64 > MAX_SESSION_BYTES {
        return Err("Shoonya session file is too large.".to_owned());
    }
    let session: Value = serde_json::from_slice(&bytes)
        .map_err(|_| "Shoonya session file is not valid JSON.".to_owned())?;
    let token = session
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 4096
                && value.bytes().all(|byte| (33..=126).contains(&byte))
        })
        .ok_or("Shoonya session file does not contain a valid access token.")?;
    Ok(Zeroizing::new(token.to_owned()))
}

fn quote(
    agent: &ureq::Agent,
    endpoint: &str,
    access_token: &str,
    user_id: &str,
    instrument: &str,
) -> Result<String, String> {
    let payload = json!({"uid": user_id, "exch": "NSE", "token": instrument});
    let bearer = Zeroizing::new(format!("Bearer {access_token}"));
    let mut response = match agent
        .post(endpoint)
        .header("Content-Type", "text/plain")
        .header("Authorization", bearer.as_str())
        .send(format!("jData={payload}"))
    {
        Ok(response) => response,
        Err(ureq::Error::StatusCode(code)) => {
            return Err(format!("Shoonya quote request returned HTTP {code}."));
        }
        Err(_) => return Err("Shoonya quote request failed (network, TLS, or timeout).".to_owned()),
    };
    if !response.status().is_success() {
        return Err(format!(
            "Shoonya quote request returned HTTP {}.",
            response.status().as_u16()
        ));
    }
    let body = response
        .body_mut()
        .with_config()
        .limit(65_536)
        .read_to_string()
        .map_err(|_| "Cannot read Shoonya quote response.".to_owned())?;
    let value: Value =
        serde_json::from_str(&body).map_err(|_| "Invalid Shoonya quote response.".to_owned())?;
    if value.get("stat").and_then(Value::as_str) != Some("Ok") {
        return Err(
            "Shoonya rejected the quote request; check session, user ID and token.".to_owned(),
        );
    }
    if value.get("token").and_then(Value::as_str) != Some(instrument)
        || value.get("exch").and_then(Value::as_str) != Some("NSE")
    {
        return Err("Shoonya quote response did not match the requested instrument.".to_owned());
    }
    let price = value
        .get("lp")
        .and_then(Value::as_str)
        .filter(|price| valid_price(price))
        .ok_or("Shoonya quote response has no valid last price.")?;
    Ok(price.to_owned())
}

fn valid_price(price: &str) -> bool {
    let Some((whole, fraction)) = price.split_once('.') else {
        return !price.is_empty()
            && price.len() <= 12
            && price.bytes().all(|byte| byte.is_ascii_digit())
            && price.bytes().any(|byte| byte != b'0');
    };
    !whole.is_empty()
        && whole.len() <= 12
        && !fraction.is_empty()
        && fraction.len() <= 2
        && whole
            .bytes()
            .chain(fraction.bytes())
            .all(|byte| byte.is_ascii_digit())
        && whole
            .bytes()
            .chain(fraction.bytes())
            .any(|byte| byte != b'0')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;

    static SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn test_request(response: &str) -> Result<String, String> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/quote", listener.local_addr().unwrap());
        let response = response.to_owned();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut input = Vec::new();
            let mut chunk = [0; 4096];
            let (header_end, content_length) = loop {
                let size = stream.read(&mut chunk).unwrap();
                assert_ne!(size, 0);
                input.extend_from_slice(&chunk[..size]);
                if let Some(position) = input.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&input[..position]).to_ascii_lowercase();
                    let length: usize = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .unwrap()
                        .parse()
                        .unwrap();
                    break (position + 4, length);
                }
            };
            while input.len() < header_end + content_length {
                let size = stream.read(&mut chunk).unwrap();
                assert_ne!(size, 0);
                input.extend_from_slice(&chunk[..size]);
            }
            let request = String::from_utf8_lossy(&input).to_ascii_lowercase();
            assert!(request.contains("authorization: bearer test-token"));
            assert!(request.contains(r#"jdata={"exch":"nse","token":"26000","uid":"user1"}"#));
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(),
                response
            )
            .unwrap();
        });
        let config = ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .timeout_global(Some(Duration::from_secs(5)))
            .build();
        let result = quote(&config.into(), &endpoint, "test-token", "USER1", "26000");
        server.join().unwrap();
        result
    }

    #[test]
    fn quote_accepts_matching_success_and_rejects_bad_or_untrusted_responses() {
        assert_eq!(
            test_request(r#"{"stat":"Ok","exch":"NSE","token":"26000","lp":"25000.25"}"#),
            Ok("25000.25".to_owned())
        );
        for response in [
            r#"{"stat":"Not_Ok","emsg":"test-token"}"#,
            r#"{"stat":"Ok","exch":"NSE","token":"wrong","lp":"25000.25"}"#,
            r#"{"stat":"Ok","exch":"NFO","token":"26000","lp":"25000.25"}"#,
            r#"{"stat":"Ok","exch":"NSE","token":"26000","lp":"0.00"}"#,
            "not json",
        ] {
            let error = test_request(response).unwrap_err();
            assert!(!error.contains("test-token"));
        }
    }

    #[test]
    fn price_validation_rejects_non_decimal_and_zero() {
        for price in ["", ".1", "1.", "-1", "NaN", "1.234", "0.00", "1\n"] {
            assert!(!valid_price(price), "{price:?}");
        }
        for price in ["1", "1.00", "0.01"] {
            assert!(valid_price(price), "{price:?}");
        }
    }

    #[test]
    fn session_input_is_bounded_and_errors_do_not_expose_secrets() {
        let path = std::env::temp_dir().join(format!(
            "nifty-connection-test-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        assert!(read_token(&path).is_err());
        for contents in [
            "not json",
            r#"{"access_token":""}"#,
            r#"{"access_token":"secret\nvalue"}"#,
            r#"{"access_token":123}"#,
        ] {
            std::fs::write(&path, contents).unwrap();
            let error = read_token(&path).unwrap_err();
            assert!(!error.contains("secret"));
        }
        std::fs::write(&path, vec![b'x'; MAX_SESSION_BYTES as usize + 1]).unwrap();
        assert!(read_token(&path).unwrap_err().contains("too large"));
        std::fs::write(&path, r#"{"access_token":"test-token"}"#).unwrap();
        assert_eq!(read_token(&path).unwrap().as_str(), "test-token");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn invalid_connection_settings_are_rejected_before_network_access() {
        let agent = agent();
        assert!(check_authenticated(&agent, "token", "", "26000").is_err());
        assert!(check_authenticated(&agent, "token", "USER1", "../26000").is_err());
        assert!(check(Path::new("nonexistent-session"), "USER1", "26000").is_err());
    }
}
