//! Finite real-child restart proof against the synthetic loopback lab only.
use etag_for_codex::{api, identity, kernel, lab::{Fault, LabBatch, ProviderBatch, ProviderFault}, Etag, Report};
use serde_json::{json, Value};
use std::{fs::{self, OpenOptions}, io::{BufRead, BufReader, Read, Write}, net::{SocketAddr, TcpStream}, path::{Path, PathBuf}, process::{Child, Command, Stdio}, sync::mpsc, thread, time::{Duration, Instant, SystemTime, UNIX_EPOCH}};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn append(path: &Path, value: &Value) -> Result<()> {
    let mut f = OpenOptions::new().append(true).create(true).open(path)?;
    writeln!(f, "{value}")?; f.sync_all()?; Ok(())
}
fn now() -> Result<String> { Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos().to_string()) }
struct Server { child: Child, address: Option<SocketAddr>, startup: Value, log: PathBuf, stopped: bool }
impl Server {
    fn start(bin: &Path, db: &Path, log: &Path, pin: &str) -> Result<Self> {
        append(log, &json!({"event":"launch_intent","owner":"lab-restart-scenarios","synthetic":true,"primary_environment":false,"binary":bin,"journal":db,"cleanup_owner":"exact-child-guard","recorded_at":now()?}))?;
        let child = Command::new(bin).args(["--port", "0", "--journal"]).arg(db).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn()?;
        let mut server = Self {child, address:None, startup:Value::Null, log:log.into(), stopped:false};
        append(log, &json!({"event":"spawn_observed","pid":server.child.id(),"observed_at":now()?}))?;
        let stderr = server.child.stderr.take().ok_or("child stderr unavailable")?;
        let (tx, rx) = mpsc::channel();
        // Keep draining even after startup's receiver closes: a closed stderr
        // pipe must not turn the child's graceful shutdown log into a failure.
        thread::spawn(move || { for line in BufReader::new(stderr).lines() { match line { Ok(line) => { let _=tx.send(line); }, Err(_) => break } } });
        let deadline = Instant::now()+Duration::from_secs(10);
        loop {
            let line=rx.recv_timeout(deadline.checked_duration_since(Instant::now()).ok_or("startup deadline")?)?;
            let Ok(event)=serde_json::from_str::<Value>(&line) else {continue};
            append(log, &event)?;
            if event["event"]!="listening" {continue;}
            let url=event["url"].as_str().ok_or("startup URL")?;
            let address:SocketAddr=url.strip_prefix("http://").ok_or("startup scheme")?.parse()?;
            // Store the observed address before later checks so the error guard verifies teardown.
            server.address=Some(address); server.startup=event;
            if !address.ip().is_loopback() || address.port()==0 || server.startup["pid"].as_u64()!=Some(server.child.id().into()) || server.startup["synthetic"]!=true || server.startup["journal_enabled"]!=true || server.startup["kernel_sha256"]!=pin || server.startup["instance_id"].as_str().is_none_or(str::is_empty) || server.startup["started_at_unix_nanos"].as_str().is_none_or(str::is_empty) {return Err("unexpected child identity or durable synthetic contract".into());}
            return Ok(server);
        }
    }
    fn address(&self)->SocketAddr {self.address.expect("validated listening event")}
    fn stop(&mut self, abrupt:bool)->Result<Value> {
        if abrupt {self.child.kill()?;} else {drop(self.child.stdin.take());}
        let deadline=Instant::now()+Duration::from_secs(5);
        let status=loop {if let Some(s)=self.child.try_wait()? {break s;} if Instant::now()>deadline {self.child.kill()?;self.child.wait()?;return Err("graceful stop deadline; exact child killed and waited".into());}thread::sleep(Duration::from_millis(10));};
        self.child.wait()?;
        let absent=if let Some(address)=self.address {TcpStream::connect_timeout(&address,Duration::from_millis(200)).is_err()} else {true};
        self.stopped=true;
        let proof=json!({"event":"teardown","startup":self.startup,"mode":if abrupt{"hard_kill"}else{"stdin_eof"},"pid":self.child.id(),"waited":true,"listener_absent":absent,"exit_success":status.success()});
        append(&self.log,&proof)?;
        if !absent || (!abrupt&&!status.success()) {return Err("child teardown oracle failed".into());} Ok(proof)
    }
}
impl Drop for Server {fn drop(&mut self) {if !self.stopped {let _=self.child.kill();let waited=self.child.wait().is_ok();let absent=self.address.is_none_or(|a|TcpStream::connect_timeout(&a,Duration::from_millis(200)).is_err());let _=append(&self.log,&json!({"event":"error_guard_teardown","pid":self.child.id(),"startup":self.startup,"waited":waited,"listener_absent":absent}));}}}

struct Response {status:u16, body:Value}
fn request(address:SocketAddr, method:&str, path:&str, tag:Option<&str>, body:&[u8])->Result<Response> {
    let mut stream=TcpStream::connect_timeout(&address,Duration::from_secs(2))?;
    stream.set_read_timeout(Some(Duration::from_secs(4)))?;stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let condition=tag.map(|s|format!("If-Match: {s}\r\n")).unwrap_or_default();
    write!(stream,"{method} {path} HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{condition}\r\n",body.len())?;
    stream.write_all(body)?;let mut bytes=Vec::new();stream.take(262145).read_to_end(&mut bytes)?;
    if bytes.is_empty() {return Ok(Response{status:0,body:Value::Null});}
    if bytes.len()>262144 {return Err("response bound".into());}
    let split=bytes.windows(4).position(|p|p==b"\r\n\r\n").ok_or("response headers")?;
    let header=std::str::from_utf8(&bytes[..split])?;
    let status=header.lines().next().and_then(|s|s.split_whitespace().nth(1)).ok_or("status")?.parse()?;
    Ok(Response{status,body:serde_json::from_slice(&bytes[split+4..])?})
}
fn get(address:SocketAddr,path:&str)->Result<Value> {let r=request(address,"GET",path,None,b"")?;if r.status!=200{return Err(format!("GET {path}: {}",r.status).into());}Ok(r.body["result"].clone())}
fn info(s:&Server)->Result<Value> {let v=get(s.address(),"/api/lab/info")?;if v["synthetic"]!=true||v["upstream_calls"]!=0{return Err("synthetic zero-upstream contract".into());}Ok(v)}
fn plan(s:&Server,path:&str,body:&[u8])->Result<String> {let r=request(s.address(),"POST",path,None,body)?;if r.status!=200{return Err("plan rejected".into());}Ok(r.body["result"]["input_etag"].as_str().ok_or("plan tag")?.into())}
fn descriptor(state:&Value,id:String,fault:Fault,partial:bool)->LabBatch {
    let mut members=vec![kernel::Member{id:"effect".into(),command:kernel::Command::IncrementCounter{amount:1}}];
    if partial {members.push(kernel::Member{id:"overflow".into(),command:kernel::Command::SumI64{values:vec![i64::MAX,1]}});}
    LabBatch{schema:"amqic.etag.lab.v1".into(),fault,batch:kernel::Batch{schema:"amqic.etag.kernel.v1".into(),scope:"lab".into(),operation_id:id,kernel_sha256:state["kernel_sha256"].as_str().unwrap_or_default().into(),expected_revision:state["revision"].as_u64().unwrap_or(0),members}}
}
fn verify_report(v:&Value,tag:&str)->Result<()> {let report:Report<Value>=serde_json::from_value(v.clone())?;report.verify(&Etag::parse(tag)?)?;Ok(())}
fn same_state(a:&Value,b:&Value)->bool {["counter","revision","executions","operations","event_id"].iter().all(|key|a[*key]==b[*key])}
fn record(rows:&mut Vec<Value>,name:&str,passed:bool,detail:Value) {println!("{name:<30} {}",if passed{"PASS"}else{"FAIL"});rows.push(json!({"scenario":name,"passed":passed,"observed":detail}));}

fn execute(bin:&Path,dir:&Path,pin:&str,run:&str,rows:&mut Vec<Value>,processes:&mut Vec<Value>)->Result<()> {
    let db=dir.join("journal.sqlite3");let log=dir.join("lifecycle.jsonl");let mut previous_instance=String::new();
    for (name,fault,partial,abrupt) in [("completed_hard_kill",Fault::None,false,true),("partial_hard_kill",Fault::None,true,true),("drop_after_commit_hard_kill",Fault::DropAfterCommit,false,true)] {
        let mut s=Server::start(bin,&db,&log,pin)?;let before=info(&s)?;
        let d=descriptor(&before,format!("{run}-{name}"),fault,partial);let body=serde_json::to_vec(&d)?;let tag=plan(&s,"/api/lab/plan",&body)?;
        let first=request(s.address(),"POST","/api/lab/run",Some(&tag),&body)?;
        let old=get(s.address(),&format!("/api/lab/operations/{}",d.batch.operation_id))?;verify_report(&old,&tag)?;
        let committed=info(&s)?;let first_ok=if fault==Fault::DropAfterCommit {first.status==0} else {first.status==200&&first.body["result"]==old};
        let start=s.startup.clone();processes.push(s.stop(abrupt)?);
        if Etag::of_bytes(&fs::read(bin)?).as_str()[8..72]!=*pin {return Err("lab executable changed between restart".into());}
        let mut restored=Server::start(bin,&db,&log,pin)?;let loaded=info(&restored)?;
        let replay=request(restored.address(),"POST","/api/lab/run",Some(&tag),&body)?;let after=info(&restored)?;
        let distinct=start["instance_id"]!=restored.startup["instance_id"] && restored.startup["instance_id"].as_str()!=Some(previous_instance.as_str());
        previous_instance=restored.startup["instance_id"].as_str().unwrap_or_default().into();
        record(rows,name,first_ok&&distinct&&same_state(&committed,&loaded)&&same_state(&loaded,&after)&&replay.status==200&&replay.body["result"]==old&&committed["counter"].as_i64()==before["counter"].as_i64().map(|n|n+1)&&old["payload"]["status"]==if partial{"partial"}else{"completed"},json!({"journal":db,"input_etag":tag,"report":old,"before":before,"committed":committed,"restored":loaded,"after_replay":after,"first_status":first.status,"replay_status":replay.status,"first_start":start,"restart_start":restored.startup}));
        if name=="completed_hard_kill" {
            let mut changed=d.clone();changed.batch.members[0].command=kernel::Command::IncrementCounter{amount:9};let cb=serde_json::to_vec(&changed)?;let ct=plan(&restored,"/api/lab/plan",&cb)?;let conflict=request(restored.address(),"POST","/api/lab/run",Some(&ct),&cb)?;
            let next=descriptor(&after,format!("{run}-new-op"),Fault::None,false);let nb=serde_json::to_vec(&next)?;let nt=plan(&restored,"/api/lab/plan",&nb)?;let nr=request(restored.address(),"POST","/api/lab/run",Some(&nt),&nb)?;let end=info(&restored)?;
            record(rows,"conflict_and_new_revision",conflict.status==409&&nr.status==200&&end["counter"].as_i64()==after["counter"].as_i64().map(|n|n+1)&&end["revision"].as_u64()==after["revision"].as_u64().map(|n|n+1),json!({"conflict_status":conflict.status,"new_status":nr.status,"before":after,"after":end}));
        }
        processes.push(restored.stop(false)?);
    }
    for (name,fault,state,creates) in [("unknown_upload_restart",ProviderFault::UnknownUpload,"unknown_upload",0),("unknown_create_restart",ProviderFault::UnknownCreate,"unknown_create",1)] {
        let mut s=Server::start(bin,&db,&log,pin)?;
        let d=ProviderBatch{schema:"amqic.etag.lab-provider.v1".into(),fault,batch:api::Batch{schema:"amqic.etag.openai-batch.v1".into(),scope:"lab-api".into(),operation_id:format!("{run}-{name}"),endpoint:api::Endpoint::Responses,requests:vec![api::Request{custom_id:"one".into(),body:json!({"model":"synthetic-only","input":"restart"})}]}};
        let body=serde_json::to_vec(&d)?;let tag=plan(&s,"/api/lab/provider/plan",&body)?;
        let first=request(s.address(),"POST","/api/lab/provider/run",Some(&tag),&body)?;let old=first.body["result"].clone();verify_report(&old,&tag)?;let before=info(&s)?;let start=s.startup.clone();processes.push(s.stop(true)?);
        let mut restored=Server::start(bin,&db,&log,pin)?;let recovered=get(restored.address(),&format!("/api/lab/provider/operations/{}",d.batch.operation_id))?;
        let replay=request(restored.address(),"POST","/api/lab/provider/run",Some(&tag),&body)?;let after=info(&restored)?;
        record(rows,name,first.status==200&&replay.status==200&&old==recovered&&replay.body["result"]==old&&same_state(&before,&after)&&start["instance_id"]!=restored.startup["instance_id"]&&old["payload"]["submission"]["payload"]["state"]==state&&old["payload"]["provider_observation"]["upload_calls"]==1&&old["payload"]["provider_observation"]["create_calls"]==creates,json!({"input_etag":tag,"before":before,"after":after,"report":old,"recovered":recovered,"first_start":start,"restart_start":restored.startup}));
        processes.push(restored.stop(false)?);
    }
    // Two independently running OS processes share exactly one journal. A real
    // thread barrier makes requests eligible together without guessed delays.
    let mut one=Server::start(bin,&db,&log,pin)?;let mut two=Server::start(bin,&db,&log,pin)?;
    let before=info(&one)?;let d=descriptor(&before,format!("{run}-cross-process-same"),Fault::None,false);
    let body=serde_json::to_vec(&d)?;let tag=plan(&one,"/api/lab/plan",&body)?;
    let paired=|a:SocketAddr,b:SocketAddr,x:Vec<u8>,xt:String,y:Vec<u8>,yt:String|->Result<(Response,Response)> {
        let barrier=std::sync::Arc::new(std::sync::Barrier::new(2));let peer=barrier.clone();
        let worker=thread::spawn(move || {peer.wait();request(a,"POST","/api/lab/run",Some(&xt),&x).map_err(|e|e.to_string())});
        barrier.wait();let second=request(b,"POST","/api/lab/run",Some(&yt),&y);
        let first=worker.join().map_err(|_|"paired request thread panic")?.map_err(std::io::Error::other)?;
        Ok((first,second?))
    };
    let (a,b)=paired(one.address(),two.address(),body.clone(),tag.clone(),body,tag.clone())?;
    let after=info(&one)?;let peer_after=info(&two)?;
    verify_report(&a.body["result"],&tag)?;
    let same_ok=a.status==200&&b.status==200&&a.body==b.body&&same_state(&after,&peer_after)&&after["counter"].as_i64()==before["counter"].as_i64().map(|n|n+1)&&after["executions"].as_u64()==before["executions"].as_u64().map(|n|n+1);
    let x=serde_json::to_vec(&descriptor(&after,format!("{run}-cross-process-a"),Fault::None,false))?;
    let y=serde_json::to_vec(&descriptor(&after,format!("{run}-cross-process-b"),Fault::None,false))?;
    let xt=plan(&one,"/api/lab/plan",&x)?;let yt=plan(&two,"/api/lab/plan",&y)?;
    let (x_result,y_result)=paired(one.address(),two.address(),x,xt,y,yt)?;
    let end=info(&one)?;let peer_end=info(&two)?;
    let stale_error=if x_result.status==412 {&x_result.body}else{&y_result.body};
    record(rows,"cross_process_admission",same_ok&&[(x_result.status,y_result.status),(y_result.status,x_result.status)].contains(&(200,412))&&stale_error["error"]["error"]=="stale_revision"&&same_state(&end,&peer_end)&&end["counter"].as_i64()==after["counter"].as_i64().map(|n|n+1)&&end["revision"].as_u64()==after["revision"].as_u64().map(|n|n+1),json!({"same_operation_statuses":[a.status,b.status],"distinct_operation_statuses":[x_result.status,y_result.status],"distinct_responses":[x_result.body,y_result.body],"before":before,"after_same":after,"after_distinct":end,"first_start":one.startup,"second_start":two.startup}));
    processes.push(one.stop(false)?);processes.push(two.stop(false)?);
    Ok(())
}
fn main()->Result<()> {
    let mut args=std::env::args().skip(1);let mut bin=None;let mut dir=None;
    while let Some(arg)=args.next(){match arg.as_str(){"--lab-bin" if bin.is_none()=>bin=Some(PathBuf::from(args.next().ok_or("lab binary path")?)),"--work-dir" if dir.is_none()=>dir=Some(PathBuf::from(args.next().ok_or("new work directory")?)),_=>return Err("usage: lab-restart-scenarios --lab-bin PATH --work-dir NEW_DIR".into())}}
    let bin=fs::canonicalize(bin.ok_or("--lab-bin required")?)?;let dir=dir.ok_or("--work-dir required")?;fs::create_dir(&dir)?;
    let run=now()?;let tag=Etag::of_bytes(&fs::read(&bin)?);let pin=&tag.as_str()[8..72];let input=identity(&("amqic.etag.restart-run.v1",&run,pin,&bin))?;
    let report_path=dir.join("report.json");let mut report_file=OpenOptions::new().write(true).create_new(true).open(&report_path)?;
    writeln!(report_file,"{}",json!({"state":"intent","run_id":run,"input_etag":input,"synthetic":true}))?;report_file.sync_all()?;
    println!("Synthetic actual-process restart suite; retained work directory {}",dir.display());
    let mut rows=Vec::new();let mut processes=Vec::new();let outcome=execute(&bin,&dir,pin,&run,&mut rows,&mut processes);
    if let Err(e)=&outcome {rows.push(json!({"scenario":"execution_error","passed":false,"error":e.to_string()}));}
    let failed=rows.iter().filter(|r|r["passed"]!=true).count();
    let report=Report::new(input,json!({"schema":"amqic.etag.restart-run.v1","run_id":run,"synthetic":true,"lab_binary":bin,"kernel_sha256":pin,"journal":dir.join("journal.sqlite3"),"cases":rows,"processes":processes,"failed":failed,"limits":"Actual OS child termination and restart; synthetic local effects only; no power-cut, hardware flush, real provider or production guarantee"}))?;
    use std::io::{Seek,SeekFrom};report_file.set_len(0)?;report_file.seek(SeekFrom::Start(0))?;serde_json::to_writer_pretty(&mut report_file,&report)?;report_file.sync_all()?;
    println!("Report: {}\nReport ETag: {}\nFailed scenarios: {failed}",report_path.display(),report.etag);
    outcome?;if failed!=0{return Err("restart oracle failed; preserve work directory and original IDs".into());}Ok(())
}
