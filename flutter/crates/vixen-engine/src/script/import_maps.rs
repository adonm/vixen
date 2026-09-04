//! Bounded import-map parsing and resolution for parser-discovered page modules.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use deno_core::serde_json::Value;
use url::Url;

const MAX_IMPORT_MAP_SOURCE_BYTES: usize = 256 * 1024;
const MAX_IMPORT_MAP_ENTRIES: usize = 2_048;
const MAX_IMPORT_MAP_INTEGRITY_ENTRIES: usize = 2_048;
const MAX_IMPORT_MAP_SCOPES: usize = 128;
const MAX_IMPORT_MAP_STRING_BYTES: usize = 16 * 1024;
const MAX_IMPORT_MAP_DIAGNOSTICS: usize = 32;
const MAX_IMPORT_MAP_DIAGNOSTIC_BYTES: usize = 1_024;
const MAX_IMPORT_MAP_STATE_BYTES: usize = 512 * 1024;
const MAX_RESOLVED_MODULE_SPECIFIERS: usize = 2_048;
const MAX_RESOLVED_MODULE_BYTES: usize = 1024 * 1024;
pub(super) const MAX_DOCUMENT_IMPORT_MAPS: usize = 64;
pub(super) const MAX_MODULE_SPECIFIER_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ResolvedModuleKey {
    base_url: String,
    specifier: String,
}

#[derive(Clone, Debug)]
struct ResolvedModuleSpecifier {
    key: ResolvedModuleKey,
    prefix_match_allowed: bool,
    result: String,
}

#[derive(Default)]
struct ResolvedModuleSet {
    entries: BTreeMap<ResolvedModuleKey, ResolvedModuleSpecifier>,
    bytes: usize,
}

#[derive(Clone)]
pub(crate) struct PageImportMap {
    resolver: Arc<import_map::ImportMap>,
    integrity: Arc<BTreeMap<String, String>>,
    resolved: Arc<Mutex<ResolvedModuleSet>>,
}

pub(super) struct ParsedPageImportMap {
    pub(super) map: PageImportMap,
    pub(super) diagnostics: Vec<String>,
}

pub(super) fn parse_inline_import_map(
    source: &str,
    base_url: Url,
) -> Result<ParsedPageImportMap, String> {
    if source.len() > MAX_IMPORT_MAP_SOURCE_BYTES {
        return Err(format!(
            "import map exceeds {MAX_IMPORT_MAP_SOURCE_BYTES} source bytes"
        ));
    }
    if base_url.as_str().len() > MAX_MODULE_SPECIFIER_BYTES {
        return Err("import map base URL exceeds the module specifier limit".to_owned());
    }
    let mut value = deno_core::serde_json::from_str::<Value>(source)
        .map_err(|error| format!("import map JSON is invalid: {error}"))?;
    let integrity = take_integrity_map(&mut value, &base_url)?;
    validate_import_map_shape(&value)?;
    let parsed = import_map::parse_from_value(base_url, value)
        .map_err(|error| format!("import map is invalid: {error}"))?;
    validate_import_map_state_size(&parsed.import_map, &integrity)?;
    let diagnostic_count = parsed.diagnostics.len();
    let mut diagnostics = parsed
        .diagnostics
        .into_iter()
        .take(MAX_IMPORT_MAP_DIAGNOSTICS)
        .map(|diagnostic| truncate_utf8(&diagnostic.to_string(), MAX_IMPORT_MAP_DIAGNOSTIC_BYTES))
        .collect::<Vec<_>>();
    if diagnostic_count > diagnostics.len() {
        diagnostics.push(format!(
            "{} additional import map diagnostics omitted",
            diagnostic_count - diagnostics.len()
        ));
    }
    Ok(ParsedPageImportMap {
        map: PageImportMap {
            resolver: Arc::new(parsed.import_map),
            integrity: Arc::new(integrity),
            resolved: Arc::new(Mutex::new(ResolvedModuleSet::default())),
        },
        diagnostics,
    })
}

pub(super) fn merge_inline_import_map(
    existing: &PageImportMap,
    source: &str,
    base_url: Url,
) -> Result<ParsedPageImportMap, String> {
    let parsed = parse_inline_import_map(source, base_url)?;
    let resolved = existing
        .resolved
        .lock()
        .map_err(|_| "import map resolved-module set is poisoned".to_owned())?
        .entries
        .clone();
    let mut merged_value = deno_core::serde_json::to_value(existing.resolver.as_ref())
        .map_err(|error| format!("existing import map serialization failed: {error}"))?;
    let mut new_value = deno_core::serde_json::to_value(parsed.map.resolver.as_ref())
        .map_err(|error| format!("new import map serialization failed: {error}"))?;
    let new_object = new_value
        .as_object_mut()
        .ok_or_else(|| "new import map serialization was not an object".to_owned())?;
    let new_imports = take_object_field(new_object, "imports")?;
    let new_scopes = take_object_field(new_object, "scopes")?;
    let merged_object = merged_value
        .as_object_mut()
        .ok_or_else(|| "existing import map serialization was not an object".to_owned())?;
    let mut diagnostics = parsed.diagnostics;
    let mut omitted_diagnostics = 0usize;

    merge_scopes(
        merged_object,
        new_scopes,
        &resolved,
        &mut diagnostics,
        &mut omitted_diagnostics,
    )?;
    merge_imports(
        merged_object,
        new_imports,
        &resolved,
        &mut diagnostics,
        &mut omitted_diagnostics,
    )?;
    validate_import_map_shape(&merged_value)?;
    let merged = import_map::parse_from_value(existing.resolver.base_url().clone(), merged_value)
        .map_err(|error| format!("merged import map is invalid: {error}"))?;

    let mut integrity = existing.integrity.as_ref().clone();
    for (url, metadata) in parsed.map.integrity.iter() {
        if integrity.contains_key(url) {
            push_merge_diagnostic(
                &mut diagnostics,
                &mut omitted_diagnostics,
                format!("later import map integrity rule for {url:?} was ignored"),
            );
        } else {
            integrity.insert(url.clone(), metadata.clone());
        }
    }
    if integrity.len() > MAX_IMPORT_MAP_INTEGRITY_ENTRIES {
        return Err(format!(
            "merged import map exceeds {MAX_IMPORT_MAP_INTEGRITY_ENTRIES} integrity entries"
        ));
    }
    validate_import_map_state_size(&merged.import_map, &integrity)?;
    if omitted_diagnostics > 0 && diagnostics.len() <= MAX_IMPORT_MAP_DIAGNOSTICS {
        diagnostics.push(format!(
            "{omitted_diagnostics} additional import map merge diagnostics omitted"
        ));
    }

    Ok(ParsedPageImportMap {
        map: PageImportMap {
            resolver: Arc::new(merged.import_map),
            integrity: Arc::new(integrity),
            resolved: Arc::clone(&existing.resolved),
        },
        diagnostics,
    })
}

impl PageImportMap {
    pub(super) fn resolve(&self, specifier: &str, referrer: &Url) -> Result<Url, String> {
        if specifier.len() > MAX_MODULE_SPECIFIER_BYTES
            || referrer.as_str().len() > MAX_MODULE_SPECIFIER_BYTES
        {
            return Err("module specifier or referrer exceeds the import map limit".to_owned());
        }
        let as_url = deno_core::resolve_import(specifier, referrer.as_str()).ok();
        let key = ResolvedModuleKey {
            base_url: referrer.to_string(),
            specifier: as_url
                .as_ref()
                .map_or_else(|| specifier.to_owned(), ToString::to_string),
        };
        if let Some(previous) = self
            .resolved
            .lock()
            .map_err(|_| "import map resolved-module set is poisoned".to_owned())?
            .entries
            .get(&key)
        {
            return Url::parse(&previous.result)
                .map_err(|_| "import map retained an invalid resolved module URL".to_owned());
        }
        let resolved = self
            .resolver
            .resolve(specifier, referrer)
            .map_err(|error| error.to_string())?;
        if resolved.as_str().len() > MAX_MODULE_SPECIFIER_BYTES {
            return Err("resolved module URL exceeds the import map limit".to_owned());
        }
        let record = ResolvedModuleSpecifier {
            key: key.clone(),
            prefix_match_allowed: as_url.as_ref().is_none_or(is_special_url),
            result: resolved.to_string(),
        };
        let mut records = self
            .resolved
            .lock()
            .map_err(|_| "import map resolved-module set is poisoned".to_owned())?;
        if let Some(previous) = records.entries.get(&key) {
            return Url::parse(&previous.result)
                .map_err(|_| "import map retained an invalid resolved module URL".to_owned());
        }
        if records.entries.len() >= MAX_RESOLVED_MODULE_SPECIFIERS {
            return Err(format!(
                "import map exceeds {MAX_RESOLVED_MODULE_SPECIFIERS} resolved module specifiers"
            ));
        }
        let record_bytes = record
            .key
            .base_url
            .len()
            .saturating_add(record.key.specifier.len())
            .saturating_add(record.result.len());
        if records.bytes.saturating_add(record_bytes) > MAX_RESOLVED_MODULE_BYTES {
            return Err(format!(
                "import map resolved module specifiers exceed {MAX_RESOLVED_MODULE_BYTES} bytes"
            ));
        }
        records.bytes += record_bytes;
        records.entries.insert(key, record);
        Ok(resolved)
    }

    pub(super) fn integrity_for(&self, url: &Url) -> Option<&str> {
        self.integrity.get(url.as_str()).map(String::as_str)
    }
}

fn validate_import_map_state_size(
    resolver: &import_map::ImportMap,
    integrity: &BTreeMap<String, String>,
) -> Result<(), String> {
    let resolver_bytes = deno_core::serde_json::to_vec(resolver)
        .map_err(|error| format!("import map serialization failed: {error}"))?
        .len();
    let integrity_bytes = integrity.iter().fold(0usize, |total, (url, metadata)| {
        total
            .saturating_add(url.len())
            .saturating_add(metadata.len())
    });
    if resolver_bytes.saturating_add(integrity_bytes) > MAX_IMPORT_MAP_STATE_BYTES {
        return Err(format!(
            "import map exceeds {MAX_IMPORT_MAP_STATE_BYTES} normalized bytes"
        ));
    }
    Ok(())
}

fn take_object_field(
    object: &mut deno_core::serde_json::Map<String, Value>,
    field: &str,
) -> Result<deno_core::serde_json::Map<String, Value>, String> {
    object
        .remove(field)
        .unwrap_or_else(|| Value::Object(Default::default()))
        .as_object()
        .cloned()
        .ok_or_else(|| format!("serialized import map {field} was not an object"))
}

fn merge_imports(
    merged: &mut deno_core::serde_json::Map<String, Value>,
    new_imports: deno_core::serde_json::Map<String, Value>,
    resolved: &BTreeMap<ResolvedModuleKey, ResolvedModuleSpecifier>,
    diagnostics: &mut Vec<String>,
    omitted_diagnostics: &mut usize,
) -> Result<(), String> {
    let old_imports = object_field_mut(merged, "imports")?;
    for (specifier, address) in new_imports {
        if resolved
            .values()
            .any(|record| specifier.starts_with(&record.key.specifier))
        {
            push_merge_diagnostic(
                diagnostics,
                omitted_diagnostics,
                format!(
                    "later import map rule for {specifier:?} was ignored after prior resolution"
                ),
            );
        } else if old_imports.contains_key(&specifier) {
            push_merge_diagnostic(
                diagnostics,
                omitted_diagnostics,
                format!("later import map rule for {specifier:?} was ignored"),
            );
        } else {
            old_imports.insert(specifier, address);
        }
    }
    Ok(())
}

fn merge_scopes(
    merged: &mut deno_core::serde_json::Map<String, Value>,
    new_scopes: deno_core::serde_json::Map<String, Value>,
    resolved: &BTreeMap<ResolvedModuleKey, ResolvedModuleSpecifier>,
    diagnostics: &mut Vec<String>,
    omitted_diagnostics: &mut usize,
) -> Result<(), String> {
    let old_scopes = object_field_mut(merged, "scopes")?;
    for (scope, value) in new_scopes {
        let new_imports = value
            .as_object()
            .ok_or_else(|| format!("serialized import map scope {scope:?} was not an object"))?;
        let old_imports = if let Some(existing) = old_scopes.get_mut(&scope) {
            existing
                .as_object_mut()
                .ok_or_else(|| format!("merged import map scope {scope:?} was not an object"))?
        } else {
            old_scopes.insert(scope.clone(), Value::Object(Default::default()));
            old_scopes
                .get_mut(&scope)
                .and_then(Value::as_object_mut)
                .ok_or_else(|| format!("merged import map scope {scope:?} was not an object"))?
        };
        for (specifier, address) in new_imports {
            let impacts_resolution = resolved.values().any(|record| {
                scope_matches(&scope, &record.key.base_url)
                    && (specifier == &record.key.specifier
                        || specifier.ends_with('/')
                            && record.key.specifier.starts_with(specifier)
                            && record.prefix_match_allowed)
            });
            if impacts_resolution {
                push_merge_diagnostic(
                    diagnostics,
                    omitted_diagnostics,
                    format!(
                        "later scoped import map rule for {specifier:?} in {scope:?} was ignored after prior resolution"
                    ),
                );
            } else if old_imports.contains_key(specifier) {
                push_merge_diagnostic(
                    diagnostics,
                    omitted_diagnostics,
                    format!(
                        "later scoped import map rule for {specifier:?} in {scope:?} was ignored"
                    ),
                );
            } else {
                old_imports.insert(specifier.clone(), address.clone());
            }
        }
    }
    Ok(())
}

fn object_field_mut<'a>(
    object: &'a mut deno_core::serde_json::Map<String, Value>,
    field: &str,
) -> Result<&'a mut deno_core::serde_json::Map<String, Value>, String> {
    if !object.contains_key(field) {
        object.insert(field.to_owned(), Value::Object(Default::default()));
    }
    object
        .get_mut(field)
        .and_then(Value::as_object_mut)
        .ok_or_else(|| format!("serialized import map {field} was not an object"))
}

fn scope_matches(scope: &str, base_url: &str) -> bool {
    scope == base_url || scope.ends_with('/') && base_url.starts_with(scope)
}

fn is_special_url(url: &Url) -> bool {
    matches!(
        url.scheme(),
        "ftp" | "file" | "http" | "https" | "ws" | "wss"
    )
}

fn push_merge_diagnostic(diagnostics: &mut Vec<String>, omitted: &mut usize, message: String) {
    if diagnostics.len() < MAX_IMPORT_MAP_DIAGNOSTICS {
        diagnostics.push(truncate_utf8(&message, MAX_IMPORT_MAP_DIAGNOSTIC_BYTES));
    } else {
        *omitted = omitted.saturating_add(1);
    }
}

fn take_integrity_map(
    value: &mut Value,
    base_url: &Url,
) -> Result<BTreeMap<String, String>, String> {
    let object = value
        .as_object_mut()
        .ok_or_else(|| "import map JSON must be an object".to_owned())?;
    let Some(integrity) = object.remove("integrity") else {
        return Ok(BTreeMap::new());
    };
    let integrity = integrity
        .as_object()
        .ok_or_else(|| "import map integrity must be an object".to_owned())?;
    if integrity.len() > MAX_IMPORT_MAP_INTEGRITY_ENTRIES {
        return Err(format!(
            "import map exceeds {MAX_IMPORT_MAP_INTEGRITY_ENTRIES} integrity entries"
        ));
    }
    let mut normalized = BTreeMap::new();
    for (key, metadata) in integrity {
        validate_string(key, "integrity URL")?;
        let metadata = metadata
            .as_str()
            .ok_or_else(|| format!("import map integrity metadata for {key:?} must be a string"))?;
        validate_string(metadata, "integrity metadata")?;
        let url = normalize_integrity_url(key, base_url)?;
        if normalized
            .insert(url.clone(), metadata.to_owned())
            .is_some()
        {
            return Err(format!(
                "import map integrity has duplicate normalized URL {url:?}"
            ));
        }
    }
    Ok(normalized)
}

fn normalize_integrity_url(value: &str, base_url: &Url) -> Result<String, String> {
    let url = Url::parse(value).or_else(|absolute_error| {
        if value.starts_with('/') || value.starts_with("./") || value.starts_with("../") {
            base_url.join(value)
        } else {
            Err(absolute_error)
        }
    });
    let url = url.map_err(|_| {
        format!("import map integrity key {value:?} must be an absolute or URL-like relative URL")
    })?;
    if url.as_str().len() > MAX_MODULE_SPECIFIER_BYTES {
        return Err("import map integrity URL exceeds the module specifier limit".to_owned());
    }
    Ok(url.to_string())
}

fn validate_import_map_shape(value: &Value) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| "import map JSON must be an object".to_owned())?;
    let mut entries = 0usize;
    if let Some(imports) = object.get("imports") {
        let imports = imports
            .as_object()
            .ok_or_else(|| "import map imports must be an object".to_owned())?;
        entries = entries.saturating_add(imports.len());
        validate_specifier_map(imports)?;
    }
    if let Some(scopes) = object.get("scopes") {
        let scopes = scopes
            .as_object()
            .ok_or_else(|| "import map scopes must be an object".to_owned())?;
        if scopes.len() > MAX_IMPORT_MAP_SCOPES {
            return Err(format!("import map exceeds {MAX_IMPORT_MAP_SCOPES} scopes"));
        }
        for (scope, imports) in scopes {
            validate_string(scope, "scope")?;
            let imports = imports
                .as_object()
                .ok_or_else(|| format!("import map scope {scope:?} must be an object"))?;
            entries = entries.saturating_add(imports.len());
            validate_specifier_map(imports)?;
        }
    }
    if entries > MAX_IMPORT_MAP_ENTRIES {
        return Err(format!(
            "import map exceeds {MAX_IMPORT_MAP_ENTRIES} mappings"
        ));
    }
    Ok(())
}

fn validate_specifier_map(map: &deno_core::serde_json::Map<String, Value>) -> Result<(), String> {
    for (key, value) in map {
        validate_string(key, "specifier")?;
        if let Some(value) = value.as_str() {
            validate_string(value, "address")?;
        }
    }
    Ok(())
}

fn validate_string(value: &str, kind: &str) -> Result<(), String> {
    if value.len() > MAX_IMPORT_MAP_STRING_BYTES {
        Err(format!(
            "import map {kind} exceeds {MAX_IMPORT_MAP_STRING_BYTES} bytes"
        ))
    } else {
        Ok(())
    }
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes.saturating_sub('…'.len_utf8());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_exact_prefix_url_like_and_scoped_mappings() {
        let parsed = parse_inline_import_map(
            r#"{
                "imports": {
                    "answer": "./answer.js",
                    "pkg/": "./vendor/pkg/",
                    "./remapped.js": "./replacement.js"
                },
                "scopes": {
                    "./feature/": { "answer": "./feature-answer.js" }
                },
                "integrity": {
                    "./answer.js": "sha384-answer",
                    "https://cdn.example.test/pkg.js": "sha512-package"
                }
            }"#,
            Url::parse("https://example.test/app/map.json").unwrap(),
        )
        .unwrap();
        assert!(parsed.diagnostics.is_empty());
        let root = Url::parse("https://example.test/app/root.js").unwrap();
        let scoped = Url::parse("https://example.test/app/feature/root.js").unwrap();
        assert_eq!(
            parsed.map.resolve("answer", &root).unwrap().as_str(),
            "https://example.test/app/answer.js"
        );
        assert_eq!(
            parsed.map.resolve("pkg/value.js", &root).unwrap().as_str(),
            "https://example.test/app/vendor/pkg/value.js"
        );
        assert_eq!(
            parsed.map.resolve("./remapped.js", &root).unwrap().as_str(),
            "https://example.test/app/replacement.js"
        );
        assert_eq!(
            parsed.map.resolve("answer", &scoped).unwrap().as_str(),
            "https://example.test/app/feature-answer.js"
        );
        assert_eq!(
            parsed
                .map
                .integrity_for(&Url::parse("https://example.test/app/answer.js").unwrap()),
            Some("sha384-answer")
        );
        assert_eq!(
            parsed
                .map
                .integrity_for(&Url::parse("https://cdn.example.test/pkg.js").unwrap()),
            Some("sha512-package")
        );
    }

    #[test]
    fn rejects_malformed_integrity_and_bounded_shapes() {
        assert!(
            parse_inline_import_map(
                r#"{"imports": {}, "integrity": []}"#,
                Url::parse("https://example.test/map.json").unwrap(),
            )
            .is_err()
        );
        assert!(
            parse_inline_import_map(
                r#"{"integrity": {"bare": "sha384-value"}}"#,
                Url::parse("https://example.test/map.json").unwrap(),
            )
            .is_err()
        );
        assert!(
            parse_inline_import_map(
                r#"{"integrity": {"./module.js": 42}}"#,
                Url::parse("https://example.test/map.json").unwrap(),
            )
            .is_err()
        );
        assert!(
            parse_inline_import_map(
                r#"{"integrity": {"./module.js": "sha384-one", "https://example.test/module.js": "sha384-two"}}"#,
                Url::parse("https://example.test/map.json").unwrap(),
            )
            .is_err()
        );
        let oversized = "x".repeat(MAX_IMPORT_MAP_STRING_BYTES + 1);
        let source = deno_core::serde_json::json!({ "imports": { oversized.clone(): "./x.js" } });
        assert!(
            parse_inline_import_map(
                &source.to_string(),
                Url::parse("https://example.test/map.json").unwrap(),
            )
            .is_err()
        );
        let source = deno_core::serde_json::json!({
            "integrity": { "./module.js": oversized }
        });
        assert!(
            parse_inline_import_map(
                &source.to_string(),
                Url::parse("https://example.test/map.json").unwrap(),
            )
            .is_err()
        );
    }

    #[test]
    fn merges_maps_first_rule_wins_and_integrity_accumulates() {
        let base = Url::parse("https://example.test/app/page.html").unwrap();
        let first = parse_inline_import_map(
            r#"{
                "imports": {"answer": "./first.js"},
                "scopes": {"./feature/": {"scoped": "./first-scoped.js"}},
                "integrity": {"./first.js": "sha384-first"}
            }"#,
            base.clone(),
        )
        .unwrap();
        let merged = merge_inline_import_map(
            &first.map,
            r#"{
                "imports": {"answer": "./ignored.js", "extra": "./extra.js"},
                "scopes": {"./feature/": {"scoped": "./ignored-scoped.js", "new": "./new.js"}},
                "integrity": {"./first.js": "sha384-ignored", "./extra.js": "sha384-extra"}
            }"#,
            base,
        )
        .unwrap();
        let root = Url::parse("https://example.test/app/root.js").unwrap();
        let scoped = Url::parse("https://example.test/app/feature/root.js").unwrap();

        assert_eq!(
            merged.map.resolve("answer", &root).unwrap().as_str(),
            "https://example.test/app/first.js"
        );
        assert_eq!(
            merged.map.resolve("extra", &root).unwrap().as_str(),
            "https://example.test/app/extra.js"
        );
        assert_eq!(
            merged.map.resolve("scoped", &scoped).unwrap().as_str(),
            "https://example.test/app/first-scoped.js"
        );
        assert_eq!(
            merged.map.resolve("new", &scoped).unwrap().as_str(),
            "https://example.test/app/new.js"
        );
        assert_eq!(
            merged
                .map
                .integrity_for(&Url::parse("https://example.test/app/first.js").unwrap()),
            Some("sha384-first")
        );
        assert_eq!(
            merged
                .map
                .integrity_for(&Url::parse("https://example.test/app/extra.js").unwrap()),
            Some("sha384-extra")
        );
        assert_eq!(merged.diagnostics.len(), 3);
    }

    #[test]
    fn resolved_module_set_blocks_later_global_and_scoped_rules() {
        let base = Url::parse("https://example.test/app/page.html").unwrap();
        let first =
            parse_inline_import_map(r#"{"imports":{"locked":"./old.js"}}"#, base.clone()).unwrap();
        let root = Url::parse("https://example.test/app/root.js").unwrap();
        assert_eq!(
            first.map.resolve("locked", &root).unwrap().as_str(),
            "https://example.test/app/old.js"
        );

        let merged = merge_inline_import_map(
            &first.map,
            r#"{
                "imports": {"locked/child": "./must-not-apply.js", "fresh": "./fresh.js"},
                "scopes": {"./": {"locked": "./must-not-apply-scoped.js"}}
            }"#,
            base,
        )
        .unwrap();
        assert!(merged.map.resolve("locked/child", &root).is_err());
        assert_eq!(
            merged.map.resolve("locked", &root).unwrap().as_str(),
            "https://example.test/app/old.js"
        );
        assert_eq!(
            merged.map.resolve("fresh", &root).unwrap().as_str(),
            "https://example.test/app/fresh.js"
        );
        assert_eq!(
            merged
                .diagnostics
                .iter()
                .filter(|message| message.contains("prior resolution"))
                .count(),
            2
        );
    }

    #[test]
    fn parser_position_snapshots_reuse_late_successful_resolution() {
        let base = Url::parse("https://example.test/app/page.html").unwrap();
        let first = parse_inline_import_map("{}", base.clone()).unwrap();
        let merged = merge_inline_import_map(
            &first.map,
            r#"{"imports":{"./late.js":"./remapped.js"}}"#,
            base,
        )
        .unwrap();
        let root = Url::parse("https://example.test/app/root.js").unwrap();

        assert_eq!(
            first.map.resolve("./late.js", &root).unwrap().as_str(),
            "https://example.test/app/late.js"
        );
        assert_eq!(
            merged.map.resolve("./late.js", &root).unwrap().as_str(),
            "https://example.test/app/late.js"
        );
    }

    #[test]
    fn resolved_module_set_is_bounded() {
        let parsed = parse_inline_import_map(
            "{}",
            Url::parse("https://example.test/app/page.html").unwrap(),
        )
        .unwrap();
        let root = Url::parse("https://example.test/app/root.js").unwrap();
        for index in 0..MAX_RESOLVED_MODULE_SPECIFIERS {
            parsed
                .map
                .resolve(&format!("./module-{index}.js"), &root)
                .unwrap();
        }
        assert!(
            parsed
                .map
                .resolve("./one-too-many.js", &root)
                .unwrap_err()
                .contains("resolved module specifiers")
        );
    }

    #[test]
    fn resolved_module_set_has_a_total_byte_bound() {
        let parsed = parse_inline_import_map(
            "{}",
            Url::parse("https://example.test/app/page.html").unwrap(),
        )
        .unwrap();
        let root = Url::parse("https://example.test/app/root.js").unwrap();
        let long_segment = "a".repeat(15_000);
        let mut error = None;
        for index in 0..MAX_RESOLVED_MODULE_SPECIFIERS {
            if let Err(message) = parsed
                .map
                .resolve(&format!("./{long_segment}-{index}.js"), &root)
            {
                error = Some(message);
                break;
            }
        }
        assert!(error.is_some_and(|message| message.contains("resolved module specifiers exceed")));
    }

    #[test]
    fn merged_normalized_state_has_a_total_byte_bound() {
        let base = Url::parse("https://example.test/app/page.html").unwrap();
        let mut current = parse_inline_import_map("{}", base.clone()).unwrap().map;
        let long_segment = "a".repeat(8_000);
        let mut error = None;
        for batch in 0..8 {
            let imports = (0..16)
                .map(|index| {
                    (
                        format!("module-{batch}-{index}"),
                        Value::String(format!("./{long_segment}-{batch}-{index}.js")),
                    )
                })
                .collect::<deno_core::serde_json::Map<_, _>>();
            let source = Value::Object(
                [("imports".to_owned(), Value::Object(imports))]
                    .into_iter()
                    .collect(),
            )
            .to_string();
            match merge_inline_import_map(&current, &source, base.clone()) {
                Ok(merged) => current = merged.map,
                Err(message) => {
                    error = Some(message);
                    break;
                }
            }
        }
        assert!(error.is_some_and(|message| message.contains("normalized bytes")));
    }
}
