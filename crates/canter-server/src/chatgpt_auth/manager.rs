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
        }
        let auth = self.clone();
        tokio::spawn(async move {
            let mut record = entry.record.lock().await;
            let remote_revoked = match record
                .tokens
                .as_ref()
                .and_then(|tokens| tokens.refresh.as_ref())
            {
                Some(token) => auth.inner.http.revoke(token, &record.info.client_id).await,
                None => false,
            };
            record.tokens = None;
            record.info.status = RecordStatus::SignedOut;
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
                    .tokens(&[
                        ("grant_type", "refresh_token"),
                        ("client_id", &record.info.client_id),
                        ("refresh_token", &refresh.0),
                        ("resource", http::RESOURCE),
                    ])
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
                // Rotation is mandatory for a successful refresh response.
                if fresh.refresh.is_none() {
                    return Err(ErrorKind::InvalidResponse.into());
                }
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
                let _publish = entry.publish.lock().unwrap();
                if entry.blocked.load(Ordering::SeqCst) {
                    // Only the waiting logout can use this rotated token, to revoke
                    // the newest session. It is never published to disk or callers.
                    record.tokens = Some(fresh);
                    return Err(ErrorKind::NotLoggedIn.into());
                }
                identity_result?;
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
        replacement.info.status = RecordStatus::NeedsLogin;
        let saved = self.inner.store.save_record(&replacement, self.host_id());
        // Even a storage failure must stop use of a known-invalid token set.
        *record = replacement;
        entry.epoch.fetch_add(1, Ordering::SeqCst);
        saved
    }
}
