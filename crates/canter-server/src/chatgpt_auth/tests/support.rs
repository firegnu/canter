use super::*;
pub use crate::chatgpt_auth::test_support::*;
pub struct Fixture {
    pub auth: ChatgptAuth,
    pub fake: Fake,
    pub root: tempfile::TempDir,
    pub now: Arc<AtomicU64>,
}
impl Fixture {
    pub async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let fake = Fake::new().await;
        let now = fake.state.lock().unwrap().now.clone();
        let clock = now.clone();
        let auth = ChatgptAuth::for_test(
            &root.path().join("state"),
            fake.origin.clone(),
            Arc::new(move || clock.load(Ordering::SeqCst)),
        );
        Self {
            auth,
            fake,
            root,
            now,
        }
    }
    pub async fn login(&self, record: Option<&str>) -> Result<RecordInfo, AuthError> {
        let attempt = self.auth.start_login(record).await?;
        self.fake.callback(&attempt).await;
        attempt.wait().await
    }
}
