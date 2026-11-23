use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use lsp_types::{Diagnostic, PublishDiagnosticsParams, Url};

use crate::global_state::GlobalState;
use crate::source_mapping::MapDirection::FromPreprocess;

/// Origin key of diagnostics produced by in-process preprocessing.
pub const PREPROCESS_ORIGIN: &str = "<preprocess>";

/// Remove diagnostics whose message matches one of the `patterns`.
pub fn remove_suppressed(patterns: &[regex::Regex], diagnostics: &mut Vec<Diagnostic>) {
    diagnostics.retain(|d| {
        let suppressed = patterns.iter().any(|p| p.is_match(&d.message));
        if suppressed {
            debug!("Suppressing diagnostic: {}", d.message);
        }
        !suppressed
    });
}

/// All cached diagnostics of `source`, as one notification.
///
/// A source file maps into several generated files (header, internal header,
/// implementation) and the server publishes diagnostics per generated file,
/// so they have to be merged instead of replacing each other.
pub fn publish_merged(state: &GlobalState, source: &Path) -> Option<PublishDiagnosticsParams> {
    let uri = Url::from_file_path(source).ok()?;
    let mut seen = HashSet::new();
    let mut diagnostics = Vec::new();
    let mut origins: Vec<_> = state.diagnostics.get(source).into_iter().flatten().collect();
    origins.sort_by(|a, b| a.0.cmp(b.0));
    for (_, list) in origins {
        for diagnostic in list {
            let key = serde_json::to_string(diagnostic).unwrap_or_default();
            if seen.insert(key) {
                diagnostics.push(diagnostic.clone());
            }
        }
    }
    remove_suppressed(&state.suppressed_diagnostics, &mut diagnostics);
    Some(PublishDiagnosticsParams { uri, diagnostics, version: None })
}

pub fn handle_publish_diagnostics(
    state: &mut GlobalState,
    mut params: PublishDiagnosticsParams,
) -> Vec<PublishDiagnosticsParams> {
    // Unmapped files are passed through, so filter here as well.
    remove_suppressed(&state.suppressed_diagnostics, &mut params.diagnostics);
    if params.uri.scheme() != "file" {
        info!("PublishDiagnostics: Encountered unsupported scheme {}.", params.uri);
        return vec![params];
    }

    let generated = PathBuf::from(params.uri.path());
    let files = state.source_mapping.map_files(FromPreprocess, params.uri.path());
    if files.is_empty() {
        warn!("PublishDiagnostics: Encountered unknown file {}.", params.uri.path());
        return vec![params];
    }

    let mut result: HashMap<PathBuf, Vec<Diagnostic>> = HashMap::new();
    for mut diagnostic in params.diagnostics {
        let mut path = params.uri.path().to_owned();
        if diagnostic.range.start == diagnostic.range.end {
            // Diagnostic for the entire file
            for file in files {
                result.entry(file.clone()).or_default().push(diagnostic.clone());
            }
        } else if state
            .source_mapping
            .map_range(FromPreprocess, &mut path, &mut diagnostic.range)
            .is_ok()
        {
            result.entry(PathBuf::from(path)).or_default().push(diagnostic);
        }
    }

    // Replace the diagnostics originating from this generated file in every
    // source file mapped into it (or that had some from it), and publish the
    // merged result. Clean source files get an explicit empty list, so the
    // client knows they have been analyzed.
    let mut affected: HashSet<PathBuf> = result.keys().cloned().collect();
    affected.extend(files.iter().cloned());
    for (source, origins) in &state.diagnostics {
        if origins.contains_key(&generated) {
            affected.insert(source.clone());
        }
    }
    let mut notifications = Vec::new();
    for source in affected {
        let list = result.remove(&source).unwrap_or_default();
        state.diagnostics.entry(source.clone()).or_default().insert(generated.clone(), list);
        notifications.extend(publish_merged(state, &source));
    }
    notifications
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{Position, Range};

    #[test]
    fn removes_matching_diagnostics() {
        let diagnostic = |message: &str| Diagnostic {
            range: Range::new(Position::new(0, 0), Position::new(0, 0)),
            message: message.into(),
            ..Default::default()
        };
        let mut diagnostics = vec![
            diagnostic("Unsupported option '-freg-struct-return' for target 'x86_64'"),
            diagnostic("use of undeclared identifier 'x'"),
            diagnostic("Unknown argument: '-mno-thumb-interwork'"),
        ];
        let patterns = [
            regex::Regex::new("^Unsupported option").unwrap(),
            regex::Regex::new("^Unknown argument").unwrap(),
        ];
        remove_suppressed(&patterns, &mut diagnostics);
        let messages: Vec<_> = diagnostics.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(messages, ["use of undeclared identifier 'x'"]);
    }
}
