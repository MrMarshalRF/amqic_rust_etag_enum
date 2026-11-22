use etag_for_codex::{lab::{self,Lab},Etag};
use std::{io::{self,Read},net::TcpListener,sync::{Arc,atomic::{AtomicBool,Ordering}},thread,time::{SystemTime,UNIX_EPOCH}};
fn main()->Result<(),Box<dyn std::error::Error>> {
    let mut args=std::env::args().skip(1);let mut port=0u16;let mut port_seen=false;let mut journal=None;
    while let Some(arg)=args.next(){match arg.as_str(){
        "--port" if !port_seen=>{port=args.next().ok_or("missing port")?.parse()?;port_seen=true;},
        "--journal" if journal.is_none()=>journal=Some(args.next().ok_or("missing journal path")?),
        "--help"=>{println!("etag-lab [--port 0] [--journal PATH] (stdin EOF stops server)");return Ok(());},
        _=>return Err("usage: etag-lab [--port 0] [--journal PATH] (no duplicate options)".into()),
    }}
    eprintln!("{{\"event\":\"launch_intent\",\"owner\":\"etag-lab\",\"synthetic\":true,\"primary_environment\":false,\"cleanup\":\"stdin EOF joins owned workers\"}}");
    let tag=Etag::of_bytes(&std::fs::read(std::env::current_exe()?)?);
    let journal_enabled=journal.is_some();
    let lab=Arc::new(match journal{Some(path)=>Lab::open(tag.as_str()[8..72].into(),path)?,None=>Lab::new(tag.as_str()[8..72].into())?});
    let listener=TcpListener::bind(("127.0.0.1",port))?;let address=listener.local_addr()?;
    let started=SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos().to_string();
    let instance_id=format!("{}-{started}",std::process::id());let stop=Arc::new(AtomicBool::new(false));
    let signal=stop.clone();thread::spawn(move||{let mut buffer=[0u8;256];while io::stdin().read(&mut buffer).is_ok_and(|n|n>0){}signal.store(true,Ordering::SeqCst);});
    eprintln!("{}",serde_json::json!({"event":"listening","url":format!("http://{address}"),"pid":std::process::id(),"instance_id":instance_id,"started_at_unix_nanos":started,
        "kernel_sha256":tag.as_str()[8..72].to_owned(),"synthetic":true,"journal_enabled":journal_enabled}));
    lab::serve(listener,lab,stop)?;eprintln!("{{\"event\":\"stopped\",\"workers\":\"joined\"}}");Ok(())
}
