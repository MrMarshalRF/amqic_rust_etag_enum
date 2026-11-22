use etag_for_codex::{api::*, kernel, *};
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;

fn kernel_batch() -> kernel::Batch {
    kernel::Batch { schema:"amqic.etag.kernel.v1".into(), scope:"test".into(), kernel_sha256:"0".repeat(64),
        operation_id:"op-1".into(), expected_revision:0, members:vec![kernel::Member {
            id:"a".into(), command:kernel::Command::IncrementCounter { amount:1 },
        }] }
}
fn api_batch() -> Batch {
    Batch { schema:"amqic.etag.openai-batch.v1".into(), scope:"test".into(), operation_id:"op-1".into(),
        endpoint:Endpoint::Responses, requests:vec![Request { custom_id:"a".into(), body:json!({"model":"test-model","input":"hello"}) },
            Request { custom_id:"b".into(), body:json!({"model":"test-model","input":"world"}) }] }
}
#[test]
fn strong_tag_golden_and_boundary() {
    assert_eq!(Etag::of_bytes(b"abc").as_str(), "\"sha256-ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\"");
    for value in ["*", "W/\"abc\"", "abc", "", "\"sha256-XXX\""] { assert!(Etag::parse(value).is_err()); }
    let tag=Etag::of_bytes(b"a");
    assert_eq!(tag.require(None),Err(Error::MissingPrecondition));
    assert_eq!(tag.require(Some(Etag::of_bytes(b"b").as_str())),Err(Error::StaleSnapshot));
    assert_eq!(Error::MissingPrecondition.http_status(),428);
    assert_eq!(Error::StaleSnapshot.http_status(),412);
    assert!(serde_json::from_value::<Etag>(json!("W/\"abc\"")).is_err());
}
#[test]
fn canonical_order_and_legacy_tuple_golden() {
    assert_eq!(canonical(&json!({"z":1,"a":{"z":2,"b":3}})).unwrap(), br#"{"a":{"b":3,"z":2},"z":1}"#);
    #[derive(Serialize)] struct Pi { operation:&'static str, digits:u32 }
    let tag=qcalculator_v1_etag(&"0".repeat(64), &[Pi { operation:"pi", digits:50 }]).unwrap();
    // Independently calculated using Node crypto over exact existing tuple bytes.
    assert_eq!(tag.as_str(),"\"sha256-72bfb4cb9e7046380a457088777a90455f43c625f63151f5530eece00a814eb0\"");
}
#[test]
fn every_relevant_kernel_dependency_invalidates() {
    let b=kernel_batch(); let tag=b.etag().unwrap();
    let mut changes=Vec::new();
    let mut x=b.clone();x.scope="other".into();changes.push(x);
    let mut x=b.clone();x.kernel_sha256="1".repeat(64);changes.push(x);
    let mut x=b.clone();x.expected_revision=1;changes.push(x);
    let mut x=b.clone();x.operation_id="op-2".into();changes.push(x);
    let mut x=b.clone();x.members[0].id="b".into();changes.push(x);
    let mut x=b.clone();x.members[0].command=kernel::Command::IncrementCounter { amount:2 };changes.push(x);
    for x in changes { assert_ne!(tag,x.etag().unwrap()); }
    let mut x=b.clone();let mut m=x.members[0].clone();m.id="b".into();x.members.push(m);
    let before=x.etag().unwrap();x.members.reverse();assert_ne!(before,x.etag().unwrap());
}
#[test]
fn stale_zero_effects_and_replay_original_result() {
    let b=kernel_batch(); let session=kernel::Session::new(b.scope.clone(),b.kernel_sha256.clone()).unwrap();
    assert_eq!(session.execute(&b,None),Err(Error::MissingPrecondition));
    assert_eq!(session.execute(&b,Some(Etag::of_bytes(b"stale").as_str())),Err(Error::StaleSnapshot));
    assert_eq!(session.state().unwrap(),(0,0));
    let tag=b.etag().unwrap();let first=session.execute(&b,Some(tag.as_str())).unwrap();
    assert_ne!(first.etag,first.input_etag);first.verify(&tag).unwrap();
    assert_eq!(first,session.execute(&b,Some(tag.as_str())).unwrap());
    assert_eq!(session.state().unwrap(),(1,1));
    let mut changed=b.clone();changed.members[0].command=kernel::Command::IncrementCounter { amount:2 };
    assert_eq!(session.execute(&changed,Some(changed.etag().unwrap().as_str())),Err(Error::OperationConflict));
    assert_eq!(session.state().unwrap(),(1,1));
}
#[test]
fn competing_revisions_admit_only_one() {
    let b=kernel_batch();let session=Arc::new(kernel::Session::new(b.scope.clone(),b.kernel_sha256.clone()).unwrap());
    let mut handles=Vec::new();
    for i in 0..12 { let mut b=b.clone();b.operation_id=format!("op-{i}");let s=session.clone();
        handles.push(std::thread::spawn(move || s.execute(&b,Some(b.etag().unwrap().as_str())))); }
    let results:Vec<_>=handles.into_iter().map(|h|h.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r|r.is_ok()).count(),1);
    assert!(results.iter().filter_map(|r|r.as_ref().err()).all(|e|e==&Error::StaleRevision));
    assert_eq!(session.state().unwrap(),(1,1));
}
#[test]
fn partial_result_and_enum_rejection() {
    let mut b=kernel_batch();b.members.push(kernel::Member { id:"overflow".into(), command:kernel::Command::SumI64 { values:vec![i64::MAX,1] } });
    let s=kernel::Session::new(b.scope.clone(),b.kernel_sha256.clone()).unwrap();
    let r=s.execute(&b,Some(b.etag().unwrap().as_str())).unwrap();assert_eq!(r.payload.status,kernel::Status::Partial);
    let mut bad=r.clone();bad.payload.counter=42;assert!(bad.verify(&r.input_etag).is_err());
    assert!(serde_json::from_value::<kernel::Command>(json!({"kind":"shell","command":"anything"})).is_err());
    assert!(serde_json::from_value::<kernel::Command>(json!({"kind":"sha256","text":"x","surprise":1})).is_err());
    assert!(serde_json::from_value::<Endpoint>(json!("/v1/unknown")).is_err());
}
#[test]
fn api_export_exact_bytes_and_conditions() {
    let b=api_batch();let p=b.prepare().unwrap();
    assert_eq!(*p.file_etag(),Etag::of_bytes(p.jsonl()));
    for line in std::str::from_utf8(p.jsonl()).unwrap().lines() {
        let row:serde_json::Value=serde_json::from_str(line).unwrap();
        assert_eq!(row.as_object().unwrap().len(),4);assert_eq!(row["method"],"POST");assert_eq!(row["url"],"/v1/responses");
        assert!(row.get("etag").is_none());
    }
    let create=p.create_body("file-1").unwrap();assert_eq!(create["metadata"]["amqic_input_etag"],p.etag().as_str());
    let mut changed=b.clone();changed.requests[0].body["input"]=json!("changed");assert_ne!(p.etag(),changed.prepare().unwrap().etag());
    let mut dup=b.clone();dup.requests[1].custom_id="a".into();assert!(dup.prepare().is_err());
    let mut mixed=b;mixed.requests[1].body["model"]=json!("other-model");assert!(mixed.prepare().is_err());
}
#[derive(Default)] struct Fake { uploads:usize, creates:usize, unknown_upload:bool, unknown_create:bool, bytes:Vec<u8> }
impl Transport for Fake {
    fn upload(&mut self,bytes:&[u8])->TransportOutcome { self.uploads+=1;self.bytes=bytes.to_vec();
        if self.unknown_upload { TransportOutcome::Unknown } else { TransportOutcome::Confirmed { id:"file-1".into() } } }
    fn create(&mut self,_:&serde_json::Value)->TransportOutcome { self.creates+=1;
        if self.unknown_create { TransportOutcome::Unknown } else { TransportOutcome::Confirmed { id:"batch-1".into() } } }
}
#[test]
fn api_side_effect_gate_and_session_ledger() {
    let p=api_batch().prepare().unwrap();let s=ApiSession::new("test".into()).unwrap();let mut t=Fake::default();
    assert_eq!(s.advance(&p,None,&mut t),Err(Error::MissingPrecondition));
    assert!(s.advance(&p,Some(Etag::of_bytes(b"stale").as_str()),&mut t).is_err());assert_eq!(t.uploads,0);
    s.advance(&p,Some(p.etag().as_str()),&mut t).unwrap();assert_eq!(t.bytes,p.jsonl());
    let copy=p.clone();s.advance(&copy,Some(copy.etag().as_str()),&mut t).unwrap();
    for _ in 0..3 { let r=s.advance(&p,Some(p.etag().as_str()),&mut t).unwrap();assert!(matches!(r.payload,SubmissionState::Submitted { .. })); }
    assert_eq!((t.uploads,t.creates),(1,1));
    let mut changed=api_batch();changed.requests[0].body["input"]=json!("changed");let changed=changed.prepare().unwrap();
    assert_eq!(s.advance(&changed,Some(changed.etag().as_str()),&mut t),Err(Error::OperationConflict));
    assert_eq!((t.uploads,t.creates),(1,1));
}
#[test]
fn unknown_api_outcomes_never_replay() {
    for at_upload in [true,false] {
        let p=api_batch().prepare().unwrap();let s=ApiSession::new("test".into()).unwrap();
        let mut t=Fake { unknown_upload:at_upload, unknown_create:!at_upload, ..Fake::default() };
        for _ in 0..5 { s.advance(&p,Some(p.etag().as_str()),&mut t).unwrap(); }
        assert_eq!(t.uploads,1);assert_eq!(t.creates,usize::from(!at_upload));
        let report=s.advance(&p,Some(p.etag().as_str()),&mut t).unwrap();
        assert!(matches!(report.payload,SubmissionState::UnknownUpload|SubmissionState::UnknownCreate { .. }));
    }
}
#[test]
fn api_reconcile_reordered_missing_errors_and_tampering() {
    let p=api_batch().prepare().unwrap();
    let a=json!({"custom_id":"a","response":{"status_code":200,"body":{"output":"ok"}},"error":null});
    let b=json!({"custom_id":"b","response":null,"error":{"code":"batch_expired"}});
    let bytes=format!("{b}\n{a}\n");let r=p.reconcile(Some(p.etag().as_str()),bytes.as_bytes()).unwrap();
    r.verify(p.etag()).unwrap();assert_eq!(r.payload.members[0].custom_id,"a");
    assert!(matches!(r.payload.members[1].result,ApiMemberResult::Failed { .. }));
    let missing=p.reconcile(Some(p.etag().as_str()),format!("{a}\n").as_bytes()).unwrap();
    assert!(matches!(missing.payload.members[1].result,ApiMemberResult::Unknown));assert_ne!(r.etag,missing.etag);
    assert!(p.reconcile(Some(p.etag().as_str()),format!("{a}\n{a}\n").as_bytes()).is_err());
    assert!(p.reconcile(Some(p.etag().as_str()),b"{\"custom_id\":\"foreign\"}\n").is_err());
}
