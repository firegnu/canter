use super::*;

impl ChatgptAuth {
    /// Block first, then wait for rotation and revoke its newest token. Like
    /// refresh, the cleanup survives cancellation of the requesting client.
    pub async fn logout(&self, record_id: &str) -> Result<LogoutResult, AuthError> {
        let entry = self.entry(record_id).await?;
        {
            let _publish = entry.publish.lock().unwrap();
            entry.blocked.store(true, Ordering::SeqCst);
            entry.epoch.fetch_add(1, Ordering::SeqCst);
            self.inner.store.begin_logout(record_id, self.host_id())?;
        }
        let auth = self.clone();
        tokio::spawn(async move {
            let mut record = entry.record.lock().await;
            record.sign_out();
            auth.inner.store.save_record(&record, auth.host_id())?;
            let remote_revoked = match &record.pending_revocation {
                Some(token) => auth.inner.http.revoke(token, &record.info.client_id).await,
                None => false,
            };
            record.pending_revocation = None;
            auth.inner.store.save_record(&record, auth.host_id())?;
            Ok(LogoutResult { remote_revoked })
        })
        .await
        .map_err(|_| AuthError::from(ErrorKind::InvalidResponse))?
    }

    /// Resolve one explicitly selected registration. Idle accounts never refresh.
    /// The owned operation completes even if its caller stops waiting: cancelling
    /// an HTTP refresh after rotation must not lose the replacement refresh token.
    pub async fn access_token(&self, record_id: &str) -> Result<AccessToken, AuthError> {
        let entry = self.entry(record_id).await?;
        if entry.blocked.load(Ordering::SeqCst) {
            return Err(ErrorKind::NotLoggedIn.into());
        }
        let auth = self.clone();
        let observed = entry.refreshes.load(Ordering::SeqCst);
        tokio::spawn(async move { auth.token(entry, None, observed).await })
            .await
            .map_err(|_| AuthError::from(ErrorKind::InvalidResponse))?
    }

    /// One recovery budget per inference attempt, carried onto the returned
    /// token. Calling this again on either token fails without another refresh.
    pub async fn recover_unauthorized(
        &self,
        rejected: &mut AccessToken,
    ) -> Result<AccessToken, AuthError> {
        if rejected.recovered {
            return Err(ErrorKind::Unauthorized.into());
        }
        rejected.recovered = true;
        let entry = rejected.entry.clone();
        let current = self.entry(&rejected.record_id).await?;
        if !Arc::ptr_eq(&entry, &current) || rejected.epoch != entry.epoch.load(Ordering::SeqCst) {
            return Err(ErrorKind::Unauthorized.into());
        }
        let previous = rejected.secret.0.clone();
        let auth = self.clone();
        let observed = entry.refreshes.load(Ordering::SeqCst);
        tokio::spawn(async move { auth.token(entry, Some(previous), observed).await })
            .await
            .map_err(|_| AuthError::from(ErrorKind::InvalidResponse))?
    }

    async fn token(
        &self,
        entry: Arc<Entry>,
        rejected: Option<String>,
        observed: u64,
    ) -> Result<AccessToken, AuthError> {
        let mut record = entry.record.lock().await;
        if entry.blocked.load(Ordering::SeqCst) {
            return Err(ErrorKind::NotLoggedIn.into());
        }
        match record.info.status {
            RecordStatus::SignedOut => return Err(ErrorKind::NotLoggedIn.into()),
            RecordStatus::NeedsLogin => return Err(ErrorKind::NeedsLogin.into()),
            RecordStatus::PlanNotAuthorized => return Err(ErrorKind::PlanNotAuthorized.into()),
            RecordStatus::LoggedIn => {}
        }
        if entry.refreshes.load(Ordering::SeqCst) != observed
            && let Some(error) = *entry.refresh_error.lock().unwrap()
        {
            return Err(error);
        }
        let tokens = record.tokens.as_ref().ok_or(ErrorKind::NotLoggedIn)?;
        let now = (self.inner.http.clock)();
        let expires = tokens.received_at.saturating_add(tokens.expires_in);
        let expired = now >= expires;
        let rejected_current = rejected
            .as_ref()
            .is_some_and(|previous| *previous == tokens.access.0);
        let earliest = tokens
            .earliest_refresh_at
            .as_ref()
            .and_then(serde_json::Value::as_u64);
        let too_early = !expired && earliest.is_some_and(|earliest| now < earliest);
        if rejected_current && too_early {
            return Err(ErrorKind::RefreshTooEarly.into());
        }
        if expired || (!too_early && (rejected_current || now.saturating_add(60) >= expires)) {
            let tokens = tokens.clone();
            let outcome = async {
                let Some(refresh) = &tokens.refresh else {
                    self.invalidate(&entry, &mut record)?;
                    return Err(ErrorKind::NeedsLogin.into());
                };
                let fresh = self
                    .inner
                    .http
                    .tokens(
                        &[
                            ("grant_type", "refresh_token"),
                            ("client_id", &record.info.client_id),
                            ("refresh_token", &refresh.0),
                            ("resource", http::RESOURCE),
                        ],
                        Some(&tokens),
                    )
                    .await;
                let mut fresh = match fresh {
                    Ok(tokens) => tokens,
                    Err(error) => {
                        if error.kind == ErrorKind::NeedsLogin {
                            self.invalidate(&entry, &mut record)?;
                        }
                        return Err(error);
                    }
                };
                let identity_result = if let Some(id) = &fresh.id {
                    self.inner
                        .http
                        .identity(id, &record.info.client_id, None)
                        .await
                        .and_then(|identity| {
                            if identity.sub != record.info.subject {
                                Err(ErrorKind::IdentityMismatch.into())
                            } else {
                                Ok(())
                            }
                        })
                } else {
                    fresh.id = tokens.id.clone();
                    Ok(())
                };
                let identity_result = match identity_result {
                    Err(error) if error.kind == ErrorKind::Network => {
                        // Rotation already succeeded. Keep the last verified ID
                        // token until a later refresh can validate its replacement.
                        fresh.id = tokens.id.clone();
                        Ok(())
                    }
                    result => result,
                };
                if let Err(error) = identity_result {
                    {
                        let _publish = entry.publish.lock().unwrap();
                        if entry.blocked.load(Ordering::SeqCst) {
                            record.tokens = Some(fresh);
                            record.sign_out();
                            self.inner.store.save_record(&record, self.host_id())?;
                            return Err(ErrorKind::NotLoggedIn.into());
                        }
                    }
                    if let Some(refresh) = &fresh.refresh {
                        let _ = self
                            .inner
                            .http
                            .revoke(refresh, &record.info.client_id)
                            .await;
                    }
                    self.invalidate(&entry, &mut record)?;
                    return Err(error);
                }
                let _publish = entry.publish.lock().unwrap();
                if entry.blocked.load(Ordering::SeqCst) {
                    // Persist only the newest refresh token for the waiting
                    // logout. Access/ID tokens never become usable again.
                    record.tokens = Some(fresh);
                    record.sign_out();
                    self.inner.store.save_record(&record, self.host_id())?;
                    return Err(ErrorKind::NotLoggedIn.into());
                }
                let mut replacement = record.clone();
                replacement.info.status = fresh.status();
                replacement.tokens = Some(fresh);
                if let Err(error) = self.inner.store.save_record(&replacement, self.host_id()) {
                    // A rotated token cannot safely be retried from the old snapshot.
                    entry.blocked.store(true, Ordering::SeqCst);
                    record.tokens = None;
                    record.info.status = RecordStatus::NeedsLogin;
                    return Err(error);
                }
                *record = replacement;
                Ok(())
            }
            .await;
            *entry.refresh_error.lock().unwrap() = outcome.as_ref().err().copied();
            entry.refreshes.fetch_add(1, Ordering::SeqCst);
            outcome?;
        }
        if record.info.status == RecordStatus::PlanNotAuthorized {
            return Err(ErrorKind::PlanNotAuthorized.into());
        }
        Ok(AccessToken {
            secret: record
                .tokens
                .as_ref()
                .ok_or(ErrorKind::NotLoggedIn)?
                .access
                .clone(),
            record_id: record.info.id.clone(),
            epoch: entry.epoch.load(Ordering::SeqCst),
            entry: entry.clone(),
            recovered: rejected.is_some(),
        })
    }

    fn invalidate(&self, entry: &Entry, record: &mut Record) -> Result<(), AuthError> {
        let _publish = entry.publish.lock().unwrap();
        let mut replacement = record.clone();
        replacement.tokens = None;
        replacement.pending_revocation = None;
        replacement.info.status = if entry.blocked.load(Ordering::SeqCst) {
            RecordStatus::SignedOut
        } else {
            RecordStatus::NeedsLogin
        };
        let saved = self.inner.store.save_record(&replacement, self.host_id());
        // Even a storage failure must stop use of a known-invalid token set.
        *record = replacement;
        entry.epoch.fetch_add(1, Ordering::SeqCst);
        saved
    }
}
