use super::*;

#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub(super) struct Preferences {
    pub current_record: Option<String>,
    pub plan_notice_shown: bool,
}

impl ChatgptAuth {
    pub async fn current_record(&self) -> Option<String> {
        self.inner.preferences.lock().await.current_record.clone()
    }

    pub async fn select(&self, id: &str) -> Result<(), AuthError> {
        self.entry(id).await?;
        let mut preferences = self.inner.preferences.lock().await;
        let mut next = preferences.clone();
        next.current_record = Some(id.into());
        self.inner.store.save_preferences(self.host_id(), &next)?;
        *preferences = next;
        Ok(())
    }

    /// Queried by the model layer per registration, independently of OAuth scope.
    pub async fn background_consent(&self, id: &str) -> Result<bool, AuthError> {
        Ok(self
            .entry(id)
            .await?
            .record
            .lock()
            .await
            .info
            .background_consent)
    }

    pub async fn set_background_consent(
        &self,
        id: &str,
        consent: bool,
    ) -> Result<RecordInfo, AuthError> {
        let entry = self.entry(id).await?;
        let mut record = entry.record.lock().await;
        let _publish = entry.publish.lock().unwrap();
        let mut next = record.clone();
        if entry.blocked.load(Ordering::SeqCst) {
            next.sign_out();
        }
        next.info.background_consent = consent;
        self.inner.store.save_record(&next, self.host_id())?;
        *record = next;
        Ok(record.info())
    }

    pub async fn complete_login(
        &self,
        id: &str,
        consent: bool,
    ) -> Result<(RecordInfo, bool), AuthError> {
        let record = self.set_background_consent(id, consent).await?;
        let mut preferences = self.inner.preferences.lock().await;
        let mut next = preferences.clone();
        next.current_record = Some(id.into());
        let show_notice = record.status == RecordStatus::LoggedIn && !next.plan_notice_shown;
        next.plan_notice_shown |= show_notice;
        self.inner.store.save_preferences(self.host_id(), &next)?;
        *preferences = next;
        Ok((record, show_notice))
    }
}
