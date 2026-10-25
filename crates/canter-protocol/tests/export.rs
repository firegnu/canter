use canter_protocol::export_protocol;
use serde_json::Value;
use std::{collections::BTreeMap, fs, path::Path};

fn files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn visit(root: &Path, directory: &Path, result: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, result);
            } else {
                result.insert(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .to_owned(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut result = BTreeMap::new();
    visit(root, root, &mut result);
    result
}

#[test]
fn exports_schema_and_typescript_and_matches_checked_in_artifacts() {
    let temp = tempfile::tempdir().unwrap();
    export_protocol(temp.path()).unwrap();
    let schema_path = temp.path().join("protocol.schema.json");
    assert!(
        schema_path.is_file(),
        "the exporter must write a JSON Schema catalog"
    );
    let schema: Value = serde_json::from_slice(&fs::read(schema_path).unwrap()).unwrap();
    assert_eq!(
        schema["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    let definitions = schema["$defs"].as_object().unwrap();
    for name in ["SessionInputParams", "QueuedInput", "Turn"] {
        assert_eq!(
            definitions[name]["properties"]["review"]["type"],
            serde_json::json!(["string", "null"])
        );
        assert!(
            definitions[name]["required"]
                .as_array()
                .unwrap()
                .iter()
                .all(|field| field != "review")
        );
        let ts = fs::read_to_string(temp.path().join(format!("typescript/{name}.ts"))).unwrap();
        assert!(ts.contains("review?: string"), "{ts}");
    }
    let old_input = serde_json::json!({"input":[{"type":"text","text":"hello"}],"mode":"queue","clientName":"old"});
    let parsed: canter_protocol::SessionInputParams =
        serde_json::from_value(old_input.clone()).unwrap();
    assert!(parsed.review.is_none());
    assert_eq!(serde_json::to_value(parsed).unwrap(), old_input);
    for name in [
        "Event",
        "ErrorResponse",
        "ErrorCode",
        "PendingRequest",
        "RequestKind",
        "QueuedInput",
        "SessionSnapshot",
        "SessionCloseReason",
        "Item",
        "InfoParams",
        "InfoResponse",
        "ReasoningEffort",
        "Model",
        "ModelListParams",
        "ModelListResponse",
        "SessionCreateParams",
        "SessionCreateResponse",
        "SessionListParams",
        "SessionListResponse",
        "SessionReadParams",
        "SessionReadResponse",
        "SessionResumeParams",
        "SessionResumeResponse",
        "SessionInputParams",
        "SessionInputResponse",
        "SessionInterruptParams",
        "SessionInterruptResponse",
        "RequestResolveParams",
        "RequestResolveResponse",
        "UserInputRequest",
        "UserInputOption",
        "UserInputAnswer",
        "SessionUpdateParams",
        "SessionUpdateResponse",
        "SessionCompactParams",
        "SessionCompactResponse",
        "SessionCloseParams",
        "SessionCloseResponse",
        "EventsParams",
        "EventsResponse",
    ] {
        assert!(definitions.contains_key(name), "missing schema for {name}");
        assert!(
            temp.path()
                .join("typescript")
                .join(format!("{name}.ts"))
                .is_file(),
            "missing TS for {name}"
        );
    }
    let settings = &definitions["SessionSettings"]["properties"];
    assert!(settings.get("model").is_some());
    assert!(settings.get("approvalPolicy").is_some());
    assert!(settings.get("env").is_none());
    assert!(settings.get("cwd").is_some());
    for name in [
        "SessionSettings",
        "SessionCreateParams",
        "SessionUpdateParams",
    ] {
        let ts = fs::read_to_string(temp.path().join(format!("typescript/{name}.ts"))).unwrap();
        assert!(ts.contains("reasoningEffort?: ReasoningEffort"), "{ts}");
        assert!(
            definitions[name]["required"]
                .as_array()
                .is_none_or(|required| required.iter().all(|field| field != "reasoningEffort"))
        );
    }
    let event_ts = fs::read_to_string(temp.path().join("typescript/Event.ts")).unwrap();
    let model_ts = fs::read_to_string(temp.path().join("typescript/Model.ts")).unwrap();
    assert!(model_ts.contains("inputModalities?: Array<string>"));
    assert!(
        definitions["Model"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .all(|field| field != "inputModalities")
    );
    let input_ts = fs::read_to_string(temp.path().join("typescript/UserInput.ts")).unwrap();
    assert!(input_ts.contains("\"type\": \"localImage\", path: string, name?: string"));
    let image = definitions["UserInput"]["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|variant| variant["properties"]["type"]["const"] == "localImage")
        .unwrap();
    assert_eq!(image["required"], serde_json::json!(["type", "path"]));
    let old_model =
        serde_json::json!({"id":"old", "model":"old", "displayName":"old", "isDefault":true});
    let model: canter_protocol::Model = serde_json::from_value(old_model.clone()).unwrap();
    assert_eq!(serde_json::to_value(model).unwrap(), old_model);
    let resolve_ts =
        fs::read_to_string(temp.path().join("typescript/RequestResolveParams.ts")).unwrap();
    assert!(resolve_ts.contains("decision?: ApprovalDecision"));
    assert!(resolve_ts.contains("answer?: UserInputAnswer"));
    let required = definitions["RequestResolveParams"]["required"]
        .as_array()
        .unwrap();
    assert!(
        !required
            .iter()
            .any(|field| field == "decision" || field == "answer")
    );
    let legacy: canter_protocol::RequestResolveParams =
        serde_json::from_value(serde_json::json!({"decision":"accept","clientName":"old-client"}))
            .unwrap();
    assert_eq!(
        legacy.decision,
        Some(canter_protocol::ApprovalDecision::Accept)
    );
    assert_eq!(legacy.answer, None);
    assert_eq!(
        serde_json::to_value(legacy).unwrap(),
        serde_json::json!({"decision":"accept","clientName":"old-client"})
    );
    assert!(event_ts.contains("seq: number"));
    assert!(event_ts.contains("sessionId: string"));
    let payload_ts = fs::read_to_string(temp.path().join("typescript/EventPayload.ts")).unwrap();
    assert!(payload_ts.contains("\"input.removed\""));
    assert!(payload_ts.contains("closeReason?: SessionCloseReason"));
    let snapshot_ts =
        fs::read_to_string(temp.path().join("typescript/SessionSnapshot.ts")).unwrap();
    assert!(snapshot_ts.contains("closeReason?: SessionCloseReason"));
    assert!(
        definitions["SessionSnapshot"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .all(|field| field != "closeReason")
    );
    assert!(
        payload_ts.contains("turnId?: string"),
        "omittable removal turnId must be optional in TypeScript"
    );
    let item_ts = fs::read_to_string(temp.path().join("typescript/Item.ts")).unwrap();
    assert!(item_ts.contains("aggregatedOutput: string"));
    assert!(item_ts.contains("exitCode: number | null"));
    let patch_ts =
        fs::read_to_string(temp.path().join("typescript/SessionUpdateParams.ts")).unwrap();
    assert!(
        patch_ts.contains("name?: string | null"),
        "PATCH fields must be optional in TypeScript too"
    );
    let generated = files(temp.path());
    let compact_ts =
        fs::read_to_string(temp.path().join("typescript/SessionCompactResponse.ts")).unwrap();
    assert!(compact_ts.contains("turnId?: string"));
    export_protocol(temp.path()).unwrap();
    assert_eq!(files(temp.path()), generated);
    let checked_in = Path::new(env!("CARGO_MANIFEST_DIR")).join("generated");
    assert_eq!(
        files(&checked_in),
        generated,
        "regenerate protocol artifacts after changing wire types"
    );
}

#[test]
fn readme_generation_command_uses_the_main_build_directory() {
    let readme = include_str!("../README.md");
    assert!(readme.contains(
        "canter-worktrees/.target/main cargo run -p canter-protocol --bin export_protocol"
    ));
    assert!(!readme.contains(".target/p1-protocol-store"));
}
