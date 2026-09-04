//! Headless DOM-side interaction actions.

use std::process::ExitCode;

use vixen_api::{ElementInfo, FormEntryInfo, FormEntryValueInfo};

use crate::{Cli, browser_adapter::BrowserSession};

/// `--focus` / `--submit-form`: deterministic DOM-side action
/// summaries over local pages. JS event listeners land with Phase 6 host
/// bindings; this path validates targets and exposes the event/submission data
/// the eventual hooks consume.
pub(crate) fn run(url: &str, cli: &Cli) -> ExitCode {
    let mut session = match BrowserSession::load(url, cli.profile_dir.as_deref()) {
        Ok(session) => session,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    if let Some(id) = cli.focus.as_deref() {
        let projection = match session.focus_projection(id) {
            Ok(projection) => projection,
            Err(error) => {
                eprintln!("error: {error}");
                return ExitCode::FAILURE;
            }
        };
        let events: Vec<_> = projection
            .events
            .into_iter()
            .map(|event| {
                serde_json::json!({
                    "event": event.event,
                    "target": event.target,
                    "bubbles": event.bubbles,
                })
            })
            .collect();
        print_json(serde_json::json!({
            "type": "focus",
            "id": id,
            "target": element_info_json(&projection.target),
            "events": events,
        }));
    }

    if let Some(id) = cli.submit_form.as_deref() {
        let submission = match session.form_submission(id) {
            Ok(submission) => submission,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        };
        let entries: Vec<_> = submission.entries.iter().map(form_entry_json).collect();
        print_json(serde_json::json!({
            "type": "submit-form",
            "id": id,
            "form": element_info_json(&submission.form),
            "action": submission.action,
            "method": submission.method,
            "enctype": submission.enctype,
            "content_type": submission.content_type,
            "entries": entries,
            "body_utf8": String::from_utf8_lossy(&submission.body),
            "body_bytes": submission.body.len(),
        }));
    }

    ExitCode::SUCCESS
}

fn print_json(value: serde_json::Value) {
    println!("{value}");
}

fn element_info_json(element: &ElementInfo) -> serde_json::Value {
    serde_json::json!({
        "node_id": element.node_id,
        "tag": element.tag,
        "id": element.id,
        "classes": element.classes,
        "attributes": element.attributes.iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect::<std::collections::BTreeMap<_, _>>(),
        "text": element.text,
        "bbox": element.bbox.map(|(x, y, w, h)| serde_json::json!({
            "x": x,
            "y": y,
            "w": w,
            "h": h,
        })),
    })
}

fn form_entry_json(entry: &FormEntryInfo) -> serde_json::Value {
    match &entry.value {
        FormEntryValueInfo::Text(value) => serde_json::json!({
            "name": entry.name,
            "kind": "text",
            "value": value,
        }),
        FormEntryValueInfo::File {
            filename,
            content_type,
            body,
        } => serde_json::json!({
            "name": entry.name,
            "kind": "file",
            "filename": filename,
            "content_type": content_type,
            "bytes": body.len(),
        }),
    }
}
