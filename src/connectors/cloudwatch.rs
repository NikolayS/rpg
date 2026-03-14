//! AWS `CloudWatch` connector (Phase 4).
//!
//! Fetches metrics and alarms from AWS `CloudWatch` using raw HTTP with
//! AWS Signature V4 request signing implemented via `ring::hmac` and
//! `ring::digest`.

use std::collections::HashMap;

use async_trait::async_trait;

use super::{
    Alert, AlertStatus, BackoffConfig, Connector, ConnectorCapabilities, ConnectorError,
    ConnectorHealth, DatabaseId, Metric, RateLimitConfig, TimeWindow,
};

// ---------------------------------------------------------------------------
// CloudWatchConnector
// ---------------------------------------------------------------------------

/// Connector for AWS `CloudWatch` metrics and alarms.
///
/// Communicates with the `CloudWatch` Monitoring API endpoint:
/// `https://monitoring.<region>.amazonaws.com`
///
/// Authentication uses AWS access key credentials; temporary credentials
/// (e.g. from IAM role assumption) are supported via the optional
/// session token.
#[allow(dead_code)]
pub struct CloudWatchConnector {
    region: String,
    access_key_id: String,
    secret_access_key: String,
    session_token: Option<String>,
    db_instance_id: Option<String>,
}

impl CloudWatchConnector {
    /// Create a new connector with long-term IAM credentials.
    pub fn new(region: String, access_key_id: String, secret_access_key: String) -> Self {
        Self {
            region,
            access_key_id,
            secret_access_key,
            session_token: None,
            db_instance_id: None,
        }
    }

    /// Attach a session token for temporary credentials (STS / IAM role).
    #[allow(dead_code)]
    pub fn with_session_token(mut self, token: String) -> Self {
        self.session_token = Some(token);
        self
    }

    /// Scope all metric/alarm queries to a specific RDS DB instance.
    #[allow(dead_code)]
    pub fn with_db_instance(mut self, id: String) -> Self {
        self.db_instance_id = Some(id);
        self
    }

    /// Base URL of the `CloudWatch` Monitoring API for this region.
    fn endpoint(&self) -> String {
        format!("https://monitoring.{}.amazonaws.com", self.region)
    }

    /// Host header value for the `CloudWatch` Monitoring API.
    fn host(&self) -> String {
        format!("monitoring.{}.amazonaws.com", self.region)
    }
}

// ---------------------------------------------------------------------------
// AWS Signature V4
// ---------------------------------------------------------------------------

/// Produce a lowercase hex string of `bytes`.
fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// SHA-256 digest of `data`, returned as lowercase hex.
fn sha256_hex(data: &[u8]) -> String {
    use ring::digest;
    let digest = digest::digest(&digest::SHA256, data);
    hex_encode(digest.as_ref())
}

/// HMAC-SHA256(`key`, `data`), returning raw bytes.
fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    use ring::hmac;
    let k = hmac::Key::new(hmac::HMAC_SHA256, key);
    let tag = hmac::sign(&k, data);
    tag.as_ref().to_vec()
}

/// AWS Signature V4 signing key derivation:
/// `HMAC(HMAC(HMAC(HMAC("AWS4" + secret, date), region), service), "aws4_request")`
fn derive_signing_key(secret: &str, date_stamp: &str, region: &str, service: &str) -> Vec<u8> {
    let k_secret = format!("AWS4{secret}");
    let k_date = hmac_sha256(k_secret.as_bytes(), date_stamp.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    hmac_sha256(&k_service, b"aws4_request")
}

/// Parameters needed to sign a single AWS API request.
struct SigningParams<'a> {
    /// HTTP method, e.g. `"POST"`.
    method: &'a str,
    /// URI path, e.g. `"/"`.
    uri: &'a str,
    /// Pre-sorted query string (empty for POST body requests).
    query_string: &'a str,
    /// Payload body bytes.
    payload: &'a [u8],
    /// Service name, e.g. `"monitoring"`.
    service: &'a str,
    /// AWS region, e.g. `"us-east-1"`.
    region: &'a str,
    /// Access key ID.
    access_key_id: &'a str,
    /// Secret access key.
    secret_access_key: &'a str,
    /// Optional session token for temporary credentials.
    session_token: Option<&'a str>,
    /// Host header value, e.g. `"monitoring.us-east-1.amazonaws.com"`.
    host: &'a str,
    /// Full ISO 8601 timestamp: `"20230101T120000Z"`.
    amz_date: &'a str,
    /// Date stamp: `"20230101"`.
    date_stamp: &'a str,
}

/// Build the `Authorization` header value and return all headers that must be
/// sent with the request as `(name, value)` pairs.
///
/// The returned vec always includes:
/// - `host`
/// - `x-amz-date`
/// - `x-amz-security-token` (when `session_token` is set)
/// - `content-type`
/// - `authorization`
fn sign_request(p: &SigningParams<'_>) -> Vec<(String, String)> {
    let payload_hash = sha256_hex(p.payload);

    // --- Step 1: canonical request -----------------------------------------
    //
    // Signed headers must be sorted lexicographically. We always sign:
    //   content-type, host, x-amz-date, and (when present) x-amz-security-token
    let content_type = "application/x-www-form-urlencoded";

    let mut header_pairs: Vec<(&str, String)> = vec![
        ("content-type", content_type.to_string()),
        ("host", p.host.to_string()),
        ("x-amz-date", p.amz_date.to_string()),
    ];
    if let Some(tok) = p.session_token {
        header_pairs.push(("x-amz-security-token", tok.to_string()));
    }
    // Sort lexicographically by header name (they already are, but sort for
    // safety in case the set changes).
    header_pairs.sort_by(|a, b| a.0.cmp(b.0));

    let canonical_headers: String = header_pairs.iter().fold(String::new(), |mut acc, (k, v)| {
        use std::fmt::Write as _;
        let _ = writeln!(acc, "{k}:{v}");
        acc
    });

    let signed_headers: String = header_pairs
        .iter()
        .map(|(k, _)| *k)
        .collect::<Vec<_>>()
        .join(";");

    let canonical_request = format!(
        "{method}\n{uri}\n{qs}\n{headers}\n{signed}\n{payload_hash}",
        method = p.method,
        uri = p.uri,
        qs = p.query_string,
        headers = canonical_headers,
        signed = signed_headers,
        payload_hash = payload_hash,
    );

    // --- Step 2: string to sign --------------------------------------------
    let credential_scope = format!("{}/{}/{}/aws4_request", p.date_stamp, p.region, p.service);

    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{cr_hash}",
        amz_date = p.amz_date,
        scope = credential_scope,
        cr_hash = sha256_hex(canonical_request.as_bytes()),
    );

    // --- Step 3: signing key and signature ---------------------------------
    let signing_key = derive_signing_key(p.secret_access_key, p.date_stamp, p.region, p.service);
    let signature = hex_encode(&hmac_sha256(&signing_key, string_to_sign.as_bytes()));

    // --- Step 4: Authorization header --------------------------------------
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={access_key}/{scope}, \
         SignedHeaders={signed}, Signature={sig}",
        access_key = p.access_key_id,
        scope = credential_scope,
        signed = signed_headers,
        sig = signature,
    );

    // Collect final header list (exclude authorization — append at end).
    let mut headers: Vec<(String, String)> = header_pairs
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    headers.push(("authorization".to_string(), authorization));
    headers
}

// ---------------------------------------------------------------------------
// Timestamp helpers (no chrono — use std::time)
// ---------------------------------------------------------------------------

/// Format `SystemTime` as the `YYYYMMDDTHHmmssZ` string required by `SigV4`.
fn format_amz_date(t: std::time::SystemTime) -> String {
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Manual UTC decomposition — avoids pulling in chrono.
    let (year, month, day, hour, minute, second) = epoch_secs_to_utc(secs);
    format!("{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z")
}

/// Extract just the `YYYYMMDD` date stamp from a `SystemTime`.
fn format_date_stamp(t: std::time::SystemTime) -> String {
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (year, month, day, _, _, _) = epoch_secs_to_utc(secs);
    format!("{year:04}{month:02}{day:02}")
}

/// Decompose a Unix timestamp (seconds since epoch) into UTC components.
///
/// Returns `(year, month, day, hour, minute, second)`.
/// Handles dates from 1970-01-01 through roughly year 2100 correctly.
#[allow(clippy::cast_possible_truncation)] // all intermediate values are bounded
fn epoch_secs_to_utc(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let second = (secs % 60) as u32; // 0-59
    let minutes = secs / 60;
    let minute = (minutes % 60) as u32; // 0-59
    let hours = minutes / 60;
    let hour = (hours % 24) as u32; // 0-23
    let mut days = hours / 24; // days since 1970-01-01

    // Leap-year-aware date extraction using the 400-year Gregorian cycle.
    // Algorithm from "Euclidean affine functions and their application to
    // calendar algorithms" (Neri & Schneider, 2022) / public domain.
    days += 719_468; // shift epoch to 0000-03-01
    let era = days / 146_097;
    let doe = days % 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    // doy < 366, mp < 12 — values fit in u32
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month_u64 = if mp < 10 { mp + 3 } else { mp - 9 }; // 1-12
    let month = month_u64 as u32;
    let year = (if month <= 2 { y + 1 } else { y }) as u32;

    (year, month, day, hour, minute, second)
}

// ---------------------------------------------------------------------------
// Response parsing helpers
// ---------------------------------------------------------------------------

/// Parse a `GetMetricData` XML response into `Vec<Metric>`.
///
/// The `CloudWatch` Query API returns XML like:
/// ```xml
/// <GetMetricDataResponse>
///   <GetMetricDataResult>
///     <MetricDataResults>
///       <member>
///         <Id>cpu</Id>
///         <Label>CPUUtilization</Label>
///         <Timestamps><member>2023-01-01T12:00:00Z</member></Timestamps>
///         <Values><member>42.5</member></Values>
///         <StatusCode>Complete</StatusCode>
///       </member>
///     </MetricDataResults>
///   </GetMetricDataResult>
/// </GetMetricDataResponse>
/// ```
///
/// This parser extracts `(label, value, timestamp_secs)` triples using
/// simple string scanning — no XML library dependency needed.
fn parse_metric_data_response(xml: &str, database: &str, source: &str) -> Vec<Metric> {
    let mut metrics = Vec::new();

    // Iterate over <member> blocks inside <MetricDataResults>.
    for member in split_tags(xml, "member") {
        let label = extract_tag(&member, "Label").unwrap_or_default();
        if label.is_empty() {
            continue;
        }
        // Extract parallel <Timestamps> and <Values> lists.
        let timestamps_block = extract_tag(&member, "Timestamps").unwrap_or_default();
        let values_block = extract_tag(&member, "Values").unwrap_or_default();

        let ts_list: Vec<&str> = timestamps_block
            .split("<member>")
            .skip(1)
            .filter_map(|s| s.split("</member>").next())
            .collect();
        let val_list: Vec<f64> = values_block
            .split("<member>")
            .skip(1)
            .filter_map(|s| s.split("</member>").next())
            .filter_map(|s| s.trim().parse().ok())
            .collect();

        for (ts_str, value) in ts_list.iter().zip(val_list.iter()) {
            let timestamp =
                parse_iso8601_to_system_time(ts_str.trim()).unwrap_or(std::time::UNIX_EPOCH);
            let mut tags = HashMap::new();
            tags.insert("db_instance".to_string(), database.to_string());
            metrics.push(Metric {
                name: label.clone(),
                value: *value,
                unit: None,
                timestamp,
                tags,
                source: source.to_string(),
            });
        }
    }
    metrics
}

/// Parse a `DescribeAlarms` XML response into `Vec<Alert>`.
///
/// Extracts `MetricAlarm` members from the `MetricAlarms` list and maps
/// `ALARM` state → `AlertStatus::Active`, anything else → `Resolved`.
fn parse_describe_alarms_response(xml: &str, database: &str, source: &str) -> Vec<Alert> {
    let mut alerts = Vec::new();

    // Find MetricAlarms block.
    let alarms_block = extract_tag(xml, "MetricAlarms").unwrap_or_default();

    for member in split_tags(&alarms_block, "member") {
        let name = extract_tag(&member, "AlarmName").unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        let description = extract_tag(&member, "AlarmDescription").unwrap_or_default();
        let state = extract_tag(&member, "StateValue").unwrap_or_default();
        let state_updated = extract_tag(&member, "StateUpdatedTimestamp").unwrap_or_default();
        let arn = extract_tag(&member, "AlarmArn").unwrap_or_default();

        let status = if state.trim().eq_ignore_ascii_case("ALARM") {
            AlertStatus::Active
        } else {
            AlertStatus::Resolved
        };

        let created_at =
            parse_iso8601_to_system_time(state_updated.trim()).unwrap_or(std::time::UNIX_EPOCH);

        let url = if arn.is_empty() { None } else { Some(arn) };
        let title = if description.is_empty() {
            name.clone()
        } else {
            format!("{name}: {description}")
        };

        alerts.push(Alert {
            id: name,
            title,
            severity: crate::governance::Severity::Warning,
            status,
            source: source.to_string(),
            database: Some(database.to_string()),
            created_at,
            url,
        });
    }
    alerts
}

/// Return the inner text of the first occurrence of `<tag>…</tag>` in `s`.
fn extract_tag(s: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = s.find(&open)? + open.len();
    let end = s[start..].find(&close)? + start;
    Some(s[start..end].to_string())
}

/// Split `s` on `<tag>…</tag>` boundaries and return each inner content.
///
/// Correctly handles nested elements with the same tag name by tracking
/// depth — the closing `</tag>` is matched to its corresponding `<tag>`.
fn split_tags(s: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut result = Vec::new();
    let mut pos = 0;

    while pos < s.len() {
        // Find the next opening tag.
        let Some(rel_open) = s[pos..].find(&open) else {
            break;
        };
        let inner_start = pos + rel_open + open.len();
        let mut depth = 1usize;
        let mut search = inner_start;

        // Advance through the string tracking nesting depth.
        loop {
            let next_open = s[search..].find(&open).map(|i| i + search);
            let next_close = s[search..].find(&close).map(|i| i + search);
            match (next_open, next_close) {
                (_, None) => {
                    // No closing tag — malformed XML, stop.
                    return result;
                }
                (Some(no), Some(nc)) if no < nc => {
                    depth += 1;
                    search = no + open.len();
                }
                (_, Some(nc)) => {
                    depth -= 1;
                    if depth == 0 {
                        result.push(s[inner_start..nc].to_string());
                        pos = nc + close.len();
                        break;
                    }
                    search = nc + close.len();
                }
            }
        }
    }
    result
}

/// Parse an ISO 8601 timestamp `"2023-01-01T12:00:00Z"` to `SystemTime`.
///
/// Returns `None` if parsing fails.
fn parse_iso8601_to_system_time(s: &str) -> Option<std::time::SystemTime> {
    // Expected format: YYYY-MM-DDTHH:MM:SSZ  (19+ chars)
    if s.len() < 19 {
        return None;
    }
    let year: u64 = s[0..4].parse().ok()?;
    let month: u64 = s[5..7].parse().ok()?;
    let day: u64 = s[8..10].parse().ok()?;
    let hour: u64 = s[11..13].parse().ok()?;
    let minute: u64 = s[14..16].parse().ok()?;
    let second: u64 = s[17..19].parse().ok()?;

    // Days from 1970-01-01 to start of year.
    let y = year;
    let days_to_year: u64 = 365 * (y - 1970) + (y - 1969) / 4 - (y - 1901) / 100 + (y - 1601) / 400;

    // Days within the year up to the start of the month.
    let days_in_months: [u64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let is_leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    // month is 1-12 parsed from two ASCII digits; usize is at least 16 bits.
    let prior_months = usize::try_from(month - 1).unwrap_or(0);
    let mut days_in_year: u64 = 0;
    for (idx, &dim) in days_in_months.iter().enumerate().take(prior_months) {
        days_in_year += dim;
        if idx == 1 && is_leap {
            days_in_year += 1;
        }
    }

    let total_days = days_to_year + days_in_year + (day - 1);
    let total_secs = total_days * 86_400 + hour * 3_600 + minute * 60 + second;

    Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(total_secs))
}

// ---------------------------------------------------------------------------
// Connector trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl Connector for CloudWatchConnector {
    fn id(&self) -> &'static str {
        "cloudwatch"
    }

    fn name(&self) -> &'static str {
        "AWS CloudWatch"
    }

    fn capabilities(&self) -> ConnectorCapabilities {
        ConnectorCapabilities {
            can_fetch_metrics: true,
            can_fetch_alerts: true,
            can_create_issues: false,
            can_update_issues: false,
            can_receive_webhooks: false,
            supports_pagination: false,
        }
    }

    fn rate_limit_config(&self) -> RateLimitConfig {
        RateLimitConfig {
            requests_per_second: 5.0,
            requests_per_minute: Some(300),
            max_concurrent: 5,
            backoff: BackoffConfig::default(),
            respect_retry_after: true,
        }
    }

    /// Validate connectivity.
    ///
    /// `CloudWatch` does not provide a dedicated ping endpoint; credential
    /// validation is deferred to the first real API call.  This method
    /// returns `connected: true` immediately so that the connector can be
    /// registered without performing a network round-trip at startup.
    async fn health_check(&self) -> Result<ConnectorHealth, ConnectorError> {
        Ok(ConnectorHealth {
            connected: true,
            message: Some("credential validation deferred to first API call".to_string()),
            latency_ms: None,
        })
    }

    /// Fetch `CloudWatch` metric data points for `database` over `window`.
    ///
    /// Constructs and signs a `GetMetricData` POST request, executes it via
    /// `reqwest`, and parses the XML response into `Vec<Metric>`.
    async fn fetch_metrics(
        &self,
        database: &DatabaseId,
        window: &TimeWindow,
    ) -> Result<Vec<Metric>, ConnectorError> {
        let now = std::time::SystemTime::now();
        let amz_date = format_amz_date(now);
        let date_stamp = format_date_stamp(now);

        let start_secs = window
            .start
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let end_secs = window
            .end
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let body = format!(
            "Action=GetMetricData\
             &Version=2010-08-01\
             &MetricDataQueries.member.1.Id=cpu\
             &MetricDataQueries.member.1.MetricStat.Metric.Namespace=AWS%2FRDS\
             &MetricDataQueries.member.1.MetricStat.Metric.MetricName=CPUUtilization\
             &MetricDataQueries.member.1.MetricStat.Metric.Dimensions.member.1.Name=\
DBInstanceIdentifier\
             &MetricDataQueries.member.1.MetricStat.Metric.Dimensions.member.1.Value=\
{database}\
             &MetricDataQueries.member.1.MetricStat.Period=60\
             &MetricDataQueries.member.1.MetricStat.Stat=Average\
             &StartTime={start_secs}\
             &EndTime={end_secs}"
        );

        let params = SigningParams {
            method: "POST",
            uri: "/",
            query_string: "",
            payload: body.as_bytes(),
            service: "monitoring",
            region: &self.region,
            access_key_id: &self.access_key_id,
            secret_access_key: &self.secret_access_key,
            session_token: self.session_token.as_deref(),
            host: &self.host(),
            amz_date: &amz_date,
            date_stamp: &date_stamp,
        };

        let headers = sign_request(&params);

        let client = reqwest::Client::new();
        let mut req = client.post(self.endpoint()).body(body);
        for (name, value) in &headers {
            req = req.header(name.as_str(), value.as_str());
        }

        let resp = req
            .send()
            .await
            .map_err(|e| ConnectorError::NetworkError(e.to_string()))?;

        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .map_err(|e| ConnectorError::NetworkError(e.to_string()))?;

        if status != 200 {
            return Err(ConnectorError::ApiError {
                status,
                message: text,
            });
        }

        Ok(parse_metric_data_response(&text, database, self.id()))
    }

    /// Fetch `CloudWatch` alarms for `database`.
    ///
    /// Constructs and signs a `DescribeAlarms` POST request, executes it via
    /// `reqwest`, and parses the XML response into `Vec<Alert>`.
    async fn fetch_alerts(&self, database: &DatabaseId) -> Result<Vec<Alert>, ConnectorError> {
        let now = std::time::SystemTime::now();
        let amz_date = format_amz_date(now);
        let date_stamp = format_date_stamp(now);

        let body = format!(
            "Action=DescribeAlarms\
             &Version=2010-08-01\
             &AlarmNamePrefix={database}-\
             &StateValue=ALARM"
        );

        let params = SigningParams {
            method: "POST",
            uri: "/",
            query_string: "",
            payload: body.as_bytes(),
            service: "monitoring",
            region: &self.region,
            access_key_id: &self.access_key_id,
            secret_access_key: &self.secret_access_key,
            session_token: self.session_token.as_deref(),
            host: &self.host(),
            amz_date: &amz_date,
            date_stamp: &date_stamp,
        };

        let headers = sign_request(&params);

        let client = reqwest::Client::new();
        let mut req = client.post(self.endpoint()).body(body);
        for (name, value) in &headers {
            req = req.header(name.as_str(), value.as_str());
        }

        let resp = req
            .send()
            .await
            .map_err(|e| ConnectorError::NetworkError(e.to_string()))?;

        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .map_err(|e| ConnectorError::NetworkError(e.to_string()))?;

        if status != 200 {
            return Err(ConnectorError::ApiError {
                status,
                message: text,
            });
        }

        Ok(parse_describe_alarms_response(&text, database, self.id()))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::Connector;

    fn make_connector() -> CloudWatchConnector {
        CloudWatchConnector::new(
            "us-east-1".to_string(),
            "AKIAIOSFODNN7EXAMPLE".to_string(),
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
        )
    }

    // ------------------------------------------------------------------
    // Construction
    // ------------------------------------------------------------------

    #[test]
    fn new_stores_credentials() {
        let c = make_connector();
        assert_eq!(c.region, "us-east-1");
        assert_eq!(c.access_key_id, "AKIAIOSFODNN7EXAMPLE");
        assert_eq!(
            c.secret_access_key,
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"
        );
        assert!(c.session_token.is_none());
        assert!(c.db_instance_id.is_none());
    }

    #[test]
    fn with_session_token_sets_token() {
        let c = make_connector().with_session_token("my-session-token".to_string());
        assert_eq!(c.session_token.as_deref(), Some("my-session-token"));
    }

    #[test]
    fn with_db_instance_sets_id() {
        let c = make_connector().with_db_instance("prod-pg-01".to_string());
        assert_eq!(c.db_instance_id.as_deref(), Some("prod-pg-01"));
    }

    #[test]
    fn builder_pattern_chaining() {
        let c = make_connector()
            .with_session_token("tok".to_string())
            .with_db_instance("db-id".to_string());
        assert_eq!(c.session_token.as_deref(), Some("tok"));
        assert_eq!(c.db_instance_id.as_deref(), Some("db-id"));
    }

    // ------------------------------------------------------------------
    // Endpoint
    // ------------------------------------------------------------------

    #[test]
    fn endpoint_us_east_1() {
        let c = make_connector();
        assert_eq!(c.endpoint(), "https://monitoring.us-east-1.amazonaws.com");
    }

    #[test]
    fn endpoint_eu_west_2() {
        let c = CloudWatchConnector::new(
            "eu-west-2".to_string(),
            "key".to_string(),
            "secret".to_string(),
        );
        assert_eq!(c.endpoint(), "https://monitoring.eu-west-2.amazonaws.com");
    }

    // ------------------------------------------------------------------
    // Connector trait — identity
    // ------------------------------------------------------------------

    #[test]
    fn id_is_cloudwatch() {
        assert_eq!(make_connector().id(), "cloudwatch");
    }

    #[test]
    fn name_is_aws_cloudwatch() {
        assert_eq!(make_connector().name(), "AWS CloudWatch");
    }

    // ------------------------------------------------------------------
    // Connector trait — capabilities
    // ------------------------------------------------------------------

    #[test]
    fn capabilities_metrics_and_alerts() {
        let caps = make_connector().capabilities();
        assert!(caps.can_fetch_metrics);
        assert!(caps.can_fetch_alerts);
        assert!(!caps.can_create_issues);
        assert!(!caps.can_update_issues);
        assert!(!caps.can_receive_webhooks);
        assert!(!caps.supports_pagination);
    }

    // ------------------------------------------------------------------
    // Connector trait — rate limiting
    // ------------------------------------------------------------------

    #[test]
    fn rate_limit_config_values() {
        let rl = make_connector().rate_limit_config();
        assert!((rl.requests_per_second - 5.0).abs() < f64::EPSILON);
        assert_eq!(rl.requests_per_minute, Some(300));
        assert_eq!(rl.max_concurrent, 5);
        assert!(rl.respect_retry_after);
    }

    // ------------------------------------------------------------------
    // Connector trait — async methods
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn health_check_returns_connected() {
        let health = make_connector().health_check().await.unwrap();
        assert!(health.connected);
        assert!(health.message.is_some());
    }

    // ------------------------------------------------------------------
    // SigV4 primitives — hex encoding
    // ------------------------------------------------------------------

    #[test]
    fn hex_encode_empty() {
        assert_eq!(hex_encode(b""), "");
    }

    #[test]
    fn hex_encode_known_bytes() {
        assert_eq!(hex_encode(&[0x00, 0xff, 0x0a, 0xb3]), "00ff0ab3");
    }

    // ------------------------------------------------------------------
    // SigV4 primitives — SHA-256
    // ------------------------------------------------------------------

    #[test]
    fn sha256_empty_string() {
        // SHA-256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn sha256_abc() {
        // SHA-256("abc") per FIPS 180-4 example B.1
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    // ------------------------------------------------------------------
    // SigV4 primitives — HMAC-SHA256
    // ------------------------------------------------------------------

    #[test]
    fn hmac_sha256_rfc2202_test_case_1() {
        // HMAC-SHA256 with key = 0x0b*20, data = "Hi There"
        // Expected from RFC 4231 test case 1:
        // b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7
        let key = [0x0bu8; 20];
        let result = hmac_sha256(&key, b"Hi There");
        assert_eq!(
            hex_encode(&result),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    // ------------------------------------------------------------------
    // SigV4 — signing key derivation (AWS test vector)
    // ------------------------------------------------------------------

    #[test]
    fn derive_signing_key_aws_test_vector() {
        // AWS documentation example:
        // secret  = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY"
        // date    = "20150830"
        // region  = "us-east-1"
        // service = "iam"
        //
        // Expected signing key (hex) verified independently with Python hmac:
        // c4afb1cc5771d871763a393e44b703571b55cc28424d1a5e86da6ed3c154a4b9
        //
        // Source: https://docs.aws.amazon.com/general/latest/gr/
        //         sigv4-calculate-signature.html (key contains '+')
        let key = derive_signing_key(
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            "20150830",
            "us-east-1",
            "iam",
        );
        assert_eq!(
            hex_encode(&key),
            "c4afb1cc5771d871763a393e44b703571b55cc28424d1a5e86da6ed3c154a4b9"
        );
    }

    // ------------------------------------------------------------------
    // SigV4 — full canonical request + authorization header
    // ------------------------------------------------------------------

    #[test]
    fn sign_request_produces_authorization_header() {
        // Fixture: deterministic timestamp so the test is repeatable.
        let body = b"Action=GetMetricData&Version=2010-08-01";
        let params = SigningParams {
            method: "POST",
            uri: "/",
            query_string: "",
            payload: body,
            service: "monitoring",
            region: "us-east-1",
            access_key_id: "AKIAIOSFODNN7EXAMPLE",
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            session_token: None,
            host: "monitoring.us-east-1.amazonaws.com",
            amz_date: "20230601T120000Z",
            date_stamp: "20230601",
        };

        let headers = sign_request(&params);

        // Must contain an authorization header starting with the algorithm.
        let auth = headers
            .iter()
            .find(|(k, _)| k == "authorization")
            .map(|(_, v)| v.as_str())
            .unwrap_or("");

        assert!(
            auth.starts_with("AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/"),
            "authorization header prefix mismatch: {auth}"
        );
        assert!(
            auth.contains("SignedHeaders=content-type;host;x-amz-date"),
            "signed headers list missing: {auth}"
        );
        assert!(
            auth.contains("Signature="),
            "signature field missing: {auth}"
        );

        // x-amz-date must match the input.
        let amz = headers
            .iter()
            .find(|(k, _)| k == "x-amz-date")
            .map(|(_, v)| v.as_str())
            .unwrap_or("");
        assert_eq!(amz, "20230601T120000Z");
    }

    #[test]
    fn sign_request_with_session_token_includes_security_token_header() {
        let body = b"Action=DescribeAlarms";
        let params = SigningParams {
            method: "POST",
            uri: "/",
            query_string: "",
            payload: body,
            service: "monitoring",
            region: "eu-west-1",
            access_key_id: "ASIAIOSFODNN7EXAMPLE",
            secret_access_key: "secret",
            session_token: Some("my-sts-token"),
            host: "monitoring.eu-west-1.amazonaws.com",
            amz_date: "20230601T120000Z",
            date_stamp: "20230601",
        };

        let headers = sign_request(&params);

        let tok = headers
            .iter()
            .find(|(k, _)| k == "x-amz-security-token")
            .map(|(_, v)| v.as_str());
        assert_eq!(tok, Some("my-sts-token"));

        // Session token header must also appear in SignedHeaders.
        let auth = headers
            .iter()
            .find(|(k, _)| k == "authorization")
            .map(|(_, v)| v.as_str())
            .unwrap_or("");
        assert!(
            auth.contains("x-amz-security-token"),
            "security token not in signed headers: {auth}"
        );
    }

    // ------------------------------------------------------------------
    // Timestamp helpers
    // ------------------------------------------------------------------

    #[test]
    fn epoch_secs_to_utc_unix_epoch() {
        assert_eq!(epoch_secs_to_utc(0), (1970, 1, 1, 0, 0, 0));
    }

    #[test]
    fn epoch_secs_to_utc_known_date() {
        // 2023-06-01T12:00:00Z = 1685620800
        assert_eq!(epoch_secs_to_utc(1_685_620_800), (2023, 6, 1, 12, 0, 0));
    }

    #[test]
    fn format_amz_date_known_timestamp() {
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_685_620_800);
        assert_eq!(format_amz_date(t), "20230601T120000Z");
    }

    #[test]
    fn format_date_stamp_known_timestamp() {
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_685_620_800);
        assert_eq!(format_date_stamp(t), "20230601");
    }

    // ------------------------------------------------------------------
    // XML helpers
    // ------------------------------------------------------------------

    #[test]
    fn extract_tag_basic() {
        let xml = "<Foo><Bar>hello</Bar></Foo>";
        assert_eq!(extract_tag(xml, "Bar").as_deref(), Some("hello"));
        assert_eq!(extract_tag(xml, "Foo").as_deref(), Some("<Bar>hello</Bar>"));
    }

    #[test]
    fn extract_tag_missing() {
        assert!(extract_tag("<a>x</a>", "b").is_none());
    }

    #[test]
    fn split_tags_basic() {
        let xml = "<list><member>a</member><member>b</member></list>";
        let inner = extract_tag(xml, "list").unwrap();
        let parts = split_tags(&inner, "member");
        assert_eq!(parts, vec!["a", "b"]);
    }

    #[test]
    fn parse_iso8601_to_system_time_known() {
        // 2023-06-01T12:00:00Z = 1685620800 secs since epoch
        let t = parse_iso8601_to_system_time("2023-06-01T12:00:00Z").unwrap();
        let secs = t.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        assert_eq!(secs, 1_685_620_800);
    }

    #[test]
    fn parse_iso8601_to_system_time_epoch() {
        let t = parse_iso8601_to_system_time("1970-01-01T00:00:00Z").unwrap();
        assert_eq!(t, std::time::UNIX_EPOCH);
    }

    #[test]
    fn parse_iso8601_to_system_time_invalid() {
        assert!(parse_iso8601_to_system_time("not-a-date").is_none());
        assert!(parse_iso8601_to_system_time("").is_none());
    }

    // ------------------------------------------------------------------
    // Response parsing
    // ------------------------------------------------------------------

    #[test]
    fn parse_metric_data_response_basic() {
        let xml = r#"
<GetMetricDataResponse>
  <GetMetricDataResult>
    <MetricDataResults>
      <member>
        <Id>cpu</Id>
        <Label>CPUUtilization</Label>
        <Timestamps>
          <member>2023-06-01T12:00:00Z</member>
        </Timestamps>
        <Values>
          <member>42.5</member>
        </Values>
        <StatusCode>Complete</StatusCode>
      </member>
    </MetricDataResults>
  </GetMetricDataResult>
</GetMetricDataResponse>"#;

        let metrics = parse_metric_data_response(xml, "mydb", "cloudwatch");
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].name, "CPUUtilization");
        assert!((metrics[0].value - 42.5).abs() < f64::EPSILON);
        assert_eq!(metrics[0].source, "cloudwatch");
        assert_eq!(
            metrics[0].tags.get("db_instance").map(|s| s.as_str()),
            Some("mydb")
        );
    }

    #[test]
    fn parse_metric_data_response_multiple_points() {
        let xml = r#"
<GetMetricDataResponse><GetMetricDataResult><MetricDataResults>
  <member>
    <Label>FreeableMemory</Label>
    <Timestamps>
      <member>2023-06-01T12:00:00Z</member>
      <member>2023-06-01T12:01:00Z</member>
    </Timestamps>
    <Values>
      <member>1024.0</member>
      <member>2048.0</member>
    </Values>
  </member>
</MetricDataResults></GetMetricDataResult></GetMetricDataResponse>"#;

        let metrics = parse_metric_data_response(xml, "db", "cloudwatch");
        assert_eq!(metrics.len(), 2);
        assert_eq!(metrics[0].name, "FreeableMemory");
        assert!((metrics[0].value - 1024.0).abs() < f64::EPSILON);
        assert!((metrics[1].value - 2048.0).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_metric_data_response_empty() {
        let metrics = parse_metric_data_response("<GetMetricDataResponse/>", "db", "cloudwatch");
        assert!(metrics.is_empty());
    }

    #[test]
    fn parse_describe_alarms_response_active_alarm() {
        let xml = r#"
<DescribeAlarmsResponse>
  <DescribeAlarmsResult>
    <MetricAlarms>
      <member>
        <AlarmName>prod-pg-HighCPU</AlarmName>
        <AlarmDescription>CPU above threshold</AlarmDescription>
        <StateValue>ALARM</StateValue>
        <StateUpdatedTimestamp>2023-06-01T12:00:00Z</StateUpdatedTimestamp>
        <AlarmArn>arn:aws:cloudwatch:us-east-1:123:alarm:prod-pg-HighCPU</AlarmArn>
      </member>
    </MetricAlarms>
  </DescribeAlarmsResult>
</DescribeAlarmsResponse>"#;

        let alerts = parse_describe_alarms_response(xml, "prod-pg", "cloudwatch");
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].id, "prod-pg-HighCPU");
        assert_eq!(alerts[0].status, AlertStatus::Active);
        assert_eq!(alerts[0].source, "cloudwatch");
        assert_eq!(alerts[0].database.as_deref(), Some("prod-pg"));
        assert!(alerts[0].url.is_some());
    }

    #[test]
    fn parse_describe_alarms_response_ok_state_resolves() {
        let xml = r#"
<DescribeAlarmsResponse><DescribeAlarmsResult><MetricAlarms>
  <member>
    <AlarmName>prod-pg-LowMemory</AlarmName>
    <StateValue>OK</StateValue>
    <StateUpdatedTimestamp>2023-06-01T10:00:00Z</StateUpdatedTimestamp>
    <AlarmArn></AlarmArn>
  </member>
</MetricAlarms></DescribeAlarmsResult></DescribeAlarmsResponse>"#;

        let alerts = parse_describe_alarms_response(xml, "prod-pg", "cloudwatch");
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].status, AlertStatus::Resolved);
        assert!(alerts[0].url.is_none());
    }

    #[test]
    fn parse_describe_alarms_response_empty() {
        let alerts =
            parse_describe_alarms_response("<DescribeAlarmsResponse/>", "db", "cloudwatch");
        assert!(alerts.is_empty());
    }
}
