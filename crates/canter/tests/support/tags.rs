use super::*;
use axum::routing::get;
use canter_tui::{Connected, Launch, Open, SocketConnector};

#[tokio::test]
async fn tagged_creation_rejects_servers_without_tags_before_sending() {
    for features in [None, Some(json!([]))] {
        let root = tempfile::Builder::new()
            .prefix("tags-")
            .tempdir_in("/tmp")
            .unwrap();
        let mut info = json!({"serverVersion":"old", "protocolVersion":1,"sessions":[]});
        if let Some(features) = features {
            info["features"] = features;
        }
        let requests = Arc::new(AtomicU64::new(0));
        let received = requests.clone();
        let app = Router::new()
            .route(
                "/v1/info",
                get(move || {
                    let info = info.clone();
                    async move { Json(info) }
                }),
            )
            .route(
                "/v1/sessions",
                post(move || {
                    received.fetch_add(1, Ordering::SeqCst);
                    async {
                        (
                            axum::http::StatusCode::BAD_REQUEST,
                            Json(json!({"code":"invalidRequest","message":"unexpected creation"})),
                        )
                    }
                }),
            );
        let socket = root.path().join("server.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let (keys, input) = canter_tui::input();
        let backend = ratatui::Terminal::new(Screen {
            backend: Arc::new(Mutex::new(TestBackend::new(WIDTH, HEIGHT))),
            keys,
            frames: Frames::default(),
        })
        .unwrap();
        let result = run_on(
            backend,
            input,
            Launch {
                connected: Connected {
                    client: Client::new(&socket).unwrap(),
                    warning: None,
                    server: None,
                },
                connector: SocketConnector(socket),
                open: Open::New {
                    tags: BTreeMap::from([("task".into(), "tagged".into())]),
                    additional_directories: vec![],
                    cwd: root.path().display().to_string(),
                    model: "fake".into(),
                    approval_policy: PermissionMode::AlwaysAsk,
                    first: None,
                },
                env: BTreeMap::new(),
                home: Some(root.path().display().to_string()),
                client_name: "tags".into(),
                version: canter::VERSION.into(),
            },
        )
        .await;
        server.abort();
        let _ = server.await;
        assert_eq!(
            requests.load(Ordering::SeqCst),
            0,
            "unsupported tags must not be sent"
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("canter serve --replace")
        );
    }
}
