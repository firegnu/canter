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
    let event_ts = fs::read_to_string(temp.path().join("typescript/Event.ts")).unwrap();
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
