//! Thin typed adapter over the engine-owned browser core.

use std::time::{Duration, Instant};

use vixen_api::{
    BrowserCommand, BrowserCommandResult, BrowserEvent, BrowsingContextId, BrowsingContextState,
    DocumentTextKind, ElementInfo, FocusProjection, FormSubmissionInfo, NavigationPhase,
    ScriptValue,
};
use vixen_engine::browser::{BrowserConfig, EngineBrowserHandle, PaintSnapshot, spawn_browser};

const NAVIGATION_WAIT_TIMEOUT: Duration = Duration::from_secs(35);

/// One ephemeral-profile browser context for a headless CLI action.
///
/// The handle is declared before the temporary profile so the engine thread and
/// open store are dropped before the profile directory is removed.
pub(crate) struct BrowserSession {
    handle: EngineBrowserHandle,
    context_id: BrowsingContextId,
    _profile: tempfile::TempDir,
}

impl BrowserSession {
    pub(crate) fn load(url: &str) -> Result<Self, String> {
        let profile = tempfile::Builder::new()
            .prefix("vixen-headless-")
            .tempdir()
            .map_err(|error| format!("create temporary browser profile: {error}"))?;
        let mut handle = spawn_browser(BrowserConfig::new(profile.path().join("profile.redb")))
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
        let state = self.state()?;
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
            BrowserCommandResult::Evaluation(value) => Ok(value),
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

    pub(crate) fn capture_paint_snapshot(
        &mut self,
        viewport: (u32, u32),
    ) -> Result<PaintSnapshot, String> {
        let state = self.state()?;
        self.handle
            .capture_paint_snapshot(self.context_id, state.document_id, viewport)
            .map_err(|error| error.to_string())
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

    pub(crate) fn display_list_text(&mut self, viewport: (u32, u32)) -> Result<String, String> {
        let state = self.state()?;
        match self
            .handle
            .dispatch(BrowserCommand::DisplayListText {
                context_id: self.context_id,
                document_id: state.document_id,
                viewport,
            })
            .map_err(|error| error.to_string())?
        {
            BrowserCommandResult::DisplayListText(text) => Ok(text),
            result => Err(format!("unexpected display-list result: {result:?}")),
        }
    }

    pub(crate) fn hit_test(
        &mut self,
        viewport: (u32, u32),
        x: f64,
        y: f64,
    ) -> Result<Option<ElementInfo>, String> {
        let state = self.state()?;
        match self
            .handle
            .dispatch(BrowserCommand::HitTest {
                context_id: self.context_id,
                document_id: state.document_id,
                viewport,
                x,
                y,
            })
            .map_err(|error| error.to_string())?
        {
            BrowserCommandResult::HitTest(target) => Ok(target),
            result => Err(format!("unexpected hit-test result: {result:?}")),
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

        // Completion stays keyed by NavigationId so a failed or superseded load
        // cannot silently evaluate the previous document's realm.
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
        let mut session = BrowserSession::load(&url).unwrap();

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
        let paint = session.capture_paint_snapshot((800, 600)).unwrap();
        assert_eq!(paint.context_id, session.context_id);
        assert!(!paint.commands.is_empty());
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

        let error = match BrowserSession::load(&url) {
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
}
