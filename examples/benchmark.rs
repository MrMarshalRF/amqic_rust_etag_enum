//! Fixed synthetic benchmark. No API calls, tokens or cost estimates.
use etag_for_codex::{kernel, Etag};
use serde_json::{json,Value};
use std::{io::Write,process::{Command,Stdio},time::Instant};
fn invoke(exe:&str, batch:&kernel::Batch) -> Result<(u64,usize),Box<dyn std::error::Error>> {
    let tag=batch.etag()?; // outside the measured process interval in BOTH modes
    let request=json!({"protocol":"etag-for-codex.v1","action":{"kind":"kernel_run","batch":batch,"expected_etag":tag}}).to_string();
    let start=Instant::now();
    let mut child=Command::new(exe).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    child.stdin.take().ok_or("stdin")?.write_all(request.as_bytes())?;
    let output=child.wait_with_output()?;
    let elapsed=start.elapsed().as_nanos() as u64;
    if !output.status.success() {return Err("benchmark child failed".into());}
    let value:Value=serde_json::from_slice(&output.stdout)?;
    if value["ok"]!=true || value["result"]["payload"]["members"].as_array().map(Vec::len)!=Some(batch.members.len()) {
        return Err("benchmark result mismatch".into());
    }
    Ok((elapsed,output.stdout.len()))
}
fn main()->Result<(),Box<dyn std::error::Error>> {
    let exe=std::env::args().nth(1).ok_or("pass exact CLI executable path")?;
    let hash=Etag::of_bytes(&std::fs::read(&exe)?);
    let batch=kernel::Batch {schema:"amqic.etag.kernel.v1".into(),scope:"benchmark".into(),kernel_sha256:hash.as_str()[8..72].to_owned(),
        operation_id:"benchmark-1".into(),expected_revision:0,members:(0..16).map(|i|kernel::Member{id:format!("m-{i}"),command:kernel::Command::Sha256{text:"x".repeat(256)}}).collect()};
    let warmup=invoke(&exe,&batch)?.0;
    let mut single=Vec::new();let mut grouped=Vec::new();let mut single_bytes=0;let mut batch_bytes=0;
    for round in 0..5 {
        let mut do_single=||->Result<u64,Box<dyn std::error::Error>> {let mut total=0;for member in &batch.members {
            let mut one=batch.clone();one.members=vec![member.clone()];let (ns,bytes)=invoke(&exe,&one)?;total+=ns;single_bytes+=bytes;
        }Ok(total)};
        if round%2==0 {single.push(do_single()?);let(ns,bytes)=invoke(&exe,&batch)?;grouped.push(ns);batch_bytes+=bytes;}
        else {let(ns,bytes)=invoke(&exe,&batch)?;grouped.push(ns);batch_bytes+=bytes;single.push(do_single()?);}
    }
    let mut a=single.clone();a.sort_unstable();let mut b=grouped.clone();b.sort_unstable();
    let start=Instant::now();for _ in 0..1000 {std::hint::black_box(batch.etag()?);}let etag_ns=start.elapsed().as_nanos() as u64;
    let result=json!({"schema":"amqic.etag.benchmark.v1","os":std::env::consts::OS,"arch":std::env::consts::ARCH,
        "cli_etag":hash,"members":16,"payload_bytes_per_member":256,"rounds":5,"warmup_batch_ns":warmup,
        "single_process_ns":single,"batch_process_ns":grouped,"single_median_ns":a[2],"batch_median_ns":b[2],
        "median_speedup":a[2] as f64/b[2] as f64,"single_stdout_bytes_total":single_bytes,"batch_stdout_bytes_total":batch_bytes,
        "etag_rounds":1000,"etag_total_ns":etag_ns,"measures":"warm synthetic CLI process amortization; child executable hashing, parsing and result serialization included; client plan hashing and request serialization excluded equally; NOT ETag versus no-ETag",
        "provider_calls":0,"tokens":"not measured","codex_end_to_end":"not measured"});
    let report=etag_for_codex::Report::new(batch.etag()?,result)?;
    println!("{}",serde_json::to_string_pretty(&report)?);Ok(())
}
