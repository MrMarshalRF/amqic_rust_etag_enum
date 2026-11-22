use etag_for_codex::api::*;
use serde_json::json;

struct DemoTransport;
impl Transport for DemoTransport {
    fn upload(&mut self, _bytes:&[u8]) -> TransportOutcome { TransportOutcome::Confirmed { id:"file-demo".into() } }
    fn create(&mut self, _body:&serde_json::Value) -> TransportOutcome { TransportOutcome::Confirmed { id:"batch-demo".into() } }
}
fn main() -> Result<(),Box<dyn std::error::Error>> {
    let batch=Batch { schema:"amqic.etag.openai-batch.v1".into(), scope:"demo".into(),operation_id:"original-op-1".into(),
        endpoint:Endpoint::Responses,requests:vec![Request { custom_id:"one".into(),body:json!({"model":"YOUR_BATCH_CAPABLE_MODEL","input":"Hello"}) }] };
    let plan=batch.prepare()?;
    let reviewed_etag=plan.etag().clone(); // In a real host retain this from the separately accepted plan.
    let session=ApiSession::new("demo".into())?;let mut transport=DemoTransport;
    println!("{}",serde_json::to_string(&session.advance(&plan,Some(reviewed_etag.as_str()),&mut transport)?)?);
    println!("{}",serde_json::to_string(&session.advance(&plan,Some(reviewed_etag.as_str()),&mut transport)?)?);
    // Re-reading a terminal state makes no additional upload/create.
    println!("{}",serde_json::to_string(&session.advance(&plan,Some(reviewed_etag.as_str()),&mut transport)?)?);
    Ok(())
}
