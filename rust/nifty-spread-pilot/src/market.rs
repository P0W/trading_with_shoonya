use std::collections::{BTreeSet, HashSet};
use std::io::{Cursor, Read};

use time::{Date, Month, OffsetDateTime, Time, UtcOffset};

use crate::OptionKind;
use crate::broker::{ExpiryChainContracts, OptionContract};
use crate::connection;

pub const DEFAULT_MASTER_URL: &str = "https://api.shoonya.com/NFO_symbols.txt.zip";
const MAX_COMPRESSED: u64 = 16 * 1024 * 1024;
const MAX_UNCOMPRESSED: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedMaster {
    pub nearest: ExpiryChainContracts,
    pub all_unexpired: Vec<OptionContract>,
    /// Retains today's expiring contracts even after 15:30 IST so post-close
    /// reconciliation of cash-settling positions never fails contract lookup.
    pub all_active_or_today: Vec<OptionContract>,
    pub nearest_lots: BTreeSet<u32>,
}

pub fn ist_now() -> Result<OffsetDateTime, String> {
    let ist = UtcOffset::from_hms(5, 30, 0).map_err(|_| "Invalid IST offset.")?;
    Ok(OffsetDateTime::now_utc().to_offset(ist))
}

pub fn download_and_parse_master(
    agent: &ureq::Agent,
    master_url: &str,
    underlying: &str,
    now_ist: OffsetDateTime,
) -> Result<ParsedMaster, String> {
    let mut response = agent
        .get(master_url)
        .call()
        .map_err(|_| "Cannot download Shoonya NFO symbol master (network, TLS, or timeout).")?;
    if !response.status().is_success() {
        return Err(format!(
            "Shoonya NFO symbol master returned HTTP {}.",
            response.status().as_u16()
        ));
    }
    let archive = response
        .body_mut()
        .with_config()
        .limit(MAX_COMPRESSED)
        .read_to_vec()
        .map_err(|_| "Cannot read Shoonya NFO symbol master.")?;
    parse_archive_for_underlying(&archive, underlying, now_ist.date(), now_ist.time())
}

pub fn check_contracts() -> Result<String, String> {
    let agent = connection::agent();
    let now = ist_now()?;
    let cfg_path = crate::oauth::default_config_path();
    let url = crate::engine::resolve_config_var(&cfg_path, "NIFTY_NFO_MASTER_URL")
        .ok()
        .flatten()
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_MASTER_URL.to_owned());
    check_contracts_from(&agent, &url, now)
}

pub fn check_contracts_from(
    agent: &ureq::Agent,
    url: &str,
    now_ist: OffsetDateTime,
) -> Result<String, String> {
    let parsed = download_and_parse_master(agent, url, "NIFTY", now_ist)?;
    let calls = parsed
        .nearest
        .contracts
        .iter()
        .filter(|c| c.option_kind == OptionKind::Call)
        .count();
    let puts = parsed
        .nearest
        .contracts
        .iter()
        .filter(|c| c.option_kind == OptionKind::Put)
        .count();
    let lots = parsed
        .nearest_lots
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    Ok(format!(
        "NFO master: next NIFTY option expiry {}; calls {}; puts {}; lot size(s) {}.",
        parsed.nearest.expiry, calls, puts, lots
    ))
}

pub fn parse_archive_for_underlying(
    bytes: &[u8],
    underlying: &str,
    today: Date,
    now: Time,
) -> Result<ParsedMaster, String> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| "Invalid NFO symbol archive.")?;
    if archive.len() != 1 {
        return Err("NFO symbol archive has unexpected entries.".to_owned());
    }
    let entry = archive
        .by_name("NFO_symbols.txt")
        .map_err(|_| "NFO symbol archive is missing NFO_symbols.txt.")?;
    if entry.size() > MAX_UNCOMPRESSED {
        return Err("NFO symbol master is too large.".to_owned());
    }
    let mut content = Vec::new();
    entry
        .take(MAX_UNCOMPRESSED + 1)
        .read_to_end(&mut content)
        .map_err(|_| "Cannot decompress NFO symbol master.")?;
    if content.len() as u64 > MAX_UNCOMPRESSED {
        return Err("NFO symbol master is too large.".to_owned());
    }
    parse_master_for_underlying(&content, underlying, today, now)
}

pub fn parse_master_for_underlying(
    content: &[u8],
    underlying: &str,
    today: Date,
    now: Time,
) -> Result<ParsedMaster, String> {
    let mut csv = csv::ReaderBuilder::new().from_reader(content);
    let headers = csv
        .headers()
        .map_err(|_| "Cannot read NFO symbol master header.")?
        .clone();
    let column = |name: &str| -> Result<usize, String> {
        let mut positions = headers
            .iter()
            .enumerate()
            .filter_map(|(index, item)| (item == name).then_some(index));
        let index = positions
            .next()
            .ok_or_else(|| format!("NFO symbol master is missing {name}."))?;
        if positions.next().is_some() {
            return Err(format!("NFO symbol master has duplicate {name}."));
        }
        Ok(index)
    };
    let exchange = column("Exchange")?;
    let token = column("Token")?;
    let lot = column("LotSize")?;
    let symbol = column("Symbol")?;
    let trading_symbol = column("TradingSymbol")?;
    let expiry = column("Expiry")?;
    let instrument = column("Instrument")?;
    let kind = column("OptionType")?;
    let strike = column("StrikePrice")?;
    let tick = column("TickSize")?;
    let cutoff = Time::from_hms(15, 30, 0).map_err(|_| "Invalid expiry cutoff.")?;

    let mut nearest_expiry: Option<Date> = None;
    let mut all_unexpired = Vec::new();
    let mut all_active_or_today = Vec::new();
    let mut tokens = HashSet::new();
    let mut identities = HashSet::new();
    let mut trading_symbols = HashSet::new();

    for row in csv.records() {
        let row = row.map_err(|_| "Invalid NFO symbol master row.")?;
        let sym = row.get(symbol).unwrap_or("");
        let inst = row.get(instrument).unwrap_or("");
        if sym != underlying || !matches!(inst, "OPTIDX" | "OPTSTK") {
            continue;
        }
        if row.get(exchange) != Some("NFO") {
            return Err("Option contract has an unexpected exchange.".to_owned());
        }
        let token_value = row
            .get(token)
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .ok_or("Option contract has an invalid token.")?;
        let lot_value = row
            .get(lot)
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|value| *value > 0)
            .ok_or("Option contract has an invalid lot size.")?;
        let trading_value = row
            .get(trading_symbol)
            .filter(|value| {
                !value.is_empty()
                    && value.len() <= 100
                    && value.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'&' | b'_')
                    })
            })
            .ok_or("Option contract has an invalid trading symbol.")?;
        let expiry_value = parse_expiry(row.get(expiry).ok_or("Missing option expiry.")?)?;
        let option_kind = match row.get(kind) {
            Some("CE") => OptionKind::Call,
            Some("PE") => OptionKind::Put,
            _ => return Err("Option contract has an invalid option type.".to_owned()),
        };
        let strike_value = row
            .get(strike)
            .and_then(parse_paise)
            .filter(|value| *value > 0)
            .ok_or("Option contract has an invalid strike.")?;
        let tick_value = row
            .get(tick)
            .and_then(parse_paise)
            .filter(|value| *value > 0)
            .ok_or("Option contract has an invalid tick size.")?;
        if !tokens.insert(token_value)
            || !identities.insert((expiry_value, option_kind == OptionKind::Call, strike_value))
            || !trading_symbols.insert(trading_value.to_owned())
        {
            return Err("NFO symbol master contains duplicate option contracts.".to_owned());
        }
        if expiry_value < today {
            continue;
        }
        let contract = OptionContract {
            exchange: "NFO".to_owned(),
            token: token_value.to_string(),
            trading_symbol: trading_value.to_owned(),
            underlying: underlying.to_owned(),
            expiry: expiry_value,
            option_kind,
            strike_paise: strike_value,
            lot_size: lot_value,
            tick_paise: tick_value,
        };
        all_active_or_today.push(contract.clone());
        if expiry_value == today && now >= cutoff {
            continue;
        }
        if nearest_expiry.is_none_or(|cur| expiry_value < cur) {
            nearest_expiry = Some(expiry_value);
        }
        all_unexpired.push(contract);
    }

    let expiry =
        nearest_expiry.ok_or("NFO master contains no unexpired option contracts for symbol.")?;
    all_unexpired.sort_by_key(|c| (c.expiry, c.strike_paise, c.option_kind == OptionKind::Put));
    all_active_or_today
        .sort_by_key(|c| (c.expiry, c.strike_paise, c.option_kind == OptionKind::Put));

    let mut nearest_contracts = Vec::new();
    let mut nearest_lots = BTreeSet::new();
    let mut nearest_ticks = BTreeSet::new();
    let mut calls = 0_usize;
    let mut puts = 0_usize;
    for contract in &all_unexpired {
        if contract.expiry == expiry {
            match contract.option_kind {
                OptionKind::Call => calls += 1,
                OptionKind::Put => puts += 1,
            }
            nearest_lots.insert(contract.lot_size);
            nearest_ticks.insert(contract.tick_paise);
            nearest_contracts.push(contract.clone());
        }
    }
    if calls == 0 || puts == 0 {
        return Err("Nearest option expiry is missing calls or puts.".to_owned());
    }
    if nearest_lots.len() != 1 || nearest_ticks.len() != 1 {
        return Err("Nearest option expiry has inconsistent lot or tick sizes.".to_owned());
    }

    Ok(ParsedMaster {
        nearest: ExpiryChainContracts {
            expiry,
            lot_size: *nearest_lots.iter().next().unwrap(),
            tick_paise: *nearest_ticks.iter().next().unwrap(),
            contracts: nearest_contracts,
        },
        all_unexpired,
        all_active_or_today,
        nearest_lots,
    })
}

fn parse_expiry(value: &str) -> Result<Date, String> {
    let mut parts = value.split('-');
    let day = parts
        .next()
        .and_then(|part| part.parse::<u8>().ok())
        .ok_or("Invalid NIFTY expiry date.")?;
    let month = match parts.next().unwrap_or("").to_ascii_uppercase().as_str() {
        "JAN" => Month::January,
        "FEB" => Month::February,
        "MAR" => Month::March,
        "APR" => Month::April,
        "MAY" => Month::May,
        "JUN" => Month::June,
        "JUL" => Month::July,
        "AUG" => Month::August,
        "SEP" => Month::September,
        "OCT" => Month::October,
        "NOV" => Month::November,
        "DEC" => Month::December,
        _ => return Err("Invalid NIFTY expiry month.".to_owned()),
    };
    let year = parts
        .next()
        .and_then(|part| part.parse::<i32>().ok())
        .ok_or("Invalid NIFTY expiry year.")?;
    if parts.next().is_some() || !(2000..=9999).contains(&year) {
        return Err("Invalid NIFTY expiry date.".to_owned());
    }
    Date::from_calendar_date(year, month, day).map_err(|_| "Invalid NIFTY expiry date.".to_owned())
}

fn parse_paise(value: &str) -> Option<i64> {
    let (whole, fractional) = value.split_once('.').unwrap_or((value, ""));
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || fractional.len() > 2
        || !fractional.bytes().all(|b| b.is_ascii_digit())
        || (value.contains('.') && fractional.is_empty())
    {
        return None;
    }
    let whole = whole.parse::<i64>().ok()?.checked_mul(100)?;
    let fraction = match fractional.len() {
        0 => 0,
        1 => fractional.parse::<i64>().ok()?.checked_mul(10)?,
        2 => fractional.parse::<i64>().ok()?,
        _ => return None,
    };
    whole.checked_add(fraction)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const HEADER: &str = "Exchange,Token,LotSize,Symbol,TradingSymbol,Expiry,Instrument,OptionType,StrikePrice,TickSize,\n";

    #[test]
    fn nearest_expiry_skips_expired_contracts_after_ist_cutoff() {
        let data = format!(
            "{HEADER}NFO,1,65,NIFTY,NIFTYC1,23-NOV-2026,OPTIDX,CE,25000,0.05,\n\
             NFO,2,65,NIFTY,NIFTYP1,23-NOV-2026,OPTIDX,PE,25000,0.05,\n\
             NFO,3,75,NIFTY,NIFTYC2,01-DEC-2026,OPTIDX,CE,25000,0.05,\n\
             NFO,4,75,NIFTY,NIFTYP2,01-DEC-2026,OPTIDX,PE,25000,0.05,\n"
        );
        let day = Date::from_calendar_date(2026, Month::November, 23).unwrap();
        let before = parse_master_for_underlying(
            data.as_bytes(),
            "NIFTY",
            day,
            Time::from_hms(15, 29, 59).unwrap(),
        )
        .unwrap();
        assert_eq!(before.nearest.expiry, day);
        assert_eq!(before.nearest.contracts.len(), 2);
        assert_eq!(before.all_unexpired.len(), 4);
        let after = parse_master_for_underlying(
            data.as_bytes(),
            "NIFTY",
            day,
            Time::from_hms(15, 30, 0).unwrap(),
        )
        .unwrap();
        assert_eq!(
            after.nearest.expiry,
            Date::from_calendar_date(2026, Month::December, 1).unwrap()
        );
        assert_eq!(after.nearest_lots.into_iter().collect::<Vec<_>>(), vec![75]);
    }

    #[test]
    fn rejects_duplicates_missing_legs_and_invalid_values() {
        let day = Date::from_calendar_date(2026, Month::November, 23).unwrap();
        let now = Time::MIDNIGHT;
        let call = "NFO,1,65,NIFTY,NIFTYC1,01-DEC-2026,OPTIDX,CE,25000,0.05,\n";
        for data in [
            format!("{HEADER}{call}"),
            format!("{HEADER}{call}{call}"),
            format!("{HEADER}{call}NFO,2,65,NIFTY,NIFTYP1,01-DEC-2026,OPTIDX,PE,NaN,0.05,\n"),
            format!("{HEADER}{call}NFO,2,75,NIFTY,NIFTYP1,01-DEC-2026,OPTIDX,PE,25000,0.05,\n"),
            "Exchange,Symbol\nNFO,NIFTY\n".to_owned(),
        ] {
            assert!(parse_master_for_underlying(data.as_bytes(), "NIFTY", day, now).is_err());
        }
        assert_eq!(parse_paise("25000.05"), Some(2_500_005));
        for invalid in ["NaN", "-1", "1.001", "1.", "", "999999999999999999"] {
            assert_eq!(parse_paise(invalid), None);
        }
        assert!(parse_expiry("29-FEB-2025").is_err());
    }

    #[test]
    fn archive_requires_expected_entry_and_parses_its_contracts() {
        let content = format!(
            "{HEADER}NFO,1,65,NIFTY,NIFTYC1,01-DEC-2026,OPTIDX,CE,25000,0.05,\n\
             NFO,2,65,NIFTY,NIFTYP1,01-DEC-2026,OPTIDX,PE,25000,0.05,\n"
        );
        let make_archive = |name| {
            let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(content.as_bytes()).unwrap();
            writer.finish().unwrap().into_inner()
        };
        let day = Date::from_calendar_date(2026, Month::November, 23).unwrap();
        let result = parse_archive_for_underlying(
            &make_archive("NFO_symbols.txt"),
            "NIFTY",
            day,
            Time::MIDNIGHT,
        )
        .unwrap();
        assert_eq!(result.nearest.contracts.len(), 2);
        assert!(
            parse_archive_for_underlying(
                &make_archive("unexpected.txt"),
                "NIFTY",
                day,
                Time::MIDNIGHT
            )
            .is_err()
        );
        assert!(parse_archive_for_underlying(b"not zip", "NIFTY", day, Time::MIDNIGHT).is_err());

        // Verify HTTP download + check_contracts_from + all 12 calendar months.
        for m in [
            "01-JAN-2027",
            "01-FEB-2027",
            "01-MAR-2027",
            "01-APR-2027",
            "01-MAY-2027",
            "01-JUN-2027",
            "01-JUL-2027",
            "01-AUG-2027",
            "01-SEP-2027",
            "01-OCT-2027",
            "01-NOV-2027",
            "01-DEC-2027",
        ] {
            assert!(parse_expiry(m).is_ok());
        }
        assert!(ist_now().is_ok());
        let zip_bytes = make_archive("NFO_symbols.txt");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/NFO.zip", listener.local_addr().unwrap());
        let srv = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0_u8; 1024];
            let _ = std::io::Read::read(&mut stream, &mut buf);
            let hdr = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                zip_bytes.len()
            );
            stream.write_all(hdr.as_bytes()).unwrap();
            stream.write_all(&zip_bytes).unwrap();
        });
        // Production agent enforces HTTPS-only, rejecting plain HTTP URLs.
        let now_ist = OffsetDateTime::from_unix_timestamp(1_795_400_000)
            .unwrap()
            .replace_date(day)
            .replace_time(Time::MIDNIGHT);
        assert!(check_contracts_from(&connection::agent(), &url, now_ist).is_err());

        let test_agent: ureq::Agent = ureq::Agent::config_builder().build().into();
        let msg = check_contracts_from(&test_agent, &url, now_ist).unwrap();
        assert!(msg.contains("calls 1; puts 1; lot size(s) 65"));
        srv.join().unwrap();

        // Also verify non-2xx HTTP status when `http_status_as_error(false)`.
        let listener_404 = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url_404 = format!("http://{}/NFO.zip", listener_404.local_addr().unwrap());
        let srv_404 = std::thread::spawn(move || {
            let (mut stream, _) = listener_404.accept().unwrap();
            let mut buf = [0_u8; 1024];
            let _ = std::io::Read::read(&mut stream, &mut buf);
            stream
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        });
        let no_err_agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        let err_404 = check_contracts_from(&no_err_agent, &url_404, now_ist).unwrap_err();
        assert!(err_404.contains("HTTP 404"), "{err_404}");
        srv_404.join().unwrap();
    }
}
