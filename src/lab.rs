//! Purpose-built synthetic loopback laboratory, not a production HTTP server.
//! ETag, admission capacity, operation recovery and stream cursors are distinct.
use crate::{api, kernel, journal::Journal, identity, Etag, Error, Report, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, VecDeque},
    io::{self, Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    sync::{Arc, Mutex, atomic::{AtomicBool, AtomicUsize, Ordering}},
    thread, time::{Duration, Instant},
};

pub const MAX_BODY: usize = 65_536;
pub const MAX_CONNECTIONS: usize = 8;
pub const MAX_STREAMS: usize = 2;
pub const MAX_OPERATIONS: usize = 128;
pub const MAX_EVENTS: usize = 32;
pub const MAX_LAB_MEMBERS: usize = 16;
const REQUEST_DEADLINE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all="snake_case")]
pub enum Fault { None, DropAfterCommit }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabBatch { pub schema: String, pub batch: kernel::Batch, pub fault: Fault }
impl LabBatch {
    pub fn etag(&self) -> Result<Etag> {
        if self.schema!="amqic.etag.lab.v1" || self.batch.scope!="lab" { return Err(Error::invalid("lab schema/scope")); }
        if self.batch.members.len()>MAX_LAB_MEMBERS { return Err(Error::invalid("lab member limit")); }
        self.batch.validate()?; identity(self)
    }
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all="snake_case")]
pub enum ProviderFault { None, UnknownUpload, UnknownCreate }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderBatch { pub schema:String, pub batch:api::Batch, pub fault:ProviderFault }
impl ProviderBatch {
    pub fn etag(&self)->Result<Etag> {
        if self.schema!="amqic.etag.lab-provider.v1" || self.batch.scope!="lab-api" || self.batch.requests.len()>MAX_LAB_MEMBERS {
            return Err(Error::invalid("lab provider schema/scope/member limit"));
        }
        self.batch.prepare()?; identity(self)
    }
}
#[derive(Debug, Clone, Serialize)]
struct FakeProvider {
    upload_calls:usize, create_calls:usize, file_id:Option<String>, batch_id:Option<String>,
    #[serde(skip)] fault:Option<ProviderFault>,
    #[serde(skip)] id_suffix:String,
}
impl FakeProvider {
    fn new(fault:ProviderFault,operation_id:&str)->Self {
        let tag=Etag::of_bytes(operation_id.as_bytes());
        Self { upload_calls:0,create_calls:0,file_id:None,batch_id:None,fault:Some(fault),id_suffix:tag.as_str()[8..72].to_owned() }
    }
}
impl api::Transport for FakeProvider {
    fn upload(&mut self,_bytes:&[u8])->api::TransportOutcome {
        self.upload_calls+=1;self.file_id=Some(format!("file-synthetic-{}",self.id_suffix));
        if self.fault==Some(ProviderFault::UnknownUpload) {api::TransportOutcome::Unknown}
        else {api::TransportOutcome::Confirmed{id:self.file_id.clone().unwrap_or_default()}}
    }
    fn create(&mut self,_body:&Value)->api::TransportOutcome {
        self.create_calls+=1;self.batch_id=Some(format!("batch-synthetic-{}",self.id_suffix));
        if self.fault==Some(ProviderFault::UnknownCreate) {api::TransportOutcome::Unknown}
        else {api::TransportOutcome::Confirmed{id:self.batch_id.clone().unwrap_or_default()}}
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KernelRecord { input:Etag, report:Report<kernel::Output> }
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderRecord { input:Etag, report:Report<Value> }
#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Book {
    operations:BTreeMap<String,KernelRecord>, providers:BTreeMap<String,ProviderRecord>,
    executions:usize, event_id:u64, events:VecDeque<Value>, rejected:usize,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LabCheckpoint {
    schema:String, kernel:kernel::Checkpoint, provider:api::Checkpoint, book:Book,
}
pub struct Lab {
    kernel_sha256:String, kernel:kernel::Session, provider:api::ApiSession, book:Mutex<Book>,
    active:AtomicUsize, peak:AtomicUsize, rejected_connections:AtomicUsize,
    streams:AtomicUsize, rejected_streams:AtomicUsize,
    journal:Option<Journal>, durable_gate:Mutex<()>,
}
pub struct LabResponse { pub status:u16, pub body:Vec<u8>, pub drop_connection:bool }
impl LabResponse {
    fn json(status:u16,value:Value)->Self {Self {status,body:value.to_string().into_bytes(),drop_connection:false}}
    fn ok(value:Value)->Self {Self::json(200,json!({"ok":true,"result":value}))}
    fn error(status:u16,code:&'static str)->Self {Self::json(status,json!({"ok":false,"http_status":status,"error":{"code":code}}))}
    fn core(error:Error)->Self {Self::json(error.http_status(),json!({"ok":false,"http_status":error.http_status(),"error":error}))}
}
impl Lab {
    pub fn new(kernel_sha256:String)->Result<Self> {
        Ok(Self { kernel:kernel::Session::new("lab".into(),kernel_sha256.clone())?,kernel_sha256,
            provider:api::ApiSession::new("lab-api".into())?,book:Mutex::new(Book::default()),
            active:AtomicUsize::new(0),peak:AtomicUsize::new(0),rejected_connections:AtomicUsize::new(0),
            streams:AtomicUsize::new(0),rejected_streams:AtomicUsize::new(0),journal:None,durable_gate:Mutex::new(()) })
    }
    pub fn open(kernel_sha256:String,path:impl AsRef<std::path::Path>)->Result<Self> {
        Self::with_journal(kernel_sha256,Journal::open(path)?)
    }
    pub fn with_journal(kernel_sha256:String,journal:Journal)->Result<Self> {
        let mut lab=Self::new(kernel_sha256)?;
        lab.journal=Some(journal);
        // Fail before the caller advertises a listening service on incompatible data.
        lab.initialize_journal()?;
        Ok(lab)
    }
    fn checkpoint(&self)->Result<LabCheckpoint> {
        Ok(LabCheckpoint{schema:"amqic.etag.lab-checkpoint.v1".into(),kernel:self.kernel.checkpoint()?,
            provider:self.provider.checkpoint()?,book:self.book.lock().map_err(|_|Error::Internal)?.clone()})
    }
    fn restore(&self,saved:&LabCheckpoint)->Result<()> {
        if saved.schema!="amqic.etag.lab-checkpoint.v1" || saved.book.operations.len()>MAX_OPERATIONS
            || saved.book.providers.len()>MAX_OPERATIONS || saved.book.events.len()>MAX_EVENTS {return Err(Error::Storage);}
        self.kernel.restore(&saved.kernel)?;self.provider.restore(&saved.provider)?;
        let(revision,counter)=self.kernel.state()?;
        if saved.book.executions!=saved.book.operations.len() || revision!=saved.book.executions as u64
            || saved.book.event_id!=revision || saved.book.events.len()!=saved.book.executions.min(MAX_EVENTS) {return Err(Error::Storage);}
        let mut revisions=std::collections::BTreeSet::new();
        for(id,record) in &saved.book.operations {
            record.report.verify(&record.input).map_err(|_|Error::Storage)?;
            let inner=self.kernel.lookup(id)?.ok_or(Error::Storage)?;
            if inner.payload!=record.report.payload || record.report.payload.operation_id!=*id
                || record.report.payload.revision==0 || record.report.payload.revision>revision
                || !revisions.insert(record.report.payload.revision) {return Err(Error::Storage);}
            if record.report.payload.revision==revision && record.report.payload.counter!=counter {return Err(Error::Storage);}
        }
        for(id,record) in &saved.book.providers {
            record.report.verify(&record.input).map_err(|_|Error::Storage)?;
            let submitted:Report<api::SubmissionState>=serde_json::from_value(record.report.payload["submission"].clone()).map_err(|_|Error::Storage)?;
            submitted.verify(&submitted.input_etag).map_err(|_|Error::Storage)?;
            if self.provider.lookup(id)?!=Some(submitted.clone()) || record.report.payload["synthetic"]!=true {return Err(Error::Storage);}
            let observation=&record.report.payload["provider_observation"];
            let valid=match submitted.payload {
                api::SubmissionState::Submitted{file_id,batch_id}=>observation["upload_calls"]==1&&observation["create_calls"]==1
                    &&observation["file_id"]==file_id&&observation["batch_id"]==batch_id,
                api::SubmissionState::UnknownUpload=>observation["upload_calls"]==1&&observation["create_calls"]==0
                    &&observation["file_id"].is_string()&&observation["batch_id"].is_null(),
                api::SubmissionState::UnknownCreate{file_id}=>observation["upload_calls"]==1&&observation["create_calls"]==1
                    &&observation["file_id"]==file_id&&observation["batch_id"].is_string(),
                api::SubmissionState::Prepared|api::SubmissionState::Uploaded{..}|api::SubmissionState::RejectedUpload{..}
                    |api::SubmissionState::RejectedCreate{..}=>false,
            };
            if !valid{return Err(Error::Storage);}
        }
        let start=saved.book.event_id.saturating_sub(saved.book.events.len() as u64)+1;
        for(index,event) in saved.book.events.iter().enumerate() {
            let id=event["operation_id"].as_str().ok_or(Error::Storage)?;
            let record=saved.book.operations.get(id).ok_or(Error::Storage)?;
            let sequence=start+index as u64;
            if event["id"]!=sequence || event["revision"]!=sequence || record.report.payload.revision!=sequence
                || event["input_etag"]!=json!(record.input) || event["etag"]!=json!(record.report.etag) {return Err(Error::Storage);}
        }
        *self.book.lock().map_err(|_|Error::Internal)?=saved.book.clone();
        Ok(())
    }
    fn initialize_journal(&self)->Result<()> {
        let Some(journal)=&self.journal else{return Ok(())};
        let _gate=self.durable_gate.lock().map_err(|_|Error::Internal)?;
        let initial=self.checkpoint()?;
        journal.transact("lab","lab",&initial,|saved|self.restore(saved))
    }
    fn events_snapshot(&self,cursor:u64)->Result<(Vec<Value>,u64,u64)> {
        let capture=||{
            let book=self.book.lock().map_err(|_|Error::Internal)?;
            Ok((book.events.iter().filter(|e|e["id"].as_u64().is_some_and(|id|id>cursor)).cloned().collect(),
                book.event_id,book.events.front().and_then(|e|e["id"].as_u64()).unwrap_or(1)))
        };
        let Some(journal)=&self.journal else{return capture()};
        let _gate=self.durable_gate.lock().map_err(|_|Error::Internal)?;
        let mut missing=self.checkpoint()?;missing.schema="missing".into();
        // Capture while the same gate/transaction still owns the restored state.
        // Return owned bytes only after successful COMMIT, before any network send.
        journal.transact("lab","lab",&missing,|saved|{self.restore(saved)?;capture()})
    }
    pub fn handle(&self,method:&str,path:&str,if_match:Option<&str>,body:&[u8])->LabResponse {
        if body.len()>MAX_BODY{return LabResponse::error(413,"body_limit");}
        let Some(journal)=&self.journal else{return self.handle_memory(method,path,if_match,body)};
        if method=="GET"&&path=="/api/lab/hold" {return self.handle_memory(method,path,if_match,body);}
        let Ok(_gate)=self.durable_gate.lock() else{return LabResponse::core(Error::Internal)};
        let mut initial=match self.checkpoint(){Ok(v)=>v,Err(e)=>return LabResponse::core(e)};
        initial.schema="missing".into(); // only construction may initialize a namespace
        // All effects in this closure are synthetic state captured by the same DB
        // transaction. External API calls must instead use durable ApiSession.
        match journal.transact("lab","lab",&initial,|saved|{
            self.restore(saved)?;
            let response=self.handle_memory(method,path,if_match,body);
            *saved=self.checkpoint()?;
            Ok(response)
        }) {
            Ok(response)=>response,
            Err(error)=>LabResponse::core(error), // no success before commit; next call reloads committed state
        }
    }
    fn handle_memory(&self,method:&str,path:&str,if_match:Option<&str>,body:&[u8])->LabResponse {
        if body.len()>MAX_BODY {return LabResponse::error(413,"body_limit");}
        if method=="GET" && path=="/api/lab/info" {
            let Ok(book)=self.book.lock() else {return LabResponse::core(Error::Internal)};
            let Ok((revision,counter))=self.kernel.state() else {return LabResponse::core(Error::Internal)};
            let mut info=json!({"schema":"amqic.etag.lab-info.v1","synthetic":true,"kernel_sha256":self.kernel_sha256,
                "revision":revision,"counter":counter,"executions":book.executions,"operations":book.operations.len(),
                "provider_operations":book.providers.len(),"rejected":book.rejected,"max_operations":MAX_OPERATIONS,
                "active_connections":self.active.load(Ordering::SeqCst),"peak_connections":self.peak.load(Ordering::SeqCst),
                "rejected_connections":self.rejected_connections.load(Ordering::SeqCst),"max_connections":MAX_CONNECTIONS,
                "stream_active":self.streams.load(Ordering::SeqCst),"rejected_streams":self.rejected_streams.load(Ordering::SeqCst),
                "max_streams":MAX_STREAMS,"event_id":book.event_id,"event_retention":MAX_EVENTS,
                "max_body_bytes":MAX_BODY,"max_members":MAX_LAB_MEMBERS,"request_deadline_ms":2000,
                "durability":if self.journal.is_some(){"sqlite-full-sync; same-binary process restart"}else{"process-local; restart loses ledger"},
                "journal_enabled":self.journal.is_some(),"upstream_calls":0});
            if let Ok(tag)=identity(&info) {info["etag"]=json!(tag);}
            return LabResponse::ok(info);
        }
        if method=="GET" && path=="/health" {return LabResponse::ok(json!({"synthetic":true,"status":"ready"}));}
        if method=="GET" && path=="/api/lab/hold" {
            thread::sleep(Duration::from_millis(500));return LabResponse::ok(json!({"held_ms":500,"effects":0}));
        }
        if method=="GET" {
            if let Some(id)=path.strip_prefix("/api/lab/operations/") {
                let Ok(book)=self.book.lock() else {return LabResponse::core(Error::Internal)};
                return match book.operations.get(id) {
                    Some(record)=>LabResponse::ok(json!(record.report)),None=>LabResponse::error(404,"operation_not_found"),
                };
            }
            if let Some(id)=path.strip_prefix("/api/lab/provider/operations/") {
                let Ok(book)=self.book.lock() else {return LabResponse::core(Error::Internal)};
                return match book.providers.get(id) {
                    Some(record)=>LabResponse::ok(json!(record.report)),None=>LabResponse::error(404,"operation_not_found"),
                };
            }
        }
        if method!="POST" {return LabResponse::error(404,"route_not_found");}
        if path=="/api/lab/plan" || path=="/api/lab/run" {
            let descriptor:LabBatch=match serde_json::from_slice(body) {Ok(v)=>v,Err(_)=>return LabResponse::error(400,"malformed_batch")};
            let input=match descriptor.etag(){Ok(v)=>v,Err(e)=>return LabResponse::core(e)};
            if descriptor.batch.kernel_sha256!=self.kernel_sha256 {return LabResponse::core(Error::StaleSnapshot);}
            if path.ends_with("/plan") {return LabResponse::ok(json!({"input_etag":input,"members":descriptor.batch.members.len(),"synthetic":true}));}
            let Ok(mut book)=self.book.lock() else {return LabResponse::core(Error::Internal)};
            if let Err(e)=input.require(if_match){book.rejected+=1;return LabResponse::core(e);}
            if let Some(record)=book.operations.get(&descriptor.batch.operation_id) {
                return if record.input==input {LabResponse::ok(json!(record.report))} else {LabResponse::core(Error::OperationConflict)};
            }
            if book.operations.len()>=MAX_OPERATIONS {return LabResponse::error(503,"operation_capacity");}
            let core_tag=match descriptor.batch.etag(){Ok(v)=>v,Err(e)=>return LabResponse::core(e)};
            let inner=match self.kernel.execute(&descriptor.batch,Some(core_tag.as_str())) {
                Ok(v)=>v,Err(e)=>{book.rejected+=1;return LabResponse::core(e)},
            };
            let report=match Report::new(input.clone(),inner.payload){Ok(v)=>v,Err(e)=>return LabResponse::core(e)};
            book.executions+=1;book.event_id+=1;
            let event=json!({"id":book.event_id,"operation_id":descriptor.batch.operation_id,"input_etag":input,"etag":report.etag,"revision":report.payload.revision});
            book.events.push_back(event);if book.events.len()>MAX_EVENTS {book.events.pop_front();}
            book.operations.insert(descriptor.batch.operation_id.clone(),KernelRecord{input,report:report.clone()});
            let mut response=LabResponse::ok(json!(report));
            response.drop_connection=descriptor.fault==Fault::DropAfterCommit;
            return response;
        }
        if path=="/api/lab/provider/plan" || path=="/api/lab/provider/run" {
            let descriptor:ProviderBatch=match serde_json::from_slice(body){Ok(v)=>v,Err(_)=>return LabResponse::error(400,"malformed_provider_batch")};
            let input=match descriptor.etag(){Ok(v)=>v,Err(e)=>return LabResponse::core(e)};
            if path.ends_with("/plan") {return LabResponse::ok(json!({"input_etag":input,"synthetic":true,"provider":"fake, no network"}));}
            let Ok(mut book)=self.book.lock() else {return LabResponse::core(Error::Internal)};
            if let Err(e)=input.require(if_match){book.rejected+=1;return LabResponse::core(e);}
            if let Some(record)=book.providers.get(&descriptor.batch.operation_id) {
                return if record.input==input {LabResponse::ok(json!(record.report))} else {LabResponse::core(Error::OperationConflict)};
            }
            if book.providers.len()>=MAX_OPERATIONS {return LabResponse::error(503,"operation_capacity");}
            let plan=match descriptor.batch.prepare(){Ok(v)=>v,Err(e)=>return LabResponse::core(e)};
            let mut transport=FakeProvider::new(descriptor.fault,&descriptor.batch.operation_id);
            let mut submission=match self.provider.advance(&plan,Some(plan.etag().as_str()),&mut transport){Ok(v)=>v,Err(e)=>return LabResponse::core(e)};
            if matches!(submission.payload,api::SubmissionState::Uploaded{..}) {
                submission=match self.provider.advance(&plan,Some(plan.etag().as_str()),&mut transport){Ok(v)=>v,Err(e)=>return LabResponse::core(e)};
            }
            let payload=json!({"synthetic":true,"submission":submission,"provider_observation":transport,
                "explanation":"provider observation is retained after lost response; no automated replay or ApiSession rebind"});
            let report=match Report::new(input.clone(),payload){Ok(v)=>v,Err(e)=>return LabResponse::core(e)};
            book.providers.insert(descriptor.batch.operation_id,ProviderRecord{input,report:report.clone()});
            return LabResponse::ok(json!(report));
        }
        LabResponse::error(404,"route_not_found")
    }
}

struct Permit<'a> { count:&'a AtomicUsize }
impl Drop for Permit<'_> {fn drop(&mut self){self.count.fetch_sub(1,Ordering::SeqCst);}}
fn increment_below(count:&AtomicUsize,limit:usize)->Option<usize> {
    let mut current=count.load(Ordering::SeqCst);
    loop {
        if current>=limit{return None;}
        match count.compare_exchange_weak(current,current+1,Ordering::SeqCst,Ordering::SeqCst) {
            Ok(_)=>return Some(current),Err(observed)=>current=observed,
        }
    }
}
fn acquire(count:&AtomicUsize,limit:usize)->Option<Permit<'_>> {
    increment_below(count,limit)?;
    Some(Permit{count})
}
fn send(stream:&mut TcpStream,response:LabResponse,content_type:&str)->io::Result<()> {
    if response.drop_connection {return stream.shutdown(Shutdown::Both);}
    let wire_etag=Etag::of_bytes(&response.body);
    let reason=match response.status {200=>"OK",400=>"Bad Request",403=>"Forbidden",404=>"Not Found",408=>"Request Timeout",409=>"Conflict",412=>"Precondition Failed",413=>"Content Too Large",428=>"Precondition Required",503=>"Service Unavailable",_=>"Error"};
    write!(stream,"HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nETag: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; img-src 'self' data:; base-uri 'none'; frame-ancestors 'none'\r\n\r\n",response.status,reason,content_type,response.body.len(),wire_etag)?;
    stream.write_all(&response.body)?;stream.flush()
}
fn header_error(stream:&mut TcpStream,status:u16,code:&'static str)->io::Result<()> {send(stream,LabResponse::error(status,code),"application/json; charset=utf-8")}
fn serve_connection(mut stream:TcpStream,lab:&Lab,port:u16)->io::Result<()> {
    // Accepted sockets can inherit listener nonblocking mode on Windows.
    stream.set_nonblocking(false)?;
    let deadline=Instant::now()+REQUEST_DEADLINE;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    let mut header=Vec::new();
    loop {
        let Some(remaining)=deadline.checked_duration_since(Instant::now()) else {return header_error(&mut stream,408,"request_deadline")};
        stream.set_read_timeout(Some(remaining))?;
        let mut b=[0u8;1];
        match stream.read(&mut b){Ok(1)=>header.push(b[0]),Ok(_)=>return Ok(()),Err(_)=>return header_error(&mut stream,408,"request_deadline")};
        if header.len()>8192 {return header_error(&mut stream,413,"header_limit");}
        if header.ends_with(b"\r\n\r\n") {break;}
    }
    let Ok(text)=std::str::from_utf8(&header) else {return header_error(&mut stream,400,"header_utf8")};
    let mut lines=text.split("\r\n");let first=lines.next().unwrap_or_default();let parts:Vec<_>=first.split(' ').collect();
    if parts.len()!=3 || parts[2]!="HTTP/1.1" || !parts[1].starts_with('/') || parts[1].len()>2048 {return header_error(&mut stream,400,"request_line");}
    let method=parts[0].to_owned();let path=parts[1].to_owned();let mut headers=BTreeMap::new();
    for line in lines.filter(|l|!l.is_empty()) {
        let Some((name,value))=line.split_once(':') else {return header_error(&mut stream,400,"header_syntax")};
        let key=name.to_ascii_lowercase();
        if key.is_empty() || !key.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'-') || headers.insert(key,value.trim().to_owned()).is_some() {
            return header_error(&mut stream,400,"duplicate_or_bad_header");
        }
    }
    let host=format!("127.0.0.1:{port}");let localhost=format!("localhost:{port}");
    if !headers.get("host").is_some_and(|h|h==&host||h==&localhost){return header_error(&mut stream,403,"host_not_loopback");}
    if headers.get("origin").is_some_and(|o|o!=&format!("http://{host}")&&o!=&format!("http://{localhost}")) {return header_error(&mut stream,403,"foreign_origin");}
    if headers.contains_key("transfer-encoding") {return header_error(&mut stream,400,"transfer_encoding_unsupported");}
    let length=match headers.get("content-length") {Some(s)=>match s.parse::<usize>(){Ok(n)=>n,Err(_)=>return header_error(&mut stream,400,"content_length")},None=>0};
    if length>MAX_BODY {return header_error(&mut stream,413,"body_limit");}
    let mut body=vec![0u8;length];let mut read=0;
    while read<length {
        let Some(remaining)=deadline.checked_duration_since(Instant::now()) else{return header_error(&mut stream,408,"request_deadline")};
        stream.set_read_timeout(Some(remaining))?;
        match stream.read(&mut body[read..]){Ok(0)=>return Ok(()),Ok(n)=>read+=n,Err(_)=>return header_error(&mut stream,408,"request_deadline")};
    }
    if method=="GET" && (path=="/api/lab/events" || path=="/api/lab/events?drop_after=1") {
        let Some(_permit)=acquire(&lab.streams,MAX_STREAMS) else {lab.rejected_streams.fetch_add(1,Ordering::SeqCst);return header_error(&mut stream,503,"stream_capacity")};
        let cursor=match headers.get("last-event-id"){Some(s)=>match s.parse::<u64>(){Ok(n)=>n,Err(_)=>return header_error(&mut stream,400,"event_cursor")},None=>0};
        let(events,last,first)=match lab.events_snapshot(cursor){Ok(v)=>v,Err(error)=>return send(&mut stream,LabResponse::core(error),"application/json; charset=utf-8")};
        if cursor>last {return header_error(&mut stream,400,"future_event_cursor");}
        if cursor>0 && cursor<first.saturating_sub(1) {return header_error(&mut stream,409,"event_gap_resync_required");}
        let chunks:Vec<_>=events.iter().map(|event|format!("id: {}\nevent: batch\ndata: {}\n\n",event["id"],event)).collect();
        let complete=format!("{}{}",chunks.concat(),": heartbeat\n\n".repeat(10));
        let wire_etag=Etag::of_bytes(complete.as_bytes());
        write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nETag: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",complete.len(),wire_etag)?;
        for chunk in chunks {
            stream.write_all(chunk.as_bytes())?;stream.flush()?;
            if path=="/api/lab/events?drop_after=1" {return stream.shutdown(Shutdown::Both);}
        }
        for _ in 0..10 {stream.write_all(b": heartbeat\n\n")?;stream.flush()?;thread::sleep(Duration::from_millis(50));}
        return Ok(());
    }
    if method=="GET" && path=="/" {
        return send(&mut stream,LabResponse::ok(json!({"lab":"synthetic API laboratory","info":"/api/lab/info",
            "scenarios":"cargo run --release --bin lab-scenarios -- 127.0.0.1:<port>",
            "interfaces":["/api/lab/plan","/api/lab/run","/api/lab/operations/{id}","/api/lab/provider/plan","/api/lab/provider/run","/api/lab/provider/operations/{id}","/api/lab/events"]})),"application/json; charset=utf-8");
    }
    send(&mut stream,lab.handle(&method,&path,headers.get("if-match").map(String::as_str),&body),"application/json; charset=utf-8")
}
/// Bounded eight-worker synthetic server. Stop flag drains owned workers.
pub fn serve(listener:TcpListener,lab:Arc<Lab>,shutdown:Arc<AtomicBool>)->io::Result<()> {
    if !listener.local_addr()?.ip().is_loopback(){return Err(io::Error::new(io::ErrorKind::PermissionDenied,"loopback only"));}
    let port=listener.local_addr()?.port();listener.set_nonblocking(true)?;
    let mut workers:Vec<thread::JoinHandle<()>>=Vec::new();
    while !shutdown.load(Ordering::SeqCst) {
        while let Some(i)=workers.iter().position(thread::JoinHandle::is_finished) {let _=workers.swap_remove(i).join();}
        match listener.accept(){
            Ok((mut stream,_))=>{
                let previous=increment_below(&lab.active,MAX_CONNECTIONS);
                if previous.is_none(){lab.rejected_connections.fetch_add(1,Ordering::SeqCst);let _=stream.set_write_timeout(Some(Duration::from_millis(100)));let _=header_error(&mut stream,503,"connection_capacity");continue;}
                lab.peak.fetch_max(previous.unwrap_or(0)+1,Ordering::SeqCst);
                let shared=lab.clone();workers.push(thread::spawn(move||{
                    let _permit=Permit{count:&shared.active};let _=serve_connection(stream,&shared,port);
                }));
            }
            Err(e) if e.kind()==io::ErrorKind::WouldBlock=>thread::sleep(Duration::from_millis(2)),
            Err(e)=>{for h in workers{let _=h.join();}return Err(e)},
        }
    }
    for worker in workers{let _=worker.join();}Ok(())
}
