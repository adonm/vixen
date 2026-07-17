//! `deno_core` runtime construction and V8 value conversion.

#![forbid(unsafe_code)]

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll, Waker};
use std::thread::JoinHandle;
use std::time::Duration;

use deno_core::futures::future::{Either, select};
use deno_core::v8;
use deno_core::{JsRuntime as DenoJsRuntime, PollEventLoopOptions, RuntimeOptions};
use vixen_api::RenderBrokerCancellation;
use vixen_net::NetworkConfig;

use crate::engine_error::{EngineError, codes};
use crate::page::Page;

use super::{JsValue, cssom, dom, encoding, webapi, webidl};

#[cfg(not(test))]
const SCRIPT_EXECUTION_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(test)]
const SCRIPT_EXECUTION_TIMEOUT: Duration = Duration::from_millis(250);

pub(super) struct DenoRuntimeInit {
    pub(super) runtime: DenoJsRuntime,
    pub(super) dom_mutations: Option<dom::DomMutationSink>,
}

pub(super) struct DenoRuntimeConfig {
    pub(super) network: NetworkConfig,
    pub(super) storage: webapi::WebStorageHost,
    pub(super) network_state: webapi::RuntimeNetworkState,
    pub(super) extra_http_headers: webapi::ExtraHttpHeaders,
    pub(super) cache_disabled: webapi::CacheDisabledFlag,
    pub(super) permission_overrides: webapi::PermissionOverrides,
    pub(super) interrupt: RuntimeInterruptHandle,
    pub(super) synchronous_layout: Option<super::SynchronousLayoutConfig>,
}

#[derive(Clone, Default)]
pub(crate) struct RuntimeInterruptHandle {
    active: Arc<Mutex<Option<ActiveExecution>>>,
    layout_cancellation: RenderLayoutCancellation,
}

#[derive(Clone, Default)]
pub struct RenderLayoutCancellation(Arc<Mutex<Option<RenderBrokerCancellation>>>);

impl RenderLayoutCancellation {
    pub fn reason(&self) -> Option<RenderBrokerCancellation> {
        *self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn cancel(&self, reason: RenderBrokerCancellation) {
        let mut current = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if current.is_none() {
            *current = Some(reason);
        }
    }

    fn clear(&self) {
        *self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }
}

struct ActiveExecution {
    state: Arc<DeadlineState>,
    isolate: v8::IsolateHandle,
}

impl RuntimeInterruptHandle {
    pub(crate) fn interrupt(&self) -> bool {
        let active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(active) = active.as_ref() else {
            return false;
        };
        if active.state.complete.load(Ordering::Acquire) {
            return false;
        }
        active.state.interrupted.store(true, Ordering::Release);
        let interrupted = active.isolate.terminate_execution();
        active.state.wake();
        interrupted
    }

    pub(crate) fn interrupt_layout(&self, reason: RenderBrokerCancellation) -> bool {
        self.layout_cancellation.cancel(reason);
        self.interrupt()
    }

    pub(crate) fn layout_cancellation(&self) -> RenderLayoutCancellation {
        self.layout_cancellation.clone()
    }

    pub(crate) fn is_terminated(&self) -> bool {
        self.active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .is_some_and(|active| {
                active.state.interrupted.load(Ordering::Acquire)
                    || active.state.timed_out.load(Ordering::Acquire)
            })
    }

    pub(crate) fn with_active_execution<T>(&self, operation: impl FnOnce() -> T) -> Option<T> {
        let active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let active = active.as_ref()?;
        if active.state.complete.load(Ordering::Acquire)
            || active.state.interrupted.load(Ordering::Acquire)
            || active.state.timed_out.load(Ordering::Acquire)
        {
            return None;
        }
        Some(operation())
    }

    fn install(&self, state: Arc<DeadlineState>, isolate: v8::IsolateHandle) {
        *self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            Some(ActiveExecution { state, isolate });
    }

    fn clear(&self, state: &Arc<DeadlineState>) {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if active
            .as_ref()
            .is_some_and(|active| Arc::ptr_eq(&active.state, state))
        {
            *active = None;
        }
    }
}

pub(super) fn new_deno_runtime(
    page: Option<&Page>,
    config: DenoRuntimeConfig,
) -> Result<DenoRuntimeInit, EngineError> {
    // A standalone initial about:blank target has no creator origin to inherit.
    // Keep its automation bootstrap fetch behavior equivalent to the no-page
    // realm while still projecting the document host objects.
    let fetch_policy = page
        .filter(|page| page.url() != "about:blank")
        .map(webapi::FetchPolicy::from_page);
    let layout_cancellation = config.interrupt.layout_cancellation();
    let mut extensions = vec![
        webidl::extension(),
        encoding::extension(),
        webapi::extension(webapi::WebApiConfig {
            network: config.network,
            storage: config.storage,
            network_state: config.network_state,
            fetch_policy,
            extra_http_headers: config.extra_http_headers,
            cache_disabled: config.cache_disabled,
            permission_overrides: config.permission_overrides,
            interrupt: config.interrupt,
        }),
    ];
    let mut dom_mutations = None;
    if let Some(page) = page {
        let mutations = dom::DomMutationSink::default();
        extensions.push(dom::extension(
            page,
            mutations.clone(),
            config
                .synchronous_layout
                .map(|config| dom::SynchronousLayoutHost {
                    config,
                    mutations: mutations.clone(),
                    cancellation: layout_cancellation,
                }),
        )?);
        extensions.push(cssom::extension(page)?);
        dom_mutations = Some(mutations);
    }

    let runtime = DenoJsRuntime::try_new(RuntimeOptions {
        extensions,
        ..Default::default()
    })
    .map_err(|err| EngineError::Other {
        code: codes::SCRIPT_OOM,
        message: format!("deno_core runtime initialisation failed: {err}"),
    })?;
    Ok(DenoRuntimeInit {
        runtime,
        dom_mutations,
    })
}

pub(super) fn execute_script(
    runtime: &mut DenoJsRuntime,
    name: &'static str,
    source: String,
    interrupt: &RuntimeInterruptHandle,
) -> Result<v8::Global<v8::Value>, EngineError> {
    let deadline = RuntimeDeadline::start(runtime, SCRIPT_EXECUTION_TIMEOUT, interrupt.clone());
    let global = match runtime.execute_script(name, source) {
        Ok(global) => global,
        Err(error) => {
            return Err(deadline.finish(runtime).map_or_else(
                || {
                    EngineError::script(
                        codes::SCRIPT_EVAL,
                        format!("script evaluation raised an exception: {error}"),
                    )
                },
                termination_error,
            ));
        }
    };

    let resolve = runtime.resolve(global);
    let outcome = {
        let event_loop =
            Box::pin(runtime.with_event_loop_promise(resolve, PollEventLoopOptions::default()));
        let timeout = Box::pin(DeadlineFuture {
            state: deadline.state.clone(),
        });
        match deno_core::futures::executor::block_on(select(event_loop, timeout)) {
            Either::Left((result, timeout)) => {
                drop(timeout);
                Some(result)
            }
            Either::Right(((), event_loop)) => {
                drop(event_loop);
                None
            }
        }
    };

    let Some(outcome) = outcome else {
        return Err(termination_error(
            deadline
                .finish(runtime)
                .unwrap_or(TerminationReason::Timeout),
        ));
    };
    let value = match outcome {
        Ok(value) => value,
        Err(error) => {
            return Err(deadline.finish(runtime).map_or_else(
                || {
                    EngineError::script(
                        codes::SCRIPT_EVAL,
                        format!("script evaluation raised an exception: {error}"),
                    )
                },
                termination_error,
            ));
        }
    };
    runtime.v8_isolate().perform_microtask_checkpoint();
    if let Some(reason) = deadline.finish(runtime) {
        return Err(termination_error(reason));
    }
    Ok(value)
}

pub(super) fn execute_module(
    runtime: &mut DenoJsRuntime,
    specifier: deno_core::ModuleSpecifier,
    source: String,
    interrupt: &RuntimeInterruptHandle,
) -> Result<(), EngineError> {
    let deadline = RuntimeDeadline::start(runtime, SCRIPT_EXECUTION_TIMEOUT, interrupt.clone());
    let timeout = Box::pin(DeadlineFuture {
        state: deadline.state.clone(),
    });
    let operation = Box::pin(async {
        let module_id = runtime
            .load_side_es_module_from_code(&specifier, source)
            .await
            .map_err(|error| error.to_string())?;
        let evaluation = runtime.mod_evaluate(module_id);
        runtime
            .run_event_loop(PollEventLoopOptions::default())
            .await
            .map_err(|error| error.to_string())?;
        evaluation.await.map_err(|error| error.to_string())
    });
    let outcome = match deno_core::futures::executor::block_on(select(operation, timeout)) {
        Either::Left((result, timeout)) => {
            drop(timeout);
            Some(result)
        }
        Either::Right(((), operation)) => {
            drop(operation);
            None
        }
    };

    let Some(outcome) = outcome else {
        return Err(termination_error(
            deadline
                .finish(runtime)
                .unwrap_or(TerminationReason::Timeout),
        ));
    };
    if let Err(error) = outcome {
        return Err(deadline.finish(runtime).map_or_else(
            || {
                EngineError::script(
                    codes::SCRIPT_EVAL,
                    format!("module evaluation raised an exception: {error}"),
                )
            },
            termination_error,
        ));
    }
    runtime.v8_isolate().perform_microtask_checkpoint();
    if let Some(reason) = deadline.finish(runtime) {
        return Err(termination_error(reason));
    }
    Ok(())
}

pub(super) fn execute_script_immediate(
    runtime: &mut DenoJsRuntime,
    name: &'static str,
    source: String,
    interrupt: &RuntimeInterruptHandle,
) -> Result<v8::Global<v8::Value>, EngineError> {
    let deadline = RuntimeDeadline::start(runtime, SCRIPT_EXECUTION_TIMEOUT, interrupt.clone());
    let value = match runtime.execute_script(name, source) {
        Ok(value) => value,
        Err(error) => {
            return Err(deadline.finish(runtime).map_or_else(
                || {
                    EngineError::script(
                        codes::SCRIPT_EVAL,
                        format!("script evaluation raised an exception: {error}"),
                    )
                },
                termination_error,
            ));
        }
    };
    if let Some(reason) = deadline.finish(runtime) {
        return Err(termination_error(reason));
    }
    Ok(value)
}

#[derive(Clone, Copy)]
enum TerminationReason {
    Timeout,
    Interrupted,
}

fn termination_error(reason: TerminationReason) -> EngineError {
    match reason {
        TerminationReason::Timeout => EngineError::script(
            codes::SCRIPT_TIMEOUT,
            "script execution exceeded the bounded runtime deadline",
        ),
        TerminationReason::Interrupted => EngineError::script(
            codes::SCRIPT_INTERRUPTED,
            "script execution was interrupted by a browser lifecycle command",
        ),
    }
}

struct RuntimeDeadline {
    state: Arc<DeadlineState>,
    watchdog: JoinHandle<()>,
    interrupt: RuntimeInterruptHandle,
}

struct DeadlineState {
    complete: AtomicBool,
    timed_out: AtomicBool,
    interrupted: AtomicBool,
    wait_lock: Mutex<()>,
    wait: Condvar,
    waker: Mutex<Option<Waker>>,
}

impl DeadlineState {
    fn wake(&self) {
        if let Some(waker) = self
            .waker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            waker.wake();
        }
    }
}

impl RuntimeDeadline {
    fn start(
        runtime: &mut DenoJsRuntime,
        timeout: Duration,
        interrupt: RuntimeInterruptHandle,
    ) -> Self {
        interrupt.layout_cancellation.clear();
        let state = Arc::new(DeadlineState {
            complete: AtomicBool::new(false),
            timed_out: AtomicBool::new(false),
            interrupted: AtomicBool::new(false),
            wait_lock: Mutex::new(()),
            wait: Condvar::new(),
            waker: Mutex::new(None),
        });
        let watchdog_state = state.clone();
        let watchdog_layout_cancellation = interrupt.layout_cancellation();
        let isolate = runtime.v8_isolate().thread_safe_handle();
        interrupt.install(state.clone(), isolate.clone());
        let watchdog = std::thread::spawn(move || {
            let guard = watchdog_state
                .wait_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let (_guard, result) = watchdog_state
                .wait
                .wait_timeout_while(guard, timeout, |_| {
                    !watchdog_state.complete.load(Ordering::Acquire)
                })
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if result.timed_out() && !watchdog_state.complete.load(Ordering::Acquire) {
                watchdog_state.timed_out.store(true, Ordering::Release);
                watchdog_layout_cancellation.cancel(RenderBrokerCancellation::Deadline);
                let _ = isolate.terminate_execution();
                watchdog_state.wake();
            }
        });
        Self {
            state,
            watchdog,
            interrupt,
        }
    }

    fn finish(self, runtime: &mut DenoJsRuntime) -> Option<TerminationReason> {
        let guard = self
            .state
            .wait_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.state.complete.store(true, Ordering::Release);
        self.state.wait.notify_all();
        drop(guard);
        self.interrupt.clear(&self.state);
        let _ = self.watchdog.join();
        let timed_out = self.state.timed_out.load(Ordering::Acquire);
        let interrupted = self.state.interrupted.load(Ordering::Acquire);
        if timed_out || interrupted {
            let _ = runtime.v8_isolate().cancel_terminate_execution();
        }
        if interrupted {
            Some(TerminationReason::Interrupted)
        } else if timed_out {
            Some(TerminationReason::Timeout)
        } else {
            None
        }
    }
}

struct DeadlineFuture {
    state: Arc<DeadlineState>,
}

impl Future for DeadlineFuture {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut waker = self
            .state
            .waker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.state.timed_out.load(Ordering::Acquire)
            || self.state.interrupted.load(Ordering::Acquire)
        {
            Poll::Ready(())
        } else {
            *waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

pub(super) fn js_value_from_global(
    runtime: &mut DenoJsRuntime,
    global: v8::Global<v8::Value>,
) -> Result<JsValue, EngineError> {
    deno_core::scope!(scope, runtime);
    let value = v8::Local::new(scope, &global);

    let converted = if value.is_undefined() {
        Ok(JsValue::Undefined)
    } else if value.is_null() {
        Ok(JsValue::Null)
    } else if value.is_boolean() {
        Ok(JsValue::Bool(value.boolean_value(scope)))
    } else if value.is_int32() {
        Ok(JsValue::Int32(value.int32_value(scope).unwrap_or_default()))
    } else if value.is_number() {
        Ok(JsValue::Number(
            value.number_value(scope).unwrap_or(f64::NAN),
        ))
    } else if value.is_string() {
        let value = value.to_string(scope).ok_or_else(|| {
            EngineError::script(codes::SCRIPT_EVAL, "failed to convert JS string")
        })?;
        Ok(JsValue::String(value.to_rust_string_lossy(scope)))
    } else {
        Ok(JsValue::Object)
    };
    drop(global);
    converted
}
