//! Headless DOM-side interaction actions.

use std::process::ExitCode;

use vixen_api::{ElementInfo, FormEntryInfo, FormEntryValueInfo};

use crate::{Cli, browser_adapter::BrowserSession, parse_viewport};

/// `--click-at` / `--focus` / `--submit-form`: deterministic DOM-side action
/// summaries over local pages. JS event listeners land with Phase 6 host
/// bindings; this path validates targets and exposes the event/submission data
/// the eventual hooks consume.
pub(crate) fn run(url: &str, cli: &Cli) -> ExitCode {
    let mut session = match BrowserSession::load(url) {
        Ok(session) => session,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let viewport = if cli.click_at.is_some() {
        match parse_viewport(&cli.viewport) {
            Ok(viewport) => Some(viewport),
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(2);
            }
        }
    } else {
        None
    };

    if let Some(raw) = cli.click_at.as_deref() {
        let (x, y) = match parse_click_coordinates(raw) {
            Ok(point) => point,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(2);
            }
        };
        let target = match session.hit_test(viewport.expect("viewport parsed"), x, y) {
            Ok(target) => target,
            Err(error) => {
                eprintln!("error: {error}");
                return ExitCode::FAILURE;
            }
        };
        print_json(serde_json::json!({
            "type": "click",
            "event": "MouseEvent",
            "x": x,
            "y": y,
            "target": target.as_ref().map(element_info_json),
        }));
    }

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

fn parse_click_coordinates(input: &str) -> Result<(f64, f64), String> {
    let Some((x, y)) = input.split_once(',') else {
        return Err("--click-at must be X,Y".to_owned());
    };
    let x: f64 = x
        .trim()
        .parse()
        .map_err(|_| "--click-at X must be a finite number".to_owned())?;
    let y: f64 = y
        .trim()
        .parse()
        .map_err(|_| "--click-at Y must be a finite number".to_owned())?;
    if !x.is_finite() || !y.is_finite() {
        return Err("--click-at coordinates must be finite".to_owned());
    }
    Ok((x, y))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_coordinates_parse_and_validate() {
        assert_eq!(parse_click_coordinates("10,20").unwrap(), (10.0, 20.0));
        assert_eq!(parse_click_coordinates(" 1.5 , -2 ").unwrap(), (1.5, -2.0));
        assert!(parse_click_coordinates("10").is_err());
        assert!(parse_click_coordinates("nan,1").is_err());
    }
}
