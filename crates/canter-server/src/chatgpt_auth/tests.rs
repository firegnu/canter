use super::*;
use std::os::unix::fs::PermissionsExt;

#[test]
fn host_identity_survives_reopening_in_private_storage() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("state");
    let auth = ChatgptAuth::open(&home).unwrap();
    let host = auth.host_id().to_owned();
    assert!(host.starts_with("urn:uuid:"));
    assert_eq!(uuid::Uuid::parse_str(&host).unwrap().get_version_num(), 4);
    drop(auth);
    let reopened = ChatgptAuth::open(&home).unwrap();
    assert_eq!(reopened.host_id(), host);
    for (name, mode) in [
        ("auth", 0o700),
        ("auth/chatgpt", 0o700),
        ("auth/chatgpt/host.json", 0o600),
    ] {
        assert_eq!(
            std::fs::metadata(home.join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            mode
        );
    }
}

mod support;
use support::*;

#[tokio::test]
async fn initial_login_exchanges_issued_client_and_persists_validated_identity() {
    let fixture = Fixture::new().await;
    let record = fixture.login(None).await.unwrap();
    assert_eq!(record.subject, "test-subject");
    assert_eq!(record.client_id, "oaiapp_test-one");
    assert_eq!(record.status, RecordStatus::LoggedIn);
    assert_eq!(fixture.auth.records().await, vec![record.clone()]);
    let token = fixture.auth.access_token(&record.id).await.unwrap();
    assert!(token.authorization_header().unwrap().is_sensitive());
    assert_eq!(
        token.authorization_header().unwrap(),
        "Bearer fake-access-1"
    );
    let state = fixture.fake.state.lock().unwrap();
    let authorize = &state.authorize[0];
    let grant = &state.grants[0];
    assert_eq!(authorize["client_id"], "dynamic_agent_client");
    assert_eq!(authorize["agent_name_hint"], "canter");
    assert_eq!(authorize["scope"], SCOPES);
    assert_eq!(authorize["ext_agent_host_id"], fixture.auth.host_id());
    assert_eq!(grant["client_id"], "oaiapp_test-one");
    assert_eq!(grant["redirect_uri"], authorize["redirect_uri"]);
    assert_eq!(grant["resource"], authorize["resource"]);
    assert_eq!(grant["code"], CODE);
    assert_eq!(authorize["code_challenge_method"], "S256");
    use base64::Engine;
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(ring::digest::digest(
        &ring::digest::SHA256,
        grant["code_verifier"].as_bytes(),
    ));
    assert_eq!(authorize["code_challenge"], challenge);
}

#[tokio::test]
async fn returning_login_reuses_binding_with_fresh_secrets_and_optional_callback_client() {
    let fixture = Fixture::new().await;
    let first = fixture.login(None).await.unwrap();
    fixture.fake.state.lock().unwrap().omit_client = true;
    let second = fixture.login(Some(&first.id)).await.unwrap();
    assert_eq!(first, second);
    let state = fixture.fake.state.lock().unwrap();
    let initial = &state.authorize[0];
    let returning = &state.authorize[1];
    assert_eq!(returning["client_id"], first.client_id);
    assert!(!returning.contains_key("agent_name_hint"));
    assert!(!returning.contains_key("id_token_hint"));
    assert!(!returning.contains_key("login_hint"));
    assert_eq!(returning["ext_agent_host_id"], initial["ext_agent_host_id"]);
    for name in ["state", "nonce", "code_challenge"] {
        assert_ne!(initial[name], returning[name]);
    }
    assert_eq!(state.grants[1]["redirect_uri"], returning["redirect_uri"]);
    assert_eq!(state.grants[1]["resource"], initial["resource"]);
    assert_eq!(state.grants[1]["client_id"], first.client_id);
}

#[tokio::test]
async fn rejects_invalid_callbacks_before_code_exchange() {
    for (key, value, expected) in [
        ("state", "incorrect", ErrorKind::StateMismatch),
        ("error", "access_denied", ErrorKind::AccessDenied),
        (
            "client_id",
            "dynamic_agent_client",
            ErrorKind::ClientMismatch,
        ),
    ] {
        let fixture = Fixture::new().await;
        fixture
            .fake
            .state
            .lock()
            .unwrap()
            .callback_override
            .insert(key.into(), value.into());
        assert_eq!(fixture.login(None).await.unwrap_err().kind, expected);
        assert!(fixture.fake.state.lock().unwrap().grants.is_empty());
        assert!(fixture.auth.records().await.is_empty());
    }
    let fixture = Fixture::new().await;
    fixture.fake.state.lock().unwrap().omit_client = true;
    assert_eq!(
        fixture.login(None).await.unwrap_err().kind,
        ErrorKind::ClientMismatch
    );
    assert!(fixture.fake.state.lock().unwrap().grants.is_empty());
}

#[tokio::test]
async fn rejects_each_invalid_id_token_claim_and_signature() {
    for bad in ["signature", "issuer", "audience", "expired", "nonce"] {
        let fixture = Fixture::new().await;
        fixture.fake.state.lock().unwrap().bad_claim = Some(bad);
        assert_eq!(
            fixture.login(None).await.unwrap_err().kind,
            ErrorKind::InvalidIdToken,
            "{bad}"
        );
        assert!(fixture.auth.records().await.is_empty());
    }
}

#[tokio::test]
async fn returning_client_or_subject_mismatch_preserves_old_credentials() {
    let fixture = Fixture::new().await;
    let first = fixture.login(None).await.unwrap();
    fixture.fake.state.lock().unwrap().client_id = "other-client".into();
    assert_eq!(
        fixture.login(Some(&first.id)).await.unwrap_err().kind,
        ErrorKind::ClientMismatch
    );
    assert_eq!(fixture.fake.state.lock().unwrap().grants.len(), 1);
    fixture.fake.state.lock().unwrap().client_id = first.client_id.clone();
    fixture.fake.state.lock().unwrap().subject = "other-user".into();
    assert_eq!(
        fixture.login(Some(&first.id)).await.unwrap_err().kind,
        ErrorKind::IdentityMismatch
    );
    assert_eq!(fixture.auth.records().await, vec![first.clone()]);
    assert_eq!(
        fixture
            .auth
            .access_token(&first.id)
            .await
            .unwrap()
            .authorization_header()
            .unwrap(),
        "Bearer fake-access-1"
    );
}

#[tokio::test]
async fn same_email_registrations_remain_separate_across_restart() {
    let fixture = Fixture::new().await;
    let first = fixture.login(None).await.unwrap();
    fixture.fake.state.lock().unwrap().client_id = "oaiapp_workspace-two".into();
    let second = fixture.login(None).await.unwrap();
    assert_ne!(first.id, second.id);
    assert_eq!(first.email, second.email);
    let Fixture {
        auth,
        fake,
        root,
        now,
    } = fixture;
    drop(auth);
    let reopened = ChatgptAuth::for_test(
        &root.path().join("state"),
        fake.origin.clone(),
        Arc::new(move || now.load(Ordering::SeqCst)),
    );
    assert_eq!(reopened.records().await.len(), 2);
    assert_eq!(
        reopened
            .access_token(&first.id)
            .await
            .unwrap()
            .authorization_header()
            .unwrap(),
        "Bearer fake-access-1"
    );
    assert_eq!(
        reopened
            .access_token(&second.id)
            .await
            .unwrap()
            .authorization_header()
            .unwrap(),
        "Bearer fake-access-2"
    );
    for id in [&first.id, &second.id] {
        assert_eq!(
            std::fs::metadata(root.path().join(format!("state/auth/chatgpt/{id}.json")))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[tokio::test]
async fn signed_in_without_plan_permission_cannot_supply_inference_token() {
    let fixture = Fixture::new().await;
    fixture.fake.state.lock().unwrap().scopes = "openid profile email".into();
    let record = fixture.login(None).await.unwrap();
    assert_eq!(record.status, RecordStatus::PlanNotAuthorized);
    assert_eq!(
        fixture
            .auth
            .access_token(&record.id)
            .await
            .err()
            .unwrap()
            .kind,
        ErrorKind::PlanNotAuthorized
    );
    assert_eq!(fixture.fake.state.lock().unwrap().grants.len(), 1);
}

#[tokio::test]
async fn cancellation_timeout_and_completion_close_callback_and_remove_transaction() {
    let fixture = Fixture::new().await;
    let attempt = fixture.auth.start_login(None).await.unwrap();
    let url = reqwest::Url::parse(attempt.authorization_url()).unwrap();
    let redirect = url
        .query_pairs()
        .find(|(key, _)| key == "redirect_uri")
        .unwrap()
        .1
        .to_string();
    let id = attempt.id.clone();
    assert!(fixture.auth.cancel_login(&id).await);
    assert_eq!(attempt.wait().await.unwrap_err().kind, ErrorKind::Cancelled);
    assert!(reqwest::get(&redirect).await.is_err());
    assert!(!fixture.auth.cancel_login(&id).await);
    let attempt = fixture.auth.start_login(None).await.unwrap();
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(601)).await;
    assert_eq!(attempt.wait().await.unwrap_err().kind, ErrorKind::TimedOut);
    tokio::time::resume();
    let attempt = fixture.auth.start_login(None).await.unwrap();
    let url = reqwest::Url::parse(attempt.authorization_url()).unwrap();
    let redirect = url
        .query_pairs()
        .find(|(key, _)| key == "redirect_uri")
        .unwrap()
        .1
        .to_string();
    let page = fixture.fake.callback(&attempt).await;
    assert_eq!(page.headers()["referrer-policy"], "no-referrer");
    let page = page.text().await.unwrap();
    assert!(!page.contains(CODE));
    assert!(!page.contains("http"));
    let id = attempt.id.clone();
    attempt.wait().await.unwrap();
    assert!(reqwest::get(&redirect).await.is_err());
    assert!(!fixture.auth.cancel_login(&id).await);
}

#[test]
fn credential_storage_rejects_symlinks_wide_permissions_and_second_manager() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("state");
    let auth = ChatgptAuth::open(&home).unwrap();
    assert_eq!(
        ChatgptAuth::open(&home).err().unwrap().kind,
        ErrorKind::Storage
    );
    drop(auth);
    let host = home.join("auth/chatgpt/host.json");
    std::fs::set_permissions(&host, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        ChatgptAuth::open(&home).err().unwrap().kind,
        ErrorKind::Storage
    );
    std::fs::set_permissions(&host, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::rename(&host, root.path().join("original.json")).unwrap();
    symlink(root.path().join("original.json"), &host).unwrap();
    assert_eq!(
        ChatgptAuth::open(&home).err().unwrap().kind,
        ErrorKind::Storage
    );
    std::fs::remove_file(&host).unwrap();
    std::fs::rename(root.path().join("original.json"), &host).unwrap();
    std::fs::rename(home.join("auth/chatgpt"), root.path().join("original-dir")).unwrap();
    symlink(root.path().join("original-dir"), home.join("auth/chatgpt")).unwrap();
    assert_eq!(
        ChatgptAuth::open(&home).err().unwrap().kind,
        ErrorKind::Storage
    );
}

#[tokio::test]
async fn concurrent_token_requests_refresh_once_and_replace_the_entire_persisted_set() {
    let fixture = Fixture::new().await;
    let record = fixture.login(None).await.unwrap();
    fixture.now.store(NOW + 3540, Ordering::SeqCst);
    fixture.fake.state.lock().unwrap().earliest = Some(serde_json::json!(NOW + 7100));
    fixture.fake.state.lock().unwrap().scopes = format!("{SCOPES} extra_scope");
    let (first, second) = tokio::join!(
        fixture.auth.access_token(&record.id),
        fixture.auth.access_token(&record.id)
    );
    assert_eq!(
        first.unwrap().authorization_header().unwrap(),
        "Bearer fake-access-2"
    );
    assert_eq!(
        second.unwrap().authorization_header().unwrap(),
        "Bearer fake-access-2"
    );
    {
        let state = fixture.fake.state.lock().unwrap();
        assert_eq!(state.grants.len(), 2);
        assert_eq!(state.grants[1]["refresh_token"], "fake-refresh-1");
        assert_eq!(state.grants[1]["client_id"], record.client_id);
        assert_eq!(state.grants[1]["resource"], "https://api.openai.com/v1");
        assert!(!state.grants[1].contains_key("scope"));
    }
    let contents: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            fixture
                .root
                .path()
                .join(format!("state/auth/chatgpt/{}.json", record.id)),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(contents["tokens"]["refresh"], "fake-refresh-2");
    assert_eq!(contents["tokens"]["received_at"], NOW + 3540);
    assert_eq!(contents["tokens"]["expires_in"], 3600);
    assert_eq!(contents["tokens"]["earliest_refresh_at"], NOW + 7100);
    assert!(
        contents["tokens"]["scopes"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("extra_scope"))
    );
    let Fixture {
        auth,
        fake,
        root,
        now,
    } = fixture;
    drop(auth);
    now.store(NOW + 7140, Ordering::SeqCst);
    let reopened = ChatgptAuth::for_test(
        &root.path().join("state"),
        fake.origin.clone(),
        Arc::new(move || now.load(Ordering::SeqCst)),
    );
    assert_eq!(
        reopened
            .access_token(&record.id)
            .await
            .unwrap()
            .authorization_header()
            .unwrap(),
        "Bearer fake-access-3"
    );
    assert_eq!(
        fake.state.lock().unwrap().grants[2]["refresh_token"],
        "fake-refresh-2"
    );
}

#[tokio::test]
async fn earliest_refresh_integer_missing_and_unparseable_follow_expiry() {
    for earliest in [
        Some(serde_json::json!(NOW + 3590)),
        None,
        Some(serde_json::json!({"unknown":"format"})),
    ] {
        let fixture = Fixture::new().await;
        fixture.fake.state.lock().unwrap().earliest = earliest.clone();
        let record = fixture.login(None).await.unwrap();
        fixture.now.store(NOW + 3539, Ordering::SeqCst);
        fixture.auth.access_token(&record.id).await.unwrap();
        assert_eq!(fixture.fake.state.lock().unwrap().grants.len(), 1);
        fixture.now.store(NOW + 3540, Ordering::SeqCst);
        fixture.auth.access_token(&record.id).await.unwrap();
        if earliest
            .as_ref()
            .and_then(serde_json::Value::as_u64)
            .is_some()
        {
            assert_eq!(fixture.fake.state.lock().unwrap().grants.len(), 1);
            fixture.now.store(NOW + 3590, Ordering::SeqCst);
            fixture.auth.access_token(&record.id).await.unwrap();
        }
        assert_eq!(fixture.fake.state.lock().unwrap().grants.len(), 2);
        let stored: serde_json::Value = serde_json::from_slice(
            &std::fs::read(
                fixture
                    .root
                    .path()
                    .join(format!("state/auth/chatgpt/{}.json", record.id)),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            stored["tokens"]["earliest_refresh_at"],
            earliest.unwrap_or(serde_json::Value::Null)
        );
    }
    let fixture = Fixture::new().await;
    fixture.fake.state.lock().unwrap().earliest = Some(serde_json::json!(NOW + 10000));
    let record = fixture.login(None).await.unwrap();
    fixture.now.store(NOW + 3600, Ordering::SeqCst);
    fixture.auth.access_token(&record.id).await.unwrap();
    assert_eq!(fixture.fake.state.lock().unwrap().grants.len(), 2);
}

#[tokio::test]
async fn terminal_refresh_errors_clear_tokens_and_preserve_registration_for_relogin() {
    for code in [
        "invalid_grant",
        "invalid_refresh_token",
        "token_expired",
        "refresh_token_expired",
        "refresh_token_invalidated",
        "refresh_token_reused",
    ] {
        let fixture = Fixture::new().await;
        let record = fixture.login(None).await.unwrap();
        let host = fixture.auth.host_id().to_owned();
        fixture.now.store(NOW + 3600, Ordering::SeqCst);
        fixture.fake.state.lock().unwrap().error = Some((400, code.into()));
        assert_eq!(
            fixture
                .auth
                .access_token(&record.id)
                .await
                .err()
                .unwrap()
                .kind,
            ErrorKind::NeedsLogin,
            "{code}"
        );
        let path = fixture
            .root
            .path()
            .join(format!("state/auth/chatgpt/{}.json", record.id));
        let stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(stored["tokens"].is_null());
        assert_eq!(
            fixture.auth.records().await[0].status,
            RecordStatus::NeedsLogin
        );
        assert_eq!(fixture.auth.host_id(), host);
        fixture.fake.state.lock().unwrap().error = None;
        let again = fixture.login(Some(&record.id)).await.unwrap();
        assert_eq!(again.id, record.id);
        assert_eq!(
            fixture.fake.state.lock().unwrap().authorize[1]["client_id"],
            record.client_id
        );
    }
}

#[tokio::test]
async fn temporary_and_client_errors_preserve_the_entire_credential_file() {
    for (status, code, kind) in [
        (503, "unavailable", ErrorKind::Network),
        (400, "invalid_client", ErrorKind::ClientConfiguration),
    ] {
        let fixture = Fixture::new().await;
        let record = fixture.login(None).await.unwrap();
        let path = fixture
            .root
            .path()
            .join(format!("state/auth/chatgpt/{}.json", record.id));
        let before = std::fs::read(&path).unwrap();
        fixture.now.store(NOW + 3600, Ordering::SeqCst);
        fixture.fake.state.lock().unwrap().error = Some((status, code.into()));
        let error = fixture.auth.access_token(&record.id).await.err().unwrap();
        assert_eq!(error.kind, kind);
        assert_eq!(error.status, Some(status));
        assert_eq!(before, std::fs::read(&path).unwrap());
        fixture.fake.state.lock().unwrap().error = None;
        fixture.auth.access_token(&record.id).await.unwrap();
        assert_eq!(
            fixture.fake.state.lock().unwrap().grants[2]["refresh_token"],
            "fake-refresh-1"
        );
    }
}

#[tokio::test]
async fn unauthorized_recovery_has_one_budget_and_reuses_concurrent_rotation() {
    let fixture = Fixture::new().await;
    let record = fixture.login(None).await.unwrap();
    let mut first = fixture.auth.access_token(&record.id).await.unwrap();
    let mut second = fixture.auth.access_token(&record.id).await.unwrap();
    let (first_result, second_result) = tokio::join!(
        fixture.auth.recover_unauthorized(&mut first),
        fixture.auth.recover_unauthorized(&mut second)
    );
    let mut recovered = first_result.unwrap();
    assert_eq!(
        recovered.authorization_header().unwrap(),
        "Bearer fake-access-2"
    );
    assert_eq!(
        second_result.unwrap().authorization_header().unwrap(),
        "Bearer fake-access-2"
    );
    assert_eq!(fixture.fake.state.lock().unwrap().grants.len(), 2);
    assert_eq!(
        fixture
            .auth
            .recover_unauthorized(&mut recovered)
            .await
            .err()
            .unwrap()
            .kind,
        ErrorKind::Unauthorized
    );
    assert_eq!(
        fixture
            .auth
            .recover_unauthorized(&mut first)
            .await
            .err()
            .unwrap()
            .kind,
        ErrorKind::Unauthorized
    );
    assert_eq!(fixture.fake.state.lock().unwrap().grants.len(), 2);
}

#[tokio::test]
async fn logout_revokes_and_clears_tokens_but_keeps_binding_and_host() {
    let fixture = Fixture::new().await;
    let record = fixture.login(None).await.unwrap();
    let old_token = fixture.auth.access_token(&record.id).await.unwrap();
    let host = fixture.auth.host_id().to_owned();
    let result = fixture.auth.logout(&record.id).await.unwrap();
    assert!(result.remote_revoked);
    assert_eq!(fixture.auth.host_id(), host);
    assert_eq!(
        fixture.auth.records().await[0].status,
        RecordStatus::SignedOut
    );
    assert_eq!(
        fixture
            .auth
            .access_token(&record.id)
            .await
            .err()
            .unwrap()
            .kind,
        ErrorKind::NotLoggedIn
    );
    assert_eq!(
        old_token.authorization_header().unwrap_err().kind,
        ErrorKind::NotLoggedIn
    );
    {
        let state = fixture.fake.state.lock().unwrap();
        assert_eq!(state.revocations.len(), 1);
        assert_eq!(state.revocations[0]["token"], "fake-refresh-1");
        assert_eq!(state.revocations[0]["token_type_hint"], "refresh_token");
        assert_eq!(state.revocations[0]["client_id"], record.client_id);
    }
    let stored: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            fixture
                .root
                .path()
                .join(format!("state/auth/chatgpt/{}.json", record.id)),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(stored["tokens"].is_null());
    assert_eq!(fixture.login(Some(&record.id)).await.unwrap(), record);
    assert_eq!(
        old_token.authorization_header().unwrap_err().kind,
        ErrorKind::NotLoggedIn
    );
}

#[tokio::test]
async fn remote_revocation_failure_is_bounded_and_still_clears_local_tokens() {
    for (status, attempts) in [(503, 3), (400, 1)] {
        let fixture = Fixture::new().await;
        let record = fixture.login(None).await.unwrap();
        fixture.fake.state.lock().unwrap().revoke_status = status;
        assert!(
            !fixture
                .auth
                .logout(&record.id)
                .await
                .unwrap()
                .remote_revoked
        );
        assert_eq!(
            fixture.fake.state.lock().unwrap().revocations.len(),
            attempts
        );
        let Fixture {
            auth,
            fake,
            root,
            now,
        } = fixture;
        drop(auth);
        let reopened = ChatgptAuth::for_test(
            &root.path().join("state"),
            fake.origin.clone(),
            Arc::new(move || now.load(Ordering::SeqCst)),
        );
        assert_eq!(reopened.records().await[0].status, RecordStatus::SignedOut);
        assert_eq!(
            reopened.access_token(&record.id).await.err().unwrap().kind,
            ErrorKind::NotLoggedIn
        );
    }
}

#[tokio::test]
async fn logout_blocks_new_requests_and_revokes_inflight_rotation_without_resurrection() {
    let fixture = Fixture::new().await;
    let record = fixture.login(None).await.unwrap();
    let old = fixture.auth.access_token(&record.id).await.unwrap();
    fixture.now.store(NOW + 3600, Ordering::SeqCst);
    let release = Arc::new(tokio::sync::Notify::new());
    fixture.fake.state.lock().unwrap().refresh_release = Some(release.clone());
    let started = fixture.fake.state.lock().unwrap().refresh_started.clone();
    let auth = fixture.auth.clone();
    let id = record.id.clone();
    let refreshing = tokio::spawn(async move { auth.access_token(&id).await });
    started.notified().await;
    let auth = fixture.auth.clone();
    let id = record.id.clone();
    let logout = tokio::spawn(async move { auth.logout(&id).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while old.authorization_header().is_ok() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let blocked = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        fixture.auth.access_token(&record.id),
    )
    .await
    .unwrap();
    assert_eq!(blocked.err().unwrap().kind, ErrorKind::NotLoggedIn);
    release.notify_one();
    assert_eq!(
        refreshing.await.unwrap().err().unwrap().kind,
        ErrorKind::NotLoggedIn
    );
    assert!(logout.await.unwrap().unwrap().remote_revoked);
    assert_eq!(
        fixture.fake.state.lock().unwrap().revocations[0]["token"],
        "fake-refresh-2"
    );
    assert_eq!(
        fixture.auth.records().await[0].status,
        RecordStatus::SignedOut
    );
    let stored: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            fixture
                .root
                .path()
                .join(format!("state/auth/chatgpt/{}.json", record.id)),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(stored["tokens"].is_null());
}

#[tokio::test]
async fn cancelling_token_caller_does_not_lose_a_rotated_refresh_token() {
    let fixture = Fixture::new().await;
    let record = fixture.login(None).await.unwrap();
    fixture.now.store(NOW + 3600, Ordering::SeqCst);
    let release = Arc::new(tokio::sync::Notify::new());
    fixture.fake.state.lock().unwrap().refresh_release = Some(release.clone());
    let started = fixture.fake.state.lock().unwrap().refresh_started.clone();
    let auth = fixture.auth.clone();
    let id = record.id.clone();
    let caller = tokio::spawn(async move { auth.access_token(&id).await });
    started.notified().await;
    caller.abort();
    let _ = caller.await;
    release.notify_one();
    assert_eq!(
        fixture
            .auth
            .access_token(&record.id)
            .await
            .unwrap()
            .authorization_header()
            .unwrap(),
        "Bearer fake-access-2"
    );
    assert_eq!(fixture.fake.state.lock().unwrap().grants.len(), 2);
}

#[tokio::test]
async fn concurrent_temporary_refresh_failure_is_shared() {
    let fixture = Fixture::new().await;
    let record = fixture.login(None).await.unwrap();
    fixture.now.store(NOW + 3600, Ordering::SeqCst);
    fixture.fake.state.lock().unwrap().error = Some((503, "temporary".into()));
    let release = Arc::new(tokio::sync::Notify::new());
    fixture.fake.state.lock().unwrap().refresh_release = Some(release.clone());
    let started = fixture.fake.state.lock().unwrap().refresh_started.clone();
    let auth = fixture.auth.clone();
    let id = record.id.clone();
    let first = tokio::spawn(async move { auth.access_token(&id).await });
    started.notified().await;
    // Poll the second request while the first still owns the refresh operation.
    let second = fixture.auth.access_token(&record.id);
    tokio::pin!(second);
    assert!(futures_util::poll!(&mut second).is_pending());
    fixture.fake.state.lock().unwrap().refresh_release = None;
    release.notify_one();
    assert_eq!(first.await.unwrap().err().unwrap().kind, ErrorKind::Network);
    assert_eq!(second.await.err().unwrap().kind, ErrorKind::Network);
    assert_eq!(fixture.fake.state.lock().unwrap().grants.len(), 2);
}

#[tokio::test]
async fn credential_record_symlink_is_rejected_on_load_and_replacement() {
    let fixture = Fixture::new().await;
    let record = fixture.login(None).await.unwrap();
    let path = fixture
        .root
        .path()
        .join(format!("state/auth/chatgpt/{}.json", record.id));
    let original = fixture.root.path().join("record-original.json");
    std::fs::rename(&path, &original).unwrap();
    std::os::unix::fs::symlink(&original, &path).unwrap();
    let before = std::fs::read(&original).unwrap();
    assert_eq!(
        fixture.login(Some(&record.id)).await.unwrap_err().kind,
        ErrorKind::Storage
    );
    assert_eq!(std::fs::read(&original).unwrap(), before);
    let Fixture {
        auth,
        fake,
        root,
        now,
    } = fixture;
    drop(auth);
    let clock = now.clone();
    let result = ChatgptAuth::with_http(
        &root.path().join("state"),
        http::Http::for_test(
            fake.origin.clone(),
            Arc::new(move || clock.load(Ordering::SeqCst)),
        ),
    );
    assert_eq!(result.err().unwrap().kind, ErrorKind::Storage);
    std::fs::remove_file(&path).unwrap();
    std::fs::rename(original, &path).unwrap();
    let reopened = ChatgptAuth::for_test(
        &root.path().join("state"),
        fake.origin.clone(),
        Arc::new(move || now.load(Ordering::SeqCst)),
    );
    assert_eq!(reopened.records().await, vec![record]);
}

#[tokio::test]
async fn network_failure_preserves_credentials_and_logout_reports_unconfirmed_revocation() {
    let fixture = Fixture::new().await;
    let record = fixture.login(None).await.unwrap();
    let path = fixture
        .root
        .path()
        .join(format!("state/auth/chatgpt/{}.json", record.id));
    let before = std::fs::read(&path).unwrap();
    fixture.now.store(NOW + 3600, Ordering::SeqCst);
    fixture.fake.stop().await;
    assert_eq!(
        fixture
            .auth
            .access_token(&record.id)
            .await
            .err()
            .unwrap()
            .kind,
        ErrorKind::Network
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(
        !fixture
            .auth
            .logout(&record.id)
            .await
            .unwrap()
            .remote_revoked
    );
    let after: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(after["tokens"].is_null());
}

#[tokio::test]
async fn untrusted_discovery_endpoints_are_rejected_before_credentials_are_sent() {
    for field in ["issuer", "jwks_uri", "revocation_endpoint"] {
        let fixture = Fixture::new().await;
        let mut discovery = serde_json::json!({
            "issuer":fixture.fake.origin,"jwks_uri":format!("{}/.well-known/jwks.json",fixture.fake.origin),
            "revocation_endpoint":format!("{}/revoke",fixture.fake.origin),
        });
        discovery[field] = serde_json::json!("https://untrusted.example.test/endpoint");
        fixture.fake.state.lock().unwrap().discovery_override = Some(discovery);
        assert_eq!(
            fixture.login(None).await.unwrap_err().kind,
            ErrorKind::InvalidResponse
        );
        assert!(fixture.auth.records().await.is_empty());
    }
}

#[tokio::test]
async fn token_redirect_is_not_followed_and_error_body_is_not_retained() {
    let fixture = Fixture::new().await;
    let record = fixture.login(None).await.unwrap();
    fixture.now.store(NOW + 3600, Ordering::SeqCst);
    fixture.fake.state.lock().unwrap().error = Some((302, format!("{CODE} fake-access-1")));
    let error = fixture.auth.access_token(&record.id).await.err().unwrap();
    assert_eq!(error.status, Some(302));
    assert_eq!(error.code, None);
    assert_eq!(fixture.fake.state.lock().unwrap().grants.len(), 2);
    assert!(std::error::Error::source(&error).is_none());
    assert!(!format!("{error:?} {error}").contains(CODE));
}

#[tokio::test]
async fn error_diagnostics_logs_stdout_and_stderr_exclude_oauth_secrets() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let state = root.path().join("state");
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "chatgpt_auth::tests::redaction_process",
            "--nocapture",
        ])
        .env("CANTER_AUTH_REDACTION_CHILD", "1")
        .env("HOME", &home)
        .env("CANTER_HOME", &state)
        .kill_on_drop(true);
    let output = tokio::time::timeout(std::time::Duration::from_secs(30), command.output())
        .await
        .unwrap()
        .unwrap();
    assert!(output.status.success(), "redaction subprocess failed");
    let log = std::fs::read(home.join("auth-test.log")).unwrap();
    assert!(!log.is_empty());
    let recorded: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.join("expected-secrets.json")).unwrap())
            .unwrap();
    for secret in recorded.as_array().unwrap() {
        let secret = secret.as_str().unwrap();
        for bytes in [&output.stdout, &output.stderr, &log] {
            assert!(
                !String::from_utf8_lossy(bytes).contains(secret),
                "secret escaped into captured diagnostics"
            );
        }
    }
}

#[tokio::test]
async fn redaction_process() {
    if std::env::var_os("CANTER_AUTH_REDACTION_CHILD").is_none() {
        return;
    }
    let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    let state = crate::state_directory().unwrap();
    let fake = Fake::new().await;
    let clock = fake.state.lock().unwrap().now.clone();
    let now = clock.clone();
    let auth = ChatgptAuth::for_test(
        &state,
        fake.origin.clone(),
        Arc::new(move || clock.load(Ordering::SeqCst)),
    );
    let attempt = auth.start_login(None).await.unwrap();
    let authorization_url = attempt.authorization_url().to_owned();
    let state_secret = reqwest::Url::parse(&authorization_url)
        .unwrap()
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .to_string();
    fake.callback(&attempt).await;
    let record = attempt.wait().await.unwrap();
    let stored: serde_json::Value = serde_json::from_slice(
        &std::fs::read(state.join(format!("auth/chatgpt/{}.json", record.id))).unwrap(),
    )
    .unwrap();
    let secrets = serde_json::json!([
        "fake-access-1",
        "fake-refresh-1",
        CODE,
        authorization_url,
        state_secret,
        stored["tokens"]["id"]
    ]);
    std::fs::write(
        home.join("expected-secrets.json"),
        serde_json::to_vec(&secrets).unwrap(),
    )
    .unwrap();
    let file = std::fs::File::create(home.join("auth-test.log")).unwrap();
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || file.try_clone().unwrap())
        .try_init()
        .unwrap();
    now.store(NOW + 3600, Ordering::SeqCst);
    for (status, code) in [
        (503, "temporarily_unavailable"),
        (400, "invalid_client"),
        (400, CODE),
        (400, "invalid_grant"),
    ] {
        fake.state.lock().unwrap().error = Some((status, code.into()));
        let error = auth.access_token(&record.id).await.err().unwrap();
        assert!(std::error::Error::source(&error).is_none());
        println!("{error:?}");
        eprintln!("{error}");
        tracing::error!(?error, "credential operation failed");
    }
}

#[tokio::test]
async fn logout_revokes_rotated_refresh_token_even_if_new_id_token_is_invalid() {
    let fixture = Fixture::new().await;
    let record = fixture.login(None).await.unwrap();
    let old = fixture.auth.access_token(&record.id).await.unwrap();
    fixture.now.store(NOW + 3600, Ordering::SeqCst);
    let release = Arc::new(tokio::sync::Notify::new());
    {
        let mut state = fixture.fake.state.lock().unwrap();
        state.refresh_release = Some(release.clone());
        state.bad_claim = Some("signature");
    }
    let started = fixture.fake.state.lock().unwrap().refresh_started.clone();
    let auth = fixture.auth.clone();
    let id = record.id.clone();
    let refreshing = tokio::spawn(async move { auth.access_token(&id).await });
    started.notified().await;
    let auth = fixture.auth.clone();
    let id = record.id.clone();
    let logout = tokio::spawn(async move { auth.logout(&id).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while old.authorization_header().is_ok() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    release.notify_one();
    assert!(refreshing.await.unwrap().is_err());
    assert!(logout.await.unwrap().unwrap().remote_revoked);
    assert_eq!(
        fixture.fake.state.lock().unwrap().revocations[0]["token"],
        "fake-refresh-2"
    );
}
