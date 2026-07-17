//! Bounded import-map parsing and resolution for parser-discovered page modules.

use std::sync::Arc;

use deno_core::serde_json::Value;
use url::Url;

const MAX_IMPORT_MAP_SOURCE_BYTES: usize = 256 * 1024;
const MAX_IMPORT_MAP_ENTRIES: usize = 2_048;
const MAX_IMPORT_MAP_SCOPES: usize = 128;
const MAX_IMPORT_MAP_STRING_BYTES: usize = 16 * 1024;
const MAX_IMPORT_MAP_DIAGNOSTICS: usize = 32;
const MAX_IMPORT_MAP_DIAGNOSTIC_BYTES: usize = 1_024;
pub(super) const MAX_MODULE_SPECIFIER_BYTES: usize = 16 * 1024;

#[derive(Clone)]
pub(super) struct PageImportMap(Arc<import_map::ImportMap>);

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
    let value = deno_core::serde_json::from_str::<Value>(source)
        .map_err(|error| format!("import map JSON is invalid: {error}"))?;
    validate_import_map_shape(&value)?;
    let parsed = import_map::parse_from_value(base_url, value)
        .map_err(|error| format!("import map is invalid: {error}"))?;
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
        map: PageImportMap(Arc::new(parsed.import_map)),
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
        let resolved = self
            .0
            .resolve(specifier, referrer)
            .map_err(|error| error.to_string())?;
        if resolved.as_str().len() > MAX_MODULE_SPECIFIER_BYTES {
            return Err("resolved module URL exceeds the import map limit".to_owned());
        }
        Ok(resolved)
    }
}

fn validate_import_map_shape(value: &Value) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| "import map JSON must be an object".to_owned())?;
    if object.contains_key("integrity") {
        return Err("import map integrity metadata is not supported".to_owned());
    }

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
    }

    #[test]
    fn rejects_integrity_and_bounded_shapes() {
        assert!(
            parse_inline_import_map(
                r#"{"imports": {}, "integrity": {}}"#,
                Url::parse("https://example.test/map.json").unwrap(),
            )
            .is_err()
        );
        let oversized = "x".repeat(MAX_IMPORT_MAP_STRING_BYTES + 1);
        let source = deno_core::serde_json::json!({ "imports": { oversized: "./x.js" } });
        assert!(
            parse_inline_import_map(
                &source.to_string(),
                Url::parse("https://example.test/map.json").unwrap(),
            )
            .is_err()
        );
    }
}
