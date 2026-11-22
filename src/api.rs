//! OpenAI Batch JSONL, local freshness gate, and explicit effect reconciliation.
//! No extra ETag fields are sent inside provider JSONL request records.
use crate::{canonical, check_id, identity, Etag, Error, Report, Result, MAX_BYTES, MAX_MEMBERS};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::{BTreeMap, BTreeSet}, sync::Mutex};
use crate::journal::Journal;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Endpoint {
    #[serde(rename = "/v1/responses")] Responses,
    #[serde(rename = "/v1/chat/completions")] ChatCompletions,
    #[serde(rename = "/v1/embeddings")] Embeddings,
}
impl Endpoint {
    pub fn as_str(self) -> &'static str {
        match self { Self::Responses => "/v1/responses", Self::ChatCompletions => "/v1/chat/completions", Self::Embeddings => "/v1/embeddings" }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request { pub custom_id: String, pub body: Value }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Batch {
    pub schema: String, pub scope: String, pub operation_id: String,
    pub endpoint: Endpoint, pub requests: Vec<Request>,
}
/// Prepared plans cannot be deserialized unchecked or mutated by callers.
#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    input_etag: Etag, file_etag: Etag, batch: Batch,
    #[serde(skip)] jsonl: Vec<u8>,
}
impl Batch {
    pub fn prepare(&self) -> Result<Plan> {
        if self.schema != "amqic.etag.openai-batch.v1" { return Err(Error::invalid("unknown API schema")); }
        check_id(&self.scope)?; check_id(&self.operation_id)?;
        if self.requests.is_empty() || self.requests.len() > MAX_MEMBERS { return Err(Error::invalid("API member count")); }
        let mut ids = BTreeSet::new(); let mut model: Option<&str> = None; let mut jsonl = Vec::new();
        for request in &self.requests {
            check_id(&request.custom_id)?;
            if !ids.insert(&request.custom_id) { return Err(Error::invalid("duplicate custom_id")); }
            let body = request.body.as_object().ok_or_else(|| Error::invalid("API body must be an object"))?;
            let current = body.get("model").and_then(Value::as_str).filter(|s| !s.is_empty())
                .ok_or_else(|| Error::invalid("API body requires model"))?;
            if model.is_some_and(|m| m != current) { return Err(Error::invalid("use a single model per API batch")); }
            model = Some(current);
            let line = json!({"custom_id":request.custom_id, "method":"POST", "url":self.endpoint, "body":request.body});
            jsonl.extend(canonical(&line)?); jsonl.push(b'\n');
            if jsonl.len() > MAX_BYTES { return Err(Error::invalid("API JSONL byte limit")); }
        }
        let file_etag = Etag::of_bytes(&jsonl);
        // Includes the output contract and fixed provider creation options.
        let input_etag = identity(&("amqic.etag.api-plan.v1", self, &file_etag, "24h", "custom_id-reconcile.v1"))?;
        Ok(Plan { input_etag, file_etag, batch: self.clone(), jsonl })
    }
}
impl Plan {
    pub fn etag(&self) -> &Etag { &self.input_etag }
    pub fn file_etag(&self) -> &Etag { &self.file_etag }
    pub fn jsonl(&self) -> &[u8] { &self.jsonl }
    pub fn endpoint(&self) -> Endpoint { self.batch.endpoint }
    pub fn scope(&self) -> &str { &self.batch.scope }
    pub fn operation_id(&self) -> &str { &self.batch.operation_id }
    pub fn create_body(&self, file_id: &str) -> Result<Value> {
        check_id(file_id)?;
        Ok(json!({"input_file_id":file_id, "endpoint":self.endpoint(), "completion_window":"24h",
            "metadata":{"amqic_input_etag":self.etag().as_str(), "amqic_operation_id":self.operation_id()}}))
    }
    pub fn manifest(&self) -> Value {
        json!({"schema":"amqic.etag.api-manifest.v1", "input_etag":self.etag(), "file_etag":self.file_etag(),
            "scope":self.scope(), "operation_id":self.operation_id(), "endpoint":self.endpoint(),
            "members":self.batch.requests.len(), "jsonl_bytes":self.jsonl.len()})
    }
    pub fn reconcile(&self, expected: Option<&str>, jsonl: &[u8]) -> Result<Report<ApiOutput>> {
        self.etag().require(expected)?;
        if jsonl.len() > MAX_BYTES { return Err(Error::invalid("result byte limit")); }
        let text = std::str::from_utf8(jsonl).map_err(|_| Error::invalid("result UTF-8"))?;
        let expected_ids: BTreeSet<_> = self.batch.requests.iter().map(|r| r.custom_id.as_str()).collect();
        let mut rows = BTreeMap::new();
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let row: Value = serde_json::from_str(line).map_err(|_| Error::invalid("result JSONL"))?;
            let id = row.get("custom_id").and_then(Value::as_str).ok_or_else(|| Error::invalid("result custom_id"))?.to_owned();
            if !expected_ids.contains(id.as_str()) || rows.insert(id, row).is_some() { return Err(Error::invalid("duplicate/foreign result id")); }
        }
        let mut members = Vec::new(); let mut completed = 0;
        for request in &self.batch.requests {
            let result = match rows.remove(&request.custom_id) {
                None => ApiMemberResult::Unknown,
                Some(row) => {
                    let status = row.pointer("/response/status_code").and_then(Value::as_u64);
                    if row.get("error").is_some_and(|e| !e.is_null()) || status.is_some_and(|s| !(200..300).contains(&s)) {
                        ApiMemberResult::Failed { record: row }
                    } else if status.is_some_and(|s| (200..300).contains(&s)) && row.pointer("/response/body").is_some() {
                        completed += 1; ApiMemberResult::Completed { record: row }
                    } else { return Err(Error::invalid("result requires response status/body or explicit error")); }
                }
            };
            members.push(ApiMemberOutput { custom_id: request.custom_id.clone(), result });
        }
        let status = if completed == members.len() { ApiStatus::Completed } else { ApiStatus::Partial };
        Report::new(self.etag().clone(), ApiOutput { operation_id:self.operation_id().to_owned(), status, members })
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag="status", rename_all="snake_case", deny_unknown_fields)]
pub enum ApiMemberResult { Completed { record: Value }, Failed { record: Value }, Unknown }
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ApiMemberOutput { pub custom_id: String, pub result: ApiMemberResult }
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all="snake_case")]
pub enum ApiStatus { Completed, Partial }
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ApiOutput { pub operation_id: String, pub status: ApiStatus, pub members: Vec<ApiMemberOutput> }

/// Transport must distinguish a proven zero-effect rejection from uncertainty.
/// Timeout/disconnect/5xx/malformed success MUST be Unknown, never safe-to-retry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag="outcome", rename_all="snake_case", deny_unknown_fields)]
pub enum TransportOutcome { Confirmed { id: String }, Rejected { code: u16 }, Unknown }
pub trait Transport {
    fn upload(&mut self, jsonl: &[u8]) -> TransportOutcome;
    fn create(&mut self, body: &Value) -> TransportOutcome;
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag="state", rename_all="snake_case", deny_unknown_fields)]
pub enum SubmissionState {
    Prepared, Uploaded { file_id: String }, Submitted { file_id: String, batch_id: String },
    RejectedUpload { code: u16 }, RejectedCreate { file_id: String, code: u16 },
    UnknownUpload, UnknownCreate { file_id: String },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Submission { batch: Batch, report: Report<SubmissionState> }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint { schema: String, scope: String, ledger: BTreeMap<String, Submission> }
impl Checkpoint {
    fn empty(scope: String) -> Self {
        Self { schema: "amqic.etag.api-checkpoint.v1".into(), scope, ledger: BTreeMap::new() }
    }
    fn validate(&self, scope: &str) -> Result<()> {
        if self.schema != "amqic.etag.api-checkpoint.v1" || self.scope != scope { return Err(Error::StaleSnapshot); }
        if self.ledger.len() > MAX_MEMBERS { return Err(Error::Storage); }
        for (id, submission) in &self.ledger {
            let plan = submission.batch.prepare()?;
            if id != plan.operation_id() || plan.scope() != scope { return Err(Error::Storage); }
            submission.report.verify(plan.etag())?;
            match &submission.report.payload {
                SubmissionState::Uploaded { file_id } | SubmissionState::RejectedCreate { file_id, .. }
                | SubmissionState::UnknownCreate { file_id } => check_id(file_id)?,
                SubmissionState::Submitted { file_id, batch_id } => { check_id(file_id)?; check_id(batch_id)?; },
                SubmissionState::Prepared | SubmissionState::RejectedUpload { .. } | SubmissionState::UnknownUpload => {},
            }
        }
        Ok(())
    }
}
/// Memory mode preserves the original contract. Disk mode commits intent before
/// transport and never automatically replays an uncertain external effect.
pub struct ApiSession { scope: String, ledger: Mutex<Checkpoint>, journal: Option<Journal> }
enum Action { None(Report<SubmissionState>), Upload, Create { file_id: String, body: Value } }
impl ApiSession {
    pub fn new(scope: String) -> Result<Self> {
        check_id(&scope)?; Ok(Self { ledger: Mutex::new(Checkpoint::empty(scope.clone())), scope, journal: None })
    }
    pub fn with_journal(scope: String, journal: Journal) -> Result<Self> {
        let mut session = Self::new(scope)?;
        let initial = Checkpoint::empty(session.scope.clone());
        journal.transact("api.v1", &session.scope, &initial, |state| state.validate(&session.scope))?;
        session.journal = Some(journal); Ok(session)
    }
    pub fn checkpoint(&self) -> Result<Checkpoint> {
        if self.journal.is_some() { return Err(Error::invalid("disk session checkpoint export is not supported")); }
        Ok(self.ledger.lock().map_err(|_| Error::Internal)?.clone())
    }
    pub fn restore(&self, checkpoint: &Checkpoint) -> Result<()> {
        if self.journal.is_some() { return Err(Error::invalid("disk session checkpoint import is not supported")); }
        checkpoint.validate(&self.scope)?;
        *self.ledger.lock().map_err(|_| Error::Internal)? = checkpoint.clone(); Ok(())
    }
    pub fn lookup(&self, operation_id: &str) -> Result<Option<Report<SubmissionState>>> {
        check_id(operation_id)?;
        if let Some(journal) = &self.journal {
            let state: Checkpoint = journal.read("api.v1", &self.scope)?.ok_or(Error::Storage)?;
            state.validate(&self.scope)?;
            return Ok(state.ledger.get(operation_id).map(|s| s.report.clone()));
        }
        Ok(self.ledger.lock().map_err(|_| Error::Internal)?.ledger.get(operation_id).map(|s| s.report.clone()))
    }
    /// Exactly one effect per call; persist/inspect the returned transition.
    pub fn advance(&self, plan: &Plan, expected: Option<&str>, transport: &mut impl Transport) -> Result<Report<SubmissionState>> {
        plan.etag().require(expected)?;
        if plan.scope() != self.scope { return Err(Error::StaleSnapshot); }
        if let Some(journal) = &self.journal {
            let mut initial = Checkpoint::empty(self.scope.clone());
            initial.schema = "missing".into(); // initialization is constructor-only
            let action = journal.transact("api.v1", &self.scope, &initial, |state| {
                state.validate(&self.scope)?; admit(state, plan)
            })?;
            if let Action::None(report) = action { return Ok(report); }
            // The Unknown marker above is durably committed and the transaction
            // is released BEFORE entering this external callback.
            let outcome = perform(&action, plan, transport);
            return journal.transact("api.v1", &self.scope, &initial, |state| {
                state.validate(&self.scope)?; complete(state, plan, &action, outcome)
            });
        }
        let mut state = self.ledger.lock().map_err(|_| Error::Internal)?;
        let action = admit(&mut state, plan)?;
        if let Action::None(report) = action { return Ok(report); }
        let outcome = perform(&action, plan, transport);
        complete(&mut state, plan, &action, outcome)
    }
}

fn admit(state: &mut Checkpoint, plan: &Plan) -> Result<Action> {
    if !state.ledger.contains_key(plan.operation_id()) && state.ledger.len() >= MAX_MEMBERS {
        return Err(Error::invalid("session ledger full; reconcile before a new session"));
    }
    let submission = state.ledger.entry(plan.operation_id().to_owned()).or_insert(Submission {
        batch: plan.batch.clone(), report: Report::new(plan.etag().clone(), SubmissionState::Prepared)?,
    });
    if submission.report.input_etag != *plan.etag() { return Err(Error::OperationConflict); }
    let (action, intent) = match submission.report.payload.clone() {
        SubmissionState::Prepared => (Action::Upload, SubmissionState::UnknownUpload),
        SubmissionState::Uploaded { file_id } => {
            let body = plan.create_body(&file_id)?;
            (Action::Create { file_id: file_id.clone(), body }, SubmissionState::UnknownCreate { file_id })
        },
        SubmissionState::Submitted { .. } | SubmissionState::RejectedUpload { .. }
        | SubmissionState::RejectedCreate { .. } | SubmissionState::UnknownUpload
        | SubmissionState::UnknownCreate { .. } => return Ok(Action::None(submission.report.clone())),
    };
    submission.report = Report::new(plan.etag().clone(), intent)?;
    Ok(action)
}
fn perform(action: &Action, plan: &Plan, transport: &mut impl Transport) -> TransportOutcome {
    match action {
        Action::Upload => transport.upload(plan.jsonl()),
        Action::Create { body, .. } => transport.create(body),
        Action::None(_) => unreachable!("terminal reports do not call transport"),
    }
}
fn complete(state: &mut Checkpoint, plan: &Plan, action: &Action, outcome: TransportOutcome) -> Result<Report<SubmissionState>> {
    let submission = state.ledger.get_mut(plan.operation_id()).ok_or(Error::Storage)?;
    if submission.report.input_etag != *plan.etag() { return Err(Error::OperationConflict); }
    let next = match action {
        Action::Upload => {
            if submission.report.payload != SubmissionState::UnknownUpload { return Err(Error::Storage); }
            match outcome {
                TransportOutcome::Confirmed { id } if check_id(&id).is_ok() => SubmissionState::Uploaded { file_id: id },
                TransportOutcome::Rejected { code } => SubmissionState::RejectedUpload { code },
                TransportOutcome::Confirmed { .. } | TransportOutcome::Unknown => SubmissionState::UnknownUpload,
            }
        },
        Action::Create { file_id, .. } => {
            if submission.report.payload != (SubmissionState::UnknownCreate { file_id: file_id.clone() }) { return Err(Error::Storage); }
            match outcome {
                TransportOutcome::Confirmed { id } if check_id(&id).is_ok() => SubmissionState::Submitted { file_id: file_id.clone(), batch_id: id },
                TransportOutcome::Rejected { code } => SubmissionState::RejectedCreate { file_id: file_id.clone(), code },
                TransportOutcome::Confirmed { .. } | TransportOutcome::Unknown => SubmissionState::UnknownCreate { file_id: file_id.clone() },
            }
        },
        Action::None(report) => return Ok(report.clone()),
    };
    submission.report = Report::new(plan.etag().clone(), next)?;
    Ok(submission.report.clone())
}
