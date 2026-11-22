//! Fixed synthetic increment with typed results and same-binary journal recovery.
use etag_for_codex::{
    journal::Journal,
    kernel::{Batch, Command, Member, MemberResult, Output, Session},
    Report,
};
use sha2::{Digest, Sha256};
use std::{env, fs};

fn show(report: &Report<Output>) -> Result<(), Box<dyn std::error::Error>> {
    for member in &report.payload.members {
        match &member.result {
            MemberResult::Sha256 { etag } => eprintln!("sha256: {etag}"),
            MemberResult::SumI64 { value } => eprintln!("sum: {value}"),
            MemberResult::Counter { value } => eprintln!("counter: {value}"),
            MemberResult::Refused { reason } => eprintln!("refused: {reason:?}"),
        }
    }
    println!("{}", serde_json::to_string(report)?);
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args().skip(1).collect();
    if args.len() != 2 {
        return Err("usage: typed_recovery JOURNAL_PATH STABLE_OPERATION_ID".into());
    }
    let executor_pin = format!("{:x}", Sha256::digest(fs::read(env::current_exe()?)?));
    let session = Session::with_journal(
        "typed-demo".into(),
        executor_pin.clone(),
        Journal::open(&args[0])?,
    )?;

    // This example has one fixed intent: increment by one. An existing ID means
    // recover that original intent, not reinterpret it as a new request.
    if let Some(saved) = session.lookup(&args[1])? {
        saved.verify(&saved.input_etag)?;
        return show(&saved);
    }
    let (revision, _) = session.state()?;
    let batch = Batch {
        schema: "amqic.etag.kernel.v1".into(),
        scope: "typed-demo".into(),
        kernel_sha256: executor_pin,
        operation_id: args[1].clone(),
        expected_revision: revision,
        members: vec![Member {
            id: "counter".into(),
            command: Command::IncrementCounter { amount: 1 },
        }],
    };
    // A real agent host retains this tag from its separately accepted planning
    // step; it must not silently replace the accepted tag at execution time.
    let accepted_input = batch.etag()?;
    let report = session.execute(&batch, Some(accepted_input.as_str()))?;
    report.verify(&accepted_input)?;
    let replay = session.execute(&batch, Some(accepted_input.as_str()))?;
    assert_eq!(serde_json::to_vec(&report)?, serde_json::to_vec(&replay)?);
    show(&report)
}
