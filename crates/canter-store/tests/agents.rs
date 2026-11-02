use canter_protocol::*;
use canter_store::Store;

#[test]
fn child_is_hidden_from_default_list_but_readable_by_id() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::new(root.path()).unwrap();
    let settings = SessionSnapshot::empty("parent").settings;
    drop(store.create("parent", settings.clone(), 1).unwrap());
    let mut child = store.create("child", settings, 2).unwrap();
    child
        .append(
            EventPayload::SessionParent {
                parent: SessionParent {
                    agent_type: Default::default(),
                    session_id: "parent".into(),
                    turn_id: "turn".into(),
                    task: "Check parser".into(),
                },
            },
            3,
        )
        .unwrap();
    drop(child);
    assert_eq!(
        store
            .list()
            .unwrap()
            .iter()
            .map(|s| s.id.as_str())
            .collect::<Vec<_>>(),
        ["parent"]
    );
    let saved = store.open("child").unwrap();
    assert_eq!(
        saved.snapshot().parent.as_ref().unwrap().task,
        "Check parser"
    );
}
