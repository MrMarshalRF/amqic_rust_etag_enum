//! A closed synthetic kernel with optional transactional local disk storage.
use crate::journal::Journal;
use crate::{
    check_id, check_sha256, identity, Error, Etag, Report, Result, MAX_BYTES, MAX_MEMBERS,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Mutex,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Sha256 { text: String },
    SumI64 { values: Vec<i64> },
    IncrementCounter { amount: i64 },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    pub id: String,
    pub command: Command,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Batch {
    pub schema: String,
    pub scope: String,
    pub kernel_sha256: String,
    pub operation_id: String,
    pub expected_revision: u64,
    pub members: Vec<Member>,
}
impl Batch {
    pub fn validate(&self) -> Result<()> {
        if self.schema != "amqic.etag.kernel.v1" {
            return Err(Error::invalid("unknown kernel schema"));
        }
        check_id(&self.scope)?;
        check_id(&self.operation_id)?;
        check_sha256(&self.kernel_sha256)?;
        if self.members.is_empty() || self.members.len() > MAX_MEMBERS {
            return Err(Error::invalid("member count"));
        }
        let mut ids = BTreeSet::new();
        let mut bytes = 0usize;
        for member in &self.members {
            check_id(&member.id)?;
            if !ids.insert(&member.id) {
                return Err(Error::invalid("duplicate member id"));
            }
            bytes = bytes
                .checked_add(match &member.command {
                    Command::Sha256 { text } => text.len(),
                    Command::SumI64 { values } => values
                        .len()
                        .checked_mul(8)
                        .ok_or_else(|| Error::invalid("size overflow"))?,
                    Command::IncrementCounter { .. } => 8,
                })
                .ok_or_else(|| Error::invalid("size overflow"))?;
            if bytes > MAX_BYTES {
                return Err(Error::invalid("kernel payload byte limit"));
            }
        }
        Ok(())
    }
    pub fn etag(&self) -> Result<Etag> {
        self.validate()?;
        identity(self)
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemberResult {
    Sha256 { etag: Etag },
    SumI64 { value: i64 },
    Counter { value: i64 },
    Refused { reason: Refusal },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    IntegerOverflow,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Completed,
    Partial,
    Refused,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberOutput {
    pub id: String,
    pub result: MemberResult,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Output {
    pub operation_id: String,
    pub status: Status,
    pub revision: u64,
    pub counter: i64,
    pub members: Vec<MemberOutput>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    batch: Batch,
    report: Report<Output>,
}
/// Versioned portable snapshot for the synthetic lab's single transaction.
/// Import is intentionally restricted to in-memory sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    schema: String,
    scope: String,
    kernel_sha256: String,
    revision: u64,
    counter: i64,
    ledger: BTreeMap<String, Entry>,
}
impl Checkpoint {
    fn empty(scope: String, kernel_sha256: String) -> Self {
        Self {
            schema: "amqic.etag.kernel-checkpoint.v1".into(),
            scope,
            kernel_sha256,
            revision: 0,
            counter: 0,
            ledger: BTreeMap::new(),
        }
    }
    fn validate(&self, scope: &str, kernel_sha256: &str) -> Result<()> {
        if self.schema != "amqic.etag.kernel-checkpoint.v1"
            || self.scope != scope
            || self.kernel_sha256 != kernel_sha256
        {
            return Err(Error::StaleSnapshot);
        }
        if self.ledger.len() > MAX_MEMBERS || self.revision != self.ledger.len() as u64 {
            return Err(Error::Storage);
        }
        // Validate saved evidence without dispatching any completed command.
        // The journal checksum detects accidental content changes; this is not
        // an authentication scheme for an adversary who can rewrite the DB.
        let mut revision = 0u64;
        let mut counter = 0i64;
        let mut entries: Vec<_> = self.ledger.iter().collect();
        entries.sort_by_key(|(_, entry)| entry.report.payload.revision);
        for (key, entry) in entries {
            let batch = &entry.batch;
            let output = &entry.report.payload;
            if key != &batch.operation_id
                || key != &output.operation_id
                || batch.scope != scope
                || batch.kernel_sha256 != kernel_sha256
                || batch.expected_revision != revision
                || output.revision != revision + 1
                || output.members.len() != batch.members.len()
            {
                return Err(Error::Storage);
            }
            entry.report.verify(&batch.etag()?)?;
            let mut success = 0;
            for (member, saved) in batch.members.iter().zip(&output.members) {
                if member.id != saved.id {
                    return Err(Error::Storage);
                }
                match (&member.command, &saved.result) {
                    (Command::Sha256 { .. }, MemberResult::Sha256 { .. })
                    | (Command::SumI64 { .. }, MemberResult::SumI64 { .. }) => success += 1,
                    (
                        Command::SumI64 { .. },
                        MemberResult::Refused {
                            reason: Refusal::IntegerOverflow,
                        },
                    ) => {}
                    (Command::IncrementCounter { amount }, MemberResult::Counter { value }) => {
                        if counter.checked_add(*amount) != Some(*value) {
                            return Err(Error::Storage);
                        }
                        counter = *value;
                        success += 1;
                    }
                    (
                        Command::IncrementCounter { amount },
                        MemberResult::Refused {
                            reason: Refusal::IntegerOverflow,
                        },
                    ) => {
                        if counter.checked_add(*amount).is_some() {
                            return Err(Error::Storage);
                        }
                    }
                    _ => return Err(Error::Storage),
                }
            }
            let status = if success == output.members.len() {
                Status::Completed
            } else if success == 0 {
                Status::Refused
            } else {
                Status::Partial
            };
            if output.status != status || output.counter != counter {
                return Err(Error::Storage);
            }
            revision += 1;
        }
        if revision != self.revision || counter != self.counter {
            return Err(Error::Storage);
        }
        Ok(())
    }
}
pub struct Session {
    scope: String,
    kernel_sha256: String,
    state: Mutex<Checkpoint>,
    journal: Option<Journal>,
}
impl Session {
    pub fn new(scope: String, kernel_sha256: String) -> Result<Self> {
        check_id(&scope)?;
        check_sha256(&kernel_sha256)?;
        let state = Mutex::new(Checkpoint::empty(scope.clone(), kernel_sha256.clone()));
        Ok(Self {
            scope,
            kernel_sha256,
            state,
            journal: None,
        })
    }
    pub fn with_journal(scope: String, kernel_sha256: String, journal: Journal) -> Result<Self> {
        let mut session = Self::new(scope, kernel_sha256)?;
        let initial = session.state.lock().map_err(|_| Error::Internal)?.clone();
        journal.transact("kernel.v1", &session.scope, &initial, |state| {
            state.validate(&session.scope, &session.kernel_sha256)
        })?;
        session.journal = Some(journal);
        Ok(session)
    }
    pub fn checkpoint(&self) -> Result<Checkpoint> {
        if self.journal.is_some() {
            return Err(Error::invalid(
                "disk session checkpoint export is not supported",
            ));
        }
        Ok(self.state.lock().map_err(|_| Error::Internal)?.clone())
    }
    pub fn restore(&self, checkpoint: &Checkpoint) -> Result<()> {
        if self.journal.is_some() {
            return Err(Error::invalid(
                "disk session checkpoint import is not supported",
            ));
        }
        checkpoint.validate(&self.scope, &self.kernel_sha256)?;
        *self.state.lock().map_err(|_| Error::Internal)? = checkpoint.clone();
        Ok(())
    }
    pub fn state(&self) -> Result<(u64, i64)> {
        if let Some(journal) = &self.journal {
            let state: Checkpoint = journal
                .read("kernel.v1", &self.scope)?
                .ok_or(Error::Storage)?;
            state.validate(&self.scope, &self.kernel_sha256)?;
            return Ok((state.revision, state.counter));
        }
        let s = self.state.lock().map_err(|_| Error::Internal)?;
        Ok((s.revision, s.counter))
    }
    pub fn lookup(&self, operation_id: &str) -> Result<Option<Report<Output>>> {
        check_id(operation_id)?;
        if let Some(journal) = &self.journal {
            let state: Checkpoint = journal
                .read("kernel.v1", &self.scope)?
                .ok_or(Error::Storage)?;
            state.validate(&self.scope, &self.kernel_sha256)?;
            return Ok(state
                .ledger
                .get(operation_id)
                .map(|entry| entry.report.clone()));
        }
        Ok(self
            .state
            .lock()
            .map_err(|_| Error::Internal)?
            .ledger
            .get(operation_id)
            .map(|entry| entry.report.clone()))
    }
    /// Counter, revision and recovery report share one memory lock or disk
    /// transaction. These are synthetic local effects, not external callbacks.
    pub fn execute(&self, batch: &Batch, expected: Option<&str>) -> Result<Report<Output>> {
        let input = batch.etag()?;
        input.require(expected)?;
        if batch.scope != self.scope || batch.kernel_sha256 != self.kernel_sha256 {
            return Err(Error::StaleSnapshot);
        }
        if let Some(journal) = &self.journal {
            let mut initial = Checkpoint::empty(self.scope.clone(), self.kernel_sha256.clone());
            // Only with_journal may initialize a key. Losing an established
            // ledger must refuse instead of replaying against a fresh counter.
            initial.schema = "missing".into();
            return journal.transact("kernel.v1", &self.scope, &initial, |state| {
                state.validate(&self.scope, &self.kernel_sha256)?;
                execute_local(state, batch)
            });
        }
        let mut state = self.state.lock().map_err(|_| Error::Internal)?;
        execute_local(&mut state, batch)
    }
}
fn execute_local(state: &mut Checkpoint, batch: &Batch) -> Result<Report<Output>> {
    let input = batch.etag()?;
    if batch.scope != state.scope || batch.kernel_sha256 != state.kernel_sha256 {
        return Err(Error::StaleSnapshot);
    }
    if let Some(previous) = state.ledger.get(&batch.operation_id) {
        return if previous.report.input_etag == input {
            Ok(previous.report.clone())
        } else {
            Err(Error::OperationConflict)
        };
    }
    if state.ledger.len() >= MAX_MEMBERS {
        return Err(Error::invalid("session ledger full"));
    }
    if batch.expected_revision != state.revision {
        return Err(Error::StaleRevision);
    }
    let next_revision = state.revision.checked_add(1).ok_or(Error::Internal)?;
    let mut results = Vec::with_capacity(batch.members.len());
    let mut success = 0;
    for member in &batch.members {
        let result = match &member.command {
            Command::Sha256 { text } => Some(MemberResult::Sha256 {
                etag: Etag::of_bytes(text.as_bytes()),
            }),
            Command::SumI64 { values } => values
                .iter()
                .try_fold(0i64, |a, b| a.checked_add(*b))
                .map(|value| MemberResult::SumI64 { value }),
            Command::IncrementCounter { amount } => {
                state.counter.checked_add(*amount).map(|value| {
                    state.counter = value;
                    MemberResult::Counter { value }
                })
            }
        };
        if result.is_some() {
            success += 1;
        }
        results.push(MemberOutput {
            id: member.id.clone(),
            result: result.unwrap_or(MemberResult::Refused {
                reason: Refusal::IntegerOverflow,
            }),
        });
    }
    state.revision = next_revision;
    let status = if success == results.len() {
        Status::Completed
    } else if success == 0 {
        Status::Refused
    } else {
        Status::Partial
    };
    let report = Report::new(
        input,
        Output {
            operation_id: batch.operation_id.clone(),
            status,
            revision: state.revision,
            counter: state.counter,
            members: results,
        },
    )?;
    state.ledger.insert(
        batch.operation_id.clone(),
        Entry {
            batch: batch.clone(),
            report: report.clone(),
        },
    );
    Ok(report)
}
