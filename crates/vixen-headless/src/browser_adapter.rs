//! Thin typed adapter over the engine-owned browser core.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use vixen_api::{
    BrowserCommand, BrowserCommandResult, BrowserEvent, BrowsingContextId, BrowsingContextState,
    DocumentTextKind, ElementInfo, EvaluationResult, FocusProjection, FormSubmissionInfo,
    NavigationId, NavigationPhase, ScriptValue,
};
use vixen_engine::browser::{BrowserConfig, EngineBrowserHandle, spawn_browser};

const NAVIGATION_WAIT_TIMEOUT: Duration = Duration::from_secs(35);

/// Owns an ephemeral profile or names a persistent profile without owning it.
pub(crate) enum BrowserProfile {
    Ephemeral(tempfile::TempDir),
    Persistent(PathBuf),
}

impl BrowserProfile {
    pub(crate) fn open(
        profile_dir: Option<&Path>,
        temporary_prefix: &str,
        description: &str,
    ) -> Result<Self, String> {
        match profile_dir {
            Some(root) => {
                std::fs::create_dir_all(root).map_err(|error| {
                    format!(
                        "create {description} profile directory {}: {error}",
                        root.display()
                    )
                })?;
                Ok(Self::Persistent(root.to_path_buf()))
            }
            None => tempfile::Builder::new()
                .prefix(temporary_prefix)
                .tempdir()
                .map(Self::Ephemeral)
                .map_err(|error| format!("create temporary {description} profile: {error}")),
        }
    }

    pub(crate) fn database_path(&self) -> PathBuf {
        match self {
            Self::Ephemeral(root) => root.path().join("profile.redb"),
            Self::Persistent(root) => root.join("profile.redb"),
        }
    }
}

/// One browser context for a headless CLI action.
///
/// The handle is declared before the temporary profile so the engine thread and
/// open store are dropped before the profile directory is removed.
pub(crate) struct BrowserSession {
    handle: EngineBrowserHandle,
    context_id: BrowsingContextId,
    _profile: BrowserProfile,
}

impl BrowserSession {
    pub(crate) fn load(url: &str, profile_dir: Option<&Path>) -> Result<Self, String> {
        let profile = BrowserProfile::open(profile_dir, "vixen-headless-", "browser")?;
        let mut handle = spawn_browser(BrowserConfig::new(profile.database_path()))
            .map_err(|error| error.to_string())?;
        let context_id = match handle
            .dispatch(BrowserCommand::CreateBrowsingContext)
            .map_err(|error| error.to_string())?
        {
            BrowserCommandResult::BrowsingContextCreated { context_id } => context_id,
            result => return Err(format!("unexpected create-context result: {result:?}")),
        };
        let mut session = Self {
            handle,
            context_id,
            _profile: profile,
        };
        session.navigate(url)?;
        Ok(session)
    }

    pub(crate) fn evaluate(&mut self, source: &str) -> Result<ScriptValue, String> {
        Ok(self.evaluate_result(source)?.value)
    }

    fn evaluate_result(&mut self, source: &str) -> Result<EvaluationResult, String> {
        let state = self.state()?;
        self.evaluate_result_in_state(source, state)
    }

    fn evaluate_result_in_state(
        &mut self,
        source: &str,
        state: BrowsingContextState,
    ) -> Result<EvaluationResult, String> {
        let runtime_context_id = state
            .runtime_context_id
            .ok_or_else(|| "loaded document has no script runtime".to_owned())?;
        match self
            .handle
            .dispatch(BrowserCommand::Evaluate {
                context_id: self.context_id,
                document_id: state.document_id,
                runtime_context_id,
                source: source.to_owned(),
            })
            .map_err(|error| error.to_string())?
        {
            BrowserCommandResult::Evaluation(evaluation) => Ok(evaluation),
            result => Err(format!("unexpected evaluation result: {result:?}")),
        }
    }

    pub(crate) fn query_selector_all(
        &mut self,
        selector: &str,
        viewport: (u32, u32),
    ) -> Result<Vec<ElementInfo>, String> {
        let state = self.state()?;
        match self
            .handle
            .dispatch(BrowserCommand::QuerySelectorAll {
                context_id: self.context_id,
                document_id: state.document_id,
                selector: selector.to_owned(),
                viewport,
            })
            .map_err(|error| error.to_string())?
        {
            BrowserCommandResult::SelectorMatches(matches) => Ok(matches),
            result => Err(format!("unexpected selector result: {result:?}")),
        }
    }

    pub(crate) fn current_url(&mut self) -> Result<String, String> {
        Ok(self.state()?.url)
    }

    pub(crate) fn document_text(
        &mut self,
        kind: DocumentTextKind,
        viewport: (u32, u32),
    ) -> Result<String, String> {
        let state = self.state()?;
        match self
            .handle
            .dispatch(BrowserCommand::DocumentText {
                context_id: self.context_id,
                document_id: state.document_id,
                viewport,
                kind,
            })
            .map_err(|error| error.to_string())?
        {
            BrowserCommandResult::DocumentText(text) => Ok(text),
            result => Err(format!("unexpected document-text result: {result:?}")),
        }
    }

    pub(crate) fn focus_projection(&mut self, element_id: &str) -> Result<FocusProjection, String> {
        let state = self.state()?;
        match self
            .handle
            .dispatch(BrowserCommand::FocusProjection {
                context_id: self.context_id,
                document_id: state.document_id,
                element_id: element_id.to_owned(),
            })
            .map_err(|error| error.message)?
        {
            BrowserCommandResult::FocusProjection(projection) => Ok(projection),
            result => Err(format!("unexpected focus-projection result: {result:?}")),
        }
    }

    pub(crate) fn form_submission(&mut self, form_id: &str) -> Result<FormSubmissionInfo, String> {
        let state = self.state()?;
        match self
            .handle
            .dispatch(BrowserCommand::FormSubmission {
                context_id: self.context_id,
                document_id: state.document_id,
                form_id: form_id.to_owned(),
            })
            .map_err(|error| error.message)?
        {
            BrowserCommandResult::FormSubmission(submission) => Ok(submission),
            result => Err(format!("unexpected form-submission result: {result:?}")),
        }
    }

    fn state(&mut self) -> Result<BrowsingContextState, String> {
        match self
            .handle
            .dispatch(BrowserCommand::GetBrowsingContextState {
                context_id: self.context_id,
            })
            .map_err(|error| error.to_string())?
        {
            BrowserCommandResult::BrowsingContextState(state) => Ok(state),
            result => Err(format!("unexpected context-state result: {result:?}")),
        }
    }

    fn navigate(&mut self, url: &str) -> Result<(), String> {
        let navigation_id = match self
            .handle
            .dispatch(BrowserCommand::Navigate {
                context_id: self.context_id,
                url: url.to_owned(),
            })
            .map_err(|error| error.to_string())?
        {
            BrowserCommandResult::NavigationAccepted { navigation_id } => navigation_id,
            result => return Err(format!("unexpected navigation result: {result:?}")),
        };

        self.wait_for_navigation(navigation_id)
    }

    // Completion stays keyed by NavigationId so a failed or superseded load
    // cannot silently use the previous document's realm.
    fn wait_for_navigation(&mut self, navigation_id: NavigationId) -> Result<(), String> {
        let deadline = Instant::now() + NAVIGATION_WAIT_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let Some(event) = self
                .handle
                .wait_next_event(remaining)
                .map_err(|error| error.to_string())?
            else {
                return Err(format!("timed out waiting for navigation {navigation_id}"));
            };
            match event {
                BrowserEvent::NavigationPhaseChanged {
                    context_id,
                    navigation_id: event_navigation_id,
                    phase: NavigationPhase::Settled,
                    ..
                } if context_id == self.context_id && event_navigation_id == navigation_id => {
                    return Ok(());
                }
                BrowserEvent::NavigationFailed {
                    context_id,
                    navigation_id: event_navigation_id,
                    error,
                    ..
                } if context_id == self.context_id && event_navigation_id == navigation_id => {
                    return Err(error.to_string());
                }
                BrowserEvent::NavigationCancelled {
                    context_id,
                    navigation_id: event_navigation_id,
                    reason,
                    ..
                } if context_id == self.context_id && event_navigation_id == navigation_id => {
                    return Err(format!(
                        "navigation {navigation_id} was cancelled: {reason:?}"
                    ));
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_scripts_execute_before_evaluation() {
        let fixture = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            fixture.path(),
            "<!doctype html><title>Core eval</title><script>globalThis.fromPage = 'ready';</script><main>Body text</main>",
        )
        .unwrap();
        let url = url::Url::from_file_path(fixture.path())
            .unwrap()
            .to_string();
        let mut session = BrowserSession::load(&url, None).unwrap();

        assert_eq!(
            session
                .evaluate("`${document.title}:${globalThis.fromPage}`")
                .unwrap(),
            ScriptValue::String("Core eval:ready".to_owned())
        );
        assert_eq!(session.current_url().unwrap(), url);
        assert_eq!(
            session
                .query_selector_all("title", (800, 600))
                .unwrap()
                .len(),
            1
        );
        assert!(
            session
                .document_text(DocumentTextKind::Dom, (800, 600))
                .unwrap()
                .contains("Core eval")
        );
        assert_eq!(
            session
                .document_text(DocumentTextKind::TextContent, (800, 600))
                .unwrap(),
            "Body text"
        );
    }

    #[test]
    fn navigation_failure_does_not_evaluate_initial_document() {
        let dir = tempfile::tempdir().unwrap();
        let url = url::Url::from_file_path(dir.path().join("missing.html"))
            .unwrap()
            .to_string();

        let error = match BrowserSession::load(&url, None) {
            Ok(_) => panic!("missing fixture unexpectedly loaded"),
            Err(error) => error,
        };

        assert!(
            error.contains("navigation.load"),
            "unexpected error: {error}"
        );
        assert!(
            error.contains("failed to read"),
            "unexpected error: {error}"
        );
    }

    fn storage_fixture() -> (tempfile::TempDir, String) {
        let fixture_dir = tempfile::tempdir().unwrap();
        let fixture = fixture_dir.path().join("storage.html");
        std::fs::write(&fixture, "<!doctype html><title>Storage</title>").unwrap();
        let url = url::Url::from_file_path(fixture).unwrap().to_string();
        (fixture_dir, url)
    }

    #[test]
    fn explicit_profile_creates_database() {
        let (_fixture_dir, url) = storage_fixture();
        let parent = tempfile::tempdir().unwrap();
        let profile_dir = parent.path().join("persistent-profile");

        drop(BrowserSession::load(&url, Some(&profile_dir)).unwrap());

        assert!(profile_dir.join("profile.redb").is_file());
    }

    #[test]
    fn explicit_profile_local_storage_survives_reopen() {
        let (_fixture_dir, url) = storage_fixture();
        let parent = tempfile::tempdir().unwrap();
        let profile_dir = parent.path().join("persistent-profile");
        {
            let mut first = BrowserSession::load(&url, Some(&profile_dir)).unwrap();
            assert_eq!(
                first
                    .evaluate("localStorage.setItem('profile-state', 'saved'); 'saved'")
                    .unwrap(),
                ScriptValue::String("saved".to_owned())
            );
        }

        let mut second = BrowserSession::load(&url, Some(&profile_dir)).unwrap();
        assert_eq!(
            second
                .evaluate("localStorage.getItem('profile-state')")
                .unwrap(),
            ScriptValue::String("saved".to_owned())
        );
    }

    #[test]
    fn default_profiles_isolate_local_storage() {
        let (_fixture_dir, url) = storage_fixture();
        let mut first = BrowserSession::load(&url, None).unwrap();
        assert_eq!(
            first
                .evaluate("localStorage.setItem('profile-state', 'first'); 'first'")
                .unwrap(),
            ScriptValue::String("first".to_owned())
        );

        let mut second = BrowserSession::load(&url, None).unwrap();
        assert_eq!(
            second
                .evaluate("localStorage.getItem('profile-state')")
                .unwrap(),
            ScriptValue::Null
        );
    }
}
