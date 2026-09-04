use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use vixen_api::{
    BrowserCommand, BrowserCommandResult, BrowsingContextId, BrowsingContextState, HostViewState,
    RenderBrokerRequestKind, RenderBrokerResponseKind, RenderCaptureRequest, RenderCommit,
    RenderNodeId, RenderRect,
};
use vixen_cdp::CdpRenderBackend;
use vixen_engine::browser::EngineBrowserClient;

use crate::c_abi::{ControllerEntry, SharedControllerEntry, drain_renderer_submissions};

const CAPTURE_TIMEOUT: Duration = Duration::from_secs(15);
const CAPTURE_POLL_INTERVAL: Duration = Duration::from_millis(5);

pub(crate) struct CdpHost {
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    join: Option<JoinHandle<()>>,
}

impl CdpHost {
    pub(crate) fn start(entry: &SharedControllerEntry, port: u16) -> Result<Self, String> {
        if port == 0 {
            return Err("CDP port must be nonzero".to_owned());
        }
        let browser = entry
            .state
            .lock()
            .map_err(|_| "browser handle is unavailable".to_owned())?
            .controller
            .subscribe_browser();
        let renderer: Arc<dyn CdpRenderBackend> = Arc::new(FlutterCdpRenderBackend {
            entry: Arc::downgrade(entry),
            viewports: Mutex::new(HashMap::new()),
        });
        let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel();
        let join = std::thread::Builder::new()
            .name("vixen-cdp-host".to_owned())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        eprintln!("Vixen automation CDP failed: {error}");
                        return;
                    }
                };
                let local = tokio::task::LocalSet::new();
                let result = local.block_on(
                    &runtime,
                    vixen_cdp::serve_with_browser_client_until(
                        port,
                        browser,
                        renderer,
                        async move {
                            let _ = shutdown_rx.await;
                        },
                    ),
                );
                if let Err(error) = result {
                    eprintln!("Vixen automation CDP failed: {error}");
                }
            })
            .map_err(|error| format!("failed to start CDP host thread: {error}"))?;
        Ok(Self {
            shutdown: Some(shutdown),
            join: Some(join),
        })
    }

    pub(crate) fn shutdown(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

struct FlutterCdpRenderBackend {
    entry: Weak<ControllerEntry>,
    viewports: Mutex<HashMap<BrowsingContextId, CdpViewportState>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CdpViewportState {
    viewport: (u32, u32),
    generation: u64,
}

impl FlutterCdpRenderBackend {
    fn present_commit(
        &self,
        browser: &mut EngineBrowserClient,
        context: &BrowsingContextState,
        viewport: (u32, u32),
    ) -> Result<(SharedControllerEntry, RenderCommit), String> {
        let entry = self
            .entry
            .upgrade()
            .ok_or_else(|| "Flutter CDP host is closed".to_owned())?;
        let viewport_generation = self.prepare_viewport(browser, context, viewport)?;
        let snapshot = match browser
            .dispatch(BrowserCommand::RenderSnapshot {
                context_id: context.context_id,
                document_id: context.document_id,
                viewport,
                viewport_generation,
                page_zoom: context.page_zoom,
            })
            .map_err(|error| error.to_string())?
        {
            BrowserCommandResult::RenderSnapshot(snapshot) => snapshot,
            result => return Err(format!("unexpected renderer snapshot result: {result:?}")),
        };

        let (accepted, current) = {
            let mut state = entry
                .renderer_state
                .lock()
                .map_err(|_| "renderer acceptance state is unavailable".to_owned())?;
            let accepted = drain_renderer_submissions(&entry.renderer, &mut state)
                .map_err(|error| format!("{}: {}", error.code, error.message))?;
            let current = state
                .commits
                .presented_commit()
                .filter(|commit| {
                    commit.revision == snapshot.revision && commit.viewport == snapshot.viewport
                })
                .cloned();
            if current.is_none() {
                let force_snapshot = state.needs_resync;
                crate::sync_renderer::publish_renderer_source(
                    &entry.renderer,
                    &mut state,
                    snapshot.clone(),
                    force_snapshot,
                )
                .map_err(|error| format!("{}: {}", error.code, error.message))?;
            }
            (accepted, current)
        };
        apply_renderer_commits(browser, accepted)?;
        if let Some(commit) = current {
            return Ok((entry.clone(), commit));
        }

        let deadline = Instant::now() + CAPTURE_TIMEOUT;
        let presented = loop {
            let (accepted, presented) = {
                let mut state = entry
                    .renderer_state
                    .lock()
                    .map_err(|_| "renderer acceptance state is unavailable".to_owned())?;
                let accepted = drain_renderer_submissions(&entry.renderer, &mut state)
                    .map_err(|error| format!("{}: {}", error.code, error.message))?;
                let presented = state
                    .commits
                    .presented_commit()
                    .filter(|commit| {
                        commit.revision == snapshot.revision && commit.viewport == snapshot.viewport
                    })
                    .cloned();
                (accepted, presented)
            };
            apply_renderer_commits(browser, accepted)?;
            if let Some(commit) = presented {
                break commit;
            }
            if Instant::now() >= deadline {
                return Err(
                    "timed out waiting for the exact Flutter commit presentation".to_owned(),
                );
            }
            std::thread::sleep(CAPTURE_POLL_INTERVAL);
        };
        Ok((entry, presented))
    }

    fn prepare_viewport(
        &self,
        browser: &mut EngineBrowserClient,
        context: &BrowsingContextState,
        viewport: (u32, u32),
    ) -> Result<u64, String> {
        let current = self
            .viewports
            .lock()
            .map_err(|_| "CDP viewport state is unavailable".to_owned())?
            .get(&context.context_id)
            .copied();
        let (next, changed) = next_viewport_state(current, viewport)?;
        if changed {
            match browser
                .dispatch(BrowserCommand::UpdateHostViewState {
                    context_id: context.context_id,
                    state: HostViewState {
                        generation: next.generation,
                        viewport: next.viewport,
                        ..HostViewState::default()
                    },
                })
                .map_err(|error| error.to_string())?
            {
                BrowserCommandResult::InputDispatched(_) => {}
                result => return Err(format!("unexpected host-view result: {result:?}")),
            }
            self.viewports
                .lock()
                .map_err(|_| "CDP viewport state is unavailable".to_owned())?
                .insert(context.context_id, next);
        }
        Ok(next.generation)
    }
}

fn apply_renderer_commits(
    browser: &mut EngineBrowserClient,
    commits: Vec<RenderCommit>,
) -> Result<(), String> {
    for commit in commits {
        match browser
            .dispatch(BrowserCommand::ApplyRendererCommit { commit })
            .map_err(|error| error.to_string())?
        {
            BrowserCommandResult::Accepted => {}
            result => return Err(format!("unexpected renderer commit result: {result:?}")),
        }
    }
    Ok(())
}

impl CdpRenderBackend for FlutterCdpRenderBackend {
    fn uses_commit_geometry(&self) -> bool {
        true
    }

    fn capture_png(
        &self,
        browser: &mut EngineBrowserClient,
        context: &BrowsingContextState,
        viewport: (u32, u32),
    ) -> Result<Vec<u8>, String> {
        let (entry, presented) = self.present_commit(browser, context, viewport)?;
        let response = entry
            .renderer
            .request(
                RenderBrokerRequestKind::CaptureScene(RenderCaptureRequest {
                    context_id: context.context_id,
                    document_id: context.document_id,
                    displayed_commit_id: presented.commit_id.get(),
                    revision: presented.revision,
                    viewport: presented.viewport,
                }),
                CAPTURE_TIMEOUT,
            )
            .map_err(|error| format!("{}: {}", error.code, error.message))?;
        match response.kind {
            RenderBrokerResponseKind::CapturePng(png) => Ok(png),
            RenderBrokerResponseKind::Cancelled(reason) => {
                Err(format!("Flutter capture was cancelled: {reason:?}"))
            }
            RenderBrokerResponseKind::Failed { code, message } => Err(format!("{code}: {message}")),
            kind => Err(format!("unexpected Flutter capture response: {kind:?}")),
        }
    }

    fn layout_box(
        &self,
        browser: &mut EngineBrowserClient,
        context: &BrowsingContextState,
        viewport: (u32, u32),
        node_id: usize,
    ) -> Result<Option<[f64; 4]>, String> {
        let (_, commit) = self.present_commit(browser, context, viewport)?;
        let raw_node_id = u64::try_from(node_id)
            .map_err(|_| "DOM node id exceeds the renderer id range".to_owned())?;
        let mut bounds: Option<RenderRect> = None;
        for geometry in commit
            .geometry_index
            .iter()
            .filter(|geometry| geometry.node_id.get() == raw_node_id)
        {
            bounds = Some(match bounds {
                None => geometry.border_box,
                Some(bounds) => union_rect(bounds, geometry.border_box),
            });
        }
        Ok(bounds.map(|rect| [rect.x, rect.y, rect.width, rect.height]))
    }

    fn hit_test(
        &self,
        browser: &mut EngineBrowserClient,
        context: &BrowsingContextState,
        viewport: (u32, u32),
        x: f64,
        y: f64,
    ) -> Result<Option<usize>, String> {
        let (entry, commit) = self.present_commit(browser, context, viewport)?;
        let hit = commit
            .geometry_index
            .iter()
            .filter(|geometry| {
                contains(geometry.border_box, x, y)
                    && geometry.clip.is_none_or(|clip| contains(clip, x, y))
            })
            .max_by_key(|geometry| geometry.paint_order)
            .map(|geometry| geometry.node_id);
        let Some(hit) = hit else {
            return Ok(None);
        };
        let state = entry
            .renderer_state
            .lock()
            .map_err(|_| "renderer acceptance state is unavailable".to_owned())?;
        let element = state
            .replica
            .nearest_element_node_id(hit)
            .or_else(|| state.replica.nearest_semantic_node_id(hit));
        element
            .map(RenderNodeId::get)
            .map(|node_id| {
                usize::try_from(node_id)
                    .map_err(|_| "renderer input node id exceeds usize".to_owned())
            })
            .transpose()
    }

    fn reset_renderer(&self, context: &BrowsingContextState) -> Result<(), String> {
        let entry = self
            .entry
            .upgrade()
            .ok_or_else(|| "Flutter CDP host is closed".to_owned())?;
        let response = entry
            .renderer
            .request(
                RenderBrokerRequestKind::Reset {
                    context_id: context.context_id,
                    document_id: context.document_id,
                },
                CAPTURE_TIMEOUT,
            )
            .map_err(|error| format!("{}: {}", error.code, error.message))?;
        match response.kind {
            RenderBrokerResponseKind::Reset => {
                let mut state = entry
                    .renderer_state
                    .lock()
                    .map_err(|_| "renderer acceptance state is unavailable".to_owned())?;
                state.replica = vixen_api::RenderReplica::default();
                state.commits = vixen_api::RenderCommitState::default();
                state.needs_resync = true;
                Ok(())
            }
            RenderBrokerResponseKind::Failed { code, message } => Err(format!("{code}: {message}")),
            kind => Err(format!("unexpected renderer reset response: {kind:?}")),
        }
    }
}

fn contains(rect: RenderRect, x: f64, y: f64) -> bool {
    x >= rect.x && y >= rect.y && x < rect.x + rect.width && y < rect.y + rect.height
}

fn union_rect(left: RenderRect, right: RenderRect) -> RenderRect {
    let x = left.x.min(right.x);
    let y = left.y.min(right.y);
    let right_edge = (left.x + left.width).max(right.x + right.width);
    let bottom = (left.y + left.height).max(right.y + right.height);
    RenderRect {
        x,
        y,
        width: right_edge - x,
        height: bottom - y,
    }
}

fn viewport_generation((width, height): (u32, u32)) -> u64 {
    (u64::from(width) << 32) | u64::from(height)
}

fn next_viewport_state(
    current: Option<CdpViewportState>,
    viewport: (u32, u32),
) -> Result<(CdpViewportState, bool), String> {
    match current {
        Some(current) if current.viewport == viewport => Ok((current, false)),
        Some(current) => Ok((
            CdpViewportState {
                viewport,
                generation: current
                    .generation
                    .checked_add(1)
                    .ok_or_else(|| "CDP viewport generation exhausted".to_owned())?,
            },
            true,
        )),
        None => Ok((
            CdpViewportState {
                viewport,
                generation: viewport_generation(viewport),
            },
            true,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cdp_viewport_generation_is_monotonic_when_the_viewport_shrinks() {
        let (initial, changed) = next_viewport_state(None, (800, 600)).unwrap();
        assert!(changed);

        let (smaller, changed) = next_viewport_state(Some(initial), (320, 240)).unwrap();
        assert!(changed);
        assert!(smaller.generation > initial.generation);

        let (unchanged, changed) = next_viewport_state(Some(smaller), (320, 240)).unwrap();
        assert!(!changed);
        assert_eq!(unchanged, smaller);
    }
}
