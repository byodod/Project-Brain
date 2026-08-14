use std::{env, io::Read, process::ExitCode};

use brain_evidence::{EvidenceCoverage, EvidencePlane, content_fingerprint};
use brain_provider_protocol::{
    PROVIDER_DESCRIPTOR_SCHEMA_VERSION, PROVIDER_PROCESS_PROTOCOL_VERSION,
    PROVIDER_RUN_RESPONSE_SCHEMA_VERSION, ProviderDescriptorV1, ProviderEvidenceCandidateV1,
    ProviderRunRequestV1, ProviderRunResponseV1, ProviderRunStatus,
};
use serde_json::json;

fn main() -> ExitCode {
    match env::args().nth(1).as_deref() {
        Some("describe") => describe(),
        Some("run") => run(),
        _ => {
            eprintln!("usage: reference_provider <describe|run>");
            ExitCode::FAILURE
        }
    }
}

fn describe() -> ExitCode {
    let descriptor = ProviderDescriptorV1 {
        schema_version: PROVIDER_DESCRIPTOR_SCHEMA_VERSION,
        protocol_version: PROVIDER_PROCESS_PROTOCOL_VERSION,
        provider_id: "reference-provider".to_owned(),
        provider_version: env!("CARGO_PKG_VERSION").to_owned(),
        provider_contract_version: 1,
        capabilities: vec![EvidencePlane::Build],
    };
    println!(
        "{}",
        serde_json::to_string(&descriptor).expect("descriptor JSON")
    );
    ExitCode::SUCCESS
}

fn run() -> ExitCode {
    let mut input = Vec::new();
    if std::io::stdin().read_to_end(&mut input).is_err() {
        return ExitCode::FAILURE;
    }
    let Ok(request) = serde_json::from_slice::<ProviderRunRequestV1>(&input) else {
        eprintln!("invalid ProviderRunRequestV1");
        return ExitCode::FAILURE;
    };
    if let Err(error) = request.validate() {
        eprintln!("{error}");
        return ExitCode::FAILURE;
    }
    let payload = json!({
        "input_entry_count": request.input_manifest.entries.len(),
        "profile_id": request.profile_id,
    });
    let candidate = ProviderEvidenceCandidateV1 {
        plane: request.plane,
        provider_version: env!("CARGO_PKG_VERSION").to_owned(),
        provider_contract_version: 1,
        coverage: EvidenceCoverage::Complete,
        upstream: Vec::new(),
        artifacts: Vec::new(),
        edges: Vec::new(),
        findings: Vec::new(),
        payload_schema: "reference-payload-v1".to_owned(),
        payload_hash: content_fingerprint(
            &serde_json::to_vec(&payload).expect("payload JSON serialization"),
        ),
        payload,
    };
    let response = ProviderRunResponseV1 {
        schema_version: PROVIDER_RUN_RESPONSE_SCHEMA_VERSION,
        protocol_version: PROVIDER_PROCESS_PROTOCOL_VERSION,
        request_id: request.request_id,
        provider_id: request.provider_id,
        profile_id: request.profile_id,
        project_key: request.project_key,
        source_fingerprint: request.source_fingerprint,
        input_manifest_hash: request.input_manifest.manifest_hash,
        status: ProviderRunStatus::Succeeded,
        candidate: Some(candidate),
        error_code: None,
        error_message: None,
    };
    println!(
        "{}",
        serde_json::to_string(&response).expect("response JSON")
    );
    ExitCode::SUCCESS
}
