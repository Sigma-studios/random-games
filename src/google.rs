//! A small Google Sheets v4 client authenticated as a service account: just
//! the calls the bot makes.

use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;

const SCOPE: &str = "https://www.googleapis.com/auth/spreadsheets";
const API: &str = "https://sheets.googleapis.com/v4/spreadsheets";

#[derive(Deserialize)]
struct ServiceAccount {
    client_email: String,
    private_key: String,
    #[serde(default = "token_uri")]
    token_uri: String,
}

fn token_uri() -> String {
    "https://oauth2.googleapis.com/token".into()
}

pub struct Sheets {
    http: reqwest::Client,
    account: ServiceAccount,
    key: jsonwebtoken::EncodingKey,
    spreadsheet: String,
    token: Mutex<Option<(String, Instant)>>,
}

/// How cells are read: formulas as typed, or values as computed.
#[derive(Clone, Copy)]
pub enum Render {
    Formula,
    Unformatted,
}

impl Render {
    fn as_str(self) -> &'static str {
        match self {
            Render::Formula => "FORMULA",
            Render::Unformatted => "UNFORMATTED_VALUE",
        }
    }
}

pub type Rows = Vec<Vec<Value>>;

impl Sheets {
    pub fn new(service_account_json: &str, spreadsheet: &str) -> anyhow::Result<Sheets> {
        let account: ServiceAccount = serde_json::from_str(service_account_json)
            .context("GOOGLE_SERVICE_ACCOUNT is not a service account key (JSON)")?;
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(account.private_key.as_bytes())
            .context("the service account's private_key is not a valid RSA key")?;
        Ok(Sheets {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()?,
            account,
            key,
            spreadsheet: spreadsheet.to_string(),
            token: Mutex::new(None),
        })
    }

    pub fn service_account_email(&self) -> &str {
        &self.account.client_email
    }

    async fn token(&self) -> anyhow::Result<String> {
        let mut cached = self.token.lock().await;
        if let Some((token, until)) = cached.as_ref()
            && Instant::now() < *until
        {
            return Ok(token.clone());
        }
        let now = chrono::Utc::now().timestamp();
        let claims = json!({
            "iss": self.account.client_email,
            "scope": SCOPE,
            "aud": self.account.token_uri,
            "iat": now,
            "exp": now + 3600,
        });
        let assertion = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256),
            &claims,
            &self.key,
        )?;
        #[derive(Deserialize)]
        struct Token {
            access_token: String,
            expires_in: u64,
        }
        let response = self
            .http
            .post(&self.account.token_uri)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", assertion.as_str()),
            ])
            .send()
            .await
            .context("cannot reach Google's token endpoint")?;
        if !response.status().is_success() {
            bail!(
                "Google refused the service account: {}",
                response.text().await.unwrap_or_default()
            );
        }
        let token: Token = response.json().await?;
        // Renewed a minute early, so a request never carries a token that
        // expires on the way.
        let until = Instant::now() + Duration::from_secs(token.expires_in.saturating_sub(60));
        *cached = Some((token.access_token.clone(), until));
        Ok(token.access_token)
    }

    async fn call(&self, request: reqwest::RequestBuilder) -> anyhow::Result<Value> {
        let response = request.bearer_auth(self.token().await?).send().await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            let message = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
                .unwrap_or(body);
            if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::NOT_FOUND
            {
                bail!(
                    "Google Sheets: {message} (is the sheet shared with {}?)",
                    self.account.client_email
                );
            }
            bail!("Google Sheets {status}: {message}");
        }
        Ok(if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&body)?
        })
    }

    /// Several ranges in one request, rows padded by nothing (Google trims
    /// trailing empty cells).
    pub async fn get(&self, ranges: &[&str], render: Render) -> anyhow::Result<Vec<Rows>> {
        let mut query: Vec<(&str, &str)> = ranges.iter().map(|r| ("ranges", *r)).collect();
        query.push(("valueRenderOption", render.as_str()));
        query.push(("dateTimeRenderOption", "FORMATTED_STRING"));
        let value = self
            .call(
                self.http
                    .get(format!("{API}/{}/values:batchGet", self.spreadsheet))
                    .query(&query),
            )
            .await?;
        let ranges = value["valueRanges"].as_array().cloned().unwrap_or_default();
        Ok(ranges
            .into_iter()
            .map(|r| serde_json::from_value(r["values"].clone()).unwrap_or_default())
            .collect())
    }

    /// Writes cells as if typed in (formulas are formulas, "7,5" follows the
    /// sheet's locale).
    pub async fn set(&self, data: Vec<(String, Rows)>) -> anyhow::Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        let data: Vec<Value> = data
            .into_iter()
            .map(|(range, values)| json!({"range": range, "values": values}))
            .collect();
        self.call(
            self.http
                .post(format!("{API}/{}/values:batchUpdate", self.spreadsheet))
                .json(&json!({"valueInputOption": "USER_ENTERED", "data": data})),
        )
        .await?;
        Ok(())
    }

    /// Appends rows after the last row of `range`'s table. `raw` stores text as
    /// is (no date or number parsing).
    pub async fn append(&self, range: &str, rows: Rows, raw: bool) -> anyhow::Result<()> {
        let input = if raw { "RAW" } else { "USER_ENTERED" };
        self.call(
            self.http
                .post(format!(
                    "{API}/{}/values/{}:append",
                    self.spreadsheet,
                    percent_encoding::utf8_percent_encode(
                        range,
                        percent_encoding::NON_ALPHANUMERIC
                    )
                ))
                .query(&[
                    ("valueInputOption", input),
                    ("insertDataOption", "INSERT_ROWS"),
                ])
                .json(&json!({"values": rows})),
        )
        .await?;
        Ok(())
    }

    /// Structural changes (insert rows, add a sheet, append to a table, …).
    pub async fn batch_update(&self, requests: Vec<Value>) -> anyhow::Result<Value> {
        self.call(
            self.http
                .post(format!("{API}/{}:batchUpdate", self.spreadsheet))
                .json(&json!({"requests": requests})),
        )
        .await
    }

    /// Sheet ids, titles and tables.
    pub async fn metadata(&self) -> anyhow::Result<Value> {
        self.call(
            self.http
                .get(format!("{API}/{}", self.spreadsheet))
                .query(&[(
                    "fields",
                    "sheets(properties(sheetId,title),tables(tableId,name,range))",
                )]),
        )
        .await
    }
}

/// Column letters to a 0-based index: "A" → 0, "AA" → 26.
pub fn col_index(letters: &str) -> Option<usize> {
    let letters = letters.trim();
    if letters.is_empty() || !letters.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    Some(
        letters
            .to_ascii_uppercase()
            .bytes()
            .fold(0usize, |n, b| n * 26 + (b - b'A' + 1) as usize)
            - 1,
    )
}

/// A 0-based column index to its letters.
pub fn col_letters(mut index: usize) -> String {
    let mut out = Vec::new();
    loop {
        out.push(b'A' + (index % 26) as u8);
        if index < 26 {
            break;
        }
        index = index / 26 - 1;
    }
    out.reverse();
    String::from_utf8(out).unwrap()
}

/// A sheet name as a range prefix: `Notes` → `'Notes'!`.
pub fn sheet_prefix(sheet: &str) -> String {
    format!("'{}'!", sheet.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns() {
        for (letters, index) in [("A", 0), ("Z", 25), ("AA", 26), ("AZ", 51), ("BA", 52)] {
            assert_eq!(col_index(letters), Some(index));
            assert_eq!(col_letters(index), letters);
        }
        assert_eq!(col_index("e"), Some(4));
        assert_eq!(col_index("A1"), None);
        assert_eq!(sheet_prefix("Stats from Simon"), "'Stats from Simon'!");
    }
}
