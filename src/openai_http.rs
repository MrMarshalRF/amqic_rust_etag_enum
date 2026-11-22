//! Optional real HTTPS transport. Caller supplies its reviewed authenticated
//! authorization headers. No credentials in descriptors, arguments or reports.
use crate::api::{Transport, TransportOutcome};
use crate::{Error, Result, MAX_BYTES};
use reqwest::blocking::{multipart, Client, Response};
use reqwest::header::{HeaderMap, AUTHORIZATION};
use serde_json::Value;
use std::{io::Read, time::Duration};

pub struct OpenAiHttp {
    client: Client,
}
impl OpenAiHttp {
    /// Headers must come from the host's protected credential consumer. Never
    /// paste keys into model chat or pass them as executable arguments.
    pub fn new(mut headers: HeaderMap) -> Result<Self> {
        if !headers.contains_key(AUTHORIZATION) {
            return Err(Error::invalid("host authorization header required"));
        }
        for value in headers.values_mut() {
            value.set_sensitive(true);
        }
        let client = Client::builder()
            .default_headers(headers)
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|_| Error::Internal)?;
        Ok(Self { client })
    }
    fn bytes(response: Response) -> Result<Vec<u8>> {
        if !response.status().is_success() {
            return Err(Error::invalid("provider read not successful"));
        }
        let mut bytes = Vec::new();
        response
            .take((MAX_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::invalid("provider read incomplete"))?;
        if bytes.len() > MAX_BYTES {
            return Err(Error::invalid("provider response byte limit"));
        }
        Ok(bytes)
    }
    fn provider_id(id: &str) -> Result<()> {
        if id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(Error::invalid("provider ID characters"));
        }
        Ok(())
    }
    /// Read-only recovery when the original batch ID is known; no resubmission.
    pub fn retrieve(&self, batch_id: &str) -> Result<Value> {
        Self::provider_id(batch_id)?;
        let response = self
            .client
            .get(format!("https://api.openai.com/v1/batches/{batch_id}"))
            .send()
            .map_err(|_| Error::invalid("provider read unavailable"))?;
        let value: Value = serde_json::from_slice(&Self::bytes(response)?)
            .map_err(|_| Error::invalid("provider JSON"))?;
        if value.get("id").and_then(Value::as_str) != Some(batch_id) {
            return Err(Error::StaleSnapshot);
        }
        Ok(value)
    }
    /// Download output_file_id/error_file_id, then reconcile by custom_id.
    pub fn file_content(&self, file_id: &str) -> Result<Vec<u8>> {
        Self::provider_id(file_id)?;
        let response = self
            .client
            .get(format!("https://api.openai.com/v1/files/{file_id}/content"))
            .send()
            .map_err(|_| Error::invalid("provider read unavailable"))?;
        Self::bytes(response)
    }
    fn outcome(response: std::result::Result<Response, reqwest::Error>) -> TransportOutcome {
        let Ok(response) = response else {
            return TransportOutcome::Unknown;
        };
        let code = response.status().as_u16();
        // Even non-2xx is conservative Unknown: HTTP error alone is not a
        // provider guarantee that no resource was created. Never retry here.
        if !(200..300).contains(&code) {
            return TransportOutcome::Unknown;
        }
        let Ok(bytes) = Self::bytes(response) else {
            return TransportOutcome::Unknown;
        };
        let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
            return TransportOutcome::Unknown;
        };
        match value.get("id").and_then(Value::as_str) {
            Some(id) => TransportOutcome::Confirmed { id: id.to_owned() },
            None => TransportOutcome::Unknown,
        }
    }
}
impl Transport for OpenAiHttp {
    fn upload(&mut self, jsonl: &[u8]) -> TransportOutcome {
        let file = multipart::Part::bytes(jsonl.to_vec()).file_name("batch.jsonl");
        let form = multipart::Form::new()
            .text("purpose", "batch")
            .part("file", file);
        Self::outcome(
            self.client
                .post("https://api.openai.com/v1/files")
                .multipart(form)
                .send(),
        )
    }
    fn create(&mut self, body: &Value) -> TransportOutcome {
        Self::outcome(
            self.client
                .post("https://api.openai.com/v1/batches")
                .json(body)
                .send(),
        )
    }
}
