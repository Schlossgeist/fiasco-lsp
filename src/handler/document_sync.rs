use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lsp_types::notification::{
    DidChangeTextDocument, DidChangeWatchedFiles, DidCloseTextDocument, DidOpenTextDocument,
    Notification,
};
use lsp_types::{
    DidChangeTextDocumentParams, DidChangeWatchedFilesParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DidSaveTextDocumentParams, FileChangeType, FileEvent,
    TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem, Url,
    VersionedTextDocumentIdentifier,
};

use crate::global_state::GlobalState;
use crate::handler::diagnostics::{publish_merged, PREPROCESS_ORIGIN};
use crate::preprocess::Severity;
use crate::source_mapping::MapDirection::ToPreprocess;
use crate::standalone::{mapped_outputs, Regenerated};

/// The source file of a document in standalone mode, if it is preprocessed.
fn standalone_source(state: &GlobalState, uri: &Url) -> Option<PathBuf> {
    let standalone = state.standalone.as_ref()?;
    if uri.scheme() != "file" {
        return None;
    }
    standalone.source_path(&uri.to_file_path().ok()?)
}

fn send_notification<N: Notification>(state: &mut GlobalState, params: N::Params) {
    let notification = lsp_server::Notification::new(N::METHOD.to_owned(), params);
    if let Err(e) = state.send_to_server(notification) {
        error!("Unable to send {} to server: {e}", N::METHOD);
    }
}

fn next_version(state: &mut GlobalState, path: &Path) -> i32 {
    let version = state.server_versions.entry(path.to_owned()).or_insert(0);
    *version += 1;
    *version
}

/// Regenerate the module of `source` and bring the server up to date.
fn regenerate(state: &mut GlobalState, source: &Path) {
    let standalone = state.standalone.as_mut().unwrap();
    let Some(job) = standalone.job_of(source) else { return };
    match standalone.regenerate(job, &mut state.source_mapping) {
        Ok(regenerated) => sync_server(state, regenerated),
        Err(e) => error!("Preprocessing {} failed: {e:?}", source.display()),
    }
}

/// Send changed generated files to the server and publish preprocess diagnostics.
fn sync_server(state: &mut GlobalState, regenerated: Regenerated) {
    let mut watched = Vec::new();
    for path in regenerated.changed {
        let Ok(uri) = Url::from_file_path(&path) else { continue };
        if state.open_files.contains_key(&path) {
            let text = state.standalone.as_ref().unwrap().generated_text(&path).unwrap_or_default().to_owned();
            let version = next_version(state, &path);
            send_notification::<DidChangeTextDocument>(
                state,
                DidChangeTextDocumentParams {
                    text_document: VersionedTextDocumentIdentifier::new(uri, version),
                    content_changes: vec![TextDocumentContentChangeEvent { range: None, range_length: None, text }],
                },
            );
        } else {
            watched.push(FileEvent { uri, typ: FileChangeType::CHANGED });
        }
    }
    if !watched.is_empty() {
        send_notification::<DidChangeWatchedFiles>(state, DidChangeWatchedFilesParams { changes: watched });
    }

    // Preprocess diagnostics of all inputs of the job.
    let standalone = state.standalone.as_ref().unwrap();
    let inputs = standalone.jobs[regenerated.job].inputs.clone();
    for input in inputs {
        let diagnostics: Vec<lsp_types::Diagnostic> = state.standalone.as_ref().unwrap()
            .diagnostics
            .get(&input)
            .into_iter()
            .flatten()
            .map(|d| lsp_types::Diagnostic {
                range: lsp_types::Range::new(
                    lsp_types::Position::new(d.line, 0),
                    lsp_types::Position::new(d.line + 1, 0),
                ),
                severity: Some(match d.severity {
                    Severity::Error => lsp_types::DiagnosticSeverity::ERROR,
                    Severity::Warning => lsp_types::DiagnosticSeverity::WARNING,
                }),
                source: Some("preprocess".into()),
                message: d.message.clone(),
                ..Default::default()
            })
            .collect();
        let entry = state.diagnostics.entry(input.clone()).or_default();
        if entry.get(Path::new(PREPROCESS_ORIGIN)).map_or(diagnostics.is_empty(), |d| *d == diagnostics) {
            continue;
        }
        entry.insert(PathBuf::from(PREPROCESS_ORIGIN), diagnostics);
        if let Some(params) = publish_merged(state, &input) {
            let notification = lsp_server::Notification::new(
                <lsp_types::notification::PublishDiagnostics as Notification>::METHOD.to_owned(),
                params,
            );
            if let Err(e) = state.send_to_client(notification) {
                error!("Unable to publish diagnostics: {e}");
            }
        }
    }
}

/// Open the generated files that contain lines of `source` in the server.
fn open_outputs(state: &mut GlobalState, source: &Path) {
    let targets = mapped_outputs(&state.source_mapping, source);
    let already = state.opened_for.get(source).cloned().unwrap_or_default();
    for path in targets {
        if already.contains(&path) {
            continue;
        }
        state.opened_for.entry(source.to_owned()).or_default().insert(path.clone());
        let count = state.open_files.entry(path.clone()).or_insert(0);
        *count += 1;
        if *count > 1 {
            continue;
        }
        let Some(text) = state.standalone.as_ref().unwrap().generated_text(&path).map(str::to_owned) else {
            continue;
        };
        let Ok(uri) = Url::from_file_path(&path) else { continue };
        let version = next_version(state, &path);
        send_notification::<DidOpenTextDocument>(
            state,
            DidOpenTextDocumentParams {
                text_document: TextDocumentItem { uri, language_id: "cpp".into(), version, text },
            },
        );
    }
}

fn close_outputs(state: &mut GlobalState, source: &Path) {
    for path in state.opened_for.remove(source).unwrap_or_default() {
        let Some(count) = state.open_files.get_mut(&path) else { continue };
        *count -= 1;
        if *count > 0 {
            continue;
        }
        state.open_files.remove(&path);
        let Ok(uri) = Url::from_file_path(&path) else { continue };
        send_notification::<DidCloseTextDocument>(
            state,
            DidCloseTextDocumentParams { text_document: TextDocumentIdentifier { uri } },
        );
    }
}

pub fn handle_did_open_text_document(
    state: &mut GlobalState,
    params: DidOpenTextDocumentParams,
) -> Vec<DidOpenTextDocumentParams> {
    if let Some(source) = standalone_source(state, &params.text_document.uri) {
        state.standalone.as_mut().unwrap().open(&source, &params.text_document.text);
        regenerate(state, &source);
        open_outputs(state, &source);
        return Vec::new();
    }
    let doc = &params.text_document;
    if doc.uri.scheme() != "file" {
        info!("DidOpenTextDocument: Encountered unsupported scheme {}.", doc.uri);
        return vec![params];
    }

    let files = state.source_mapping.map_files(ToPreprocess, doc.uri.path());
    if files.is_empty() {
        warn!("DidOpenTextDocument: Encountered unknown file {}.", doc.uri.path());
        return vec![params];
    }

    let mut result = Vec::new();
    for file in files {
        if let Some(count) = state.open_files.get_mut(file) {
            *count += 1;
            // File already opened, multiple source files might map to the same preprocessed file),
            // we must sent another open notification.
            continue;
        }

        // Remember that file is opened and send notification to server.
        state.open_files.insert(file.clone(), 1);
        result.push(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: Url::from_file_path(file).unwrap(),
                language_id: doc.language_id.clone(),
                // TODO: We need custom version numbering...
                version: doc.version,
                text: std::fs::read_to_string(file).unwrap(),
            },
        })
    }
    result
}

pub fn handle_did_change_text_document(
    state: &mut GlobalState,
    params: DidChangeTextDocumentParams,
) -> Vec<DidChangeTextDocumentParams> {
    if let Some(source) = standalone_source(state, &params.text_document.uri) {
        let encoding = state.position_encoding.clone();
        let standalone = state.standalone.as_mut().unwrap();
        if let Err(e) = standalone.change(&source, params.content_changes, &encoding) {
            error!("DidChangeTextDocument: {e:?}");
            return Vec::new();
        }
        regenerate(state, &source);
        // A change may add lines of the source to further generated files.
        open_outputs(state, &source);
        return Vec::new();
    }
    let doc = &params.text_document;
    if doc.uri.scheme() != "file" {
        info!("DidChangeTextDocument: Encountered unsupported scheme {}.", doc.uri);
        return vec![params];
    }

    let files = state.source_mapping.map_files(ToPreprocess, doc.uri.path());
    if files.is_empty() {
        warn!("DidChangeTextDocument: Encountered unknown file {}.", doc.uri.path());
        return vec![params];
    }

    let mut result = HashMap::new();
    for mut change in params.content_changes {
        match &mut change.range {
            Some(range) => {
                let mut path = doc.uri.path().to_owned();
                if state.source_mapping.map_range(ToPreprocess, &mut path, range).is_ok() {
                    result.entry(path).or_insert(Vec::new()).push(change);
                }
            }
            None => warn!("TODO: Changing of entire files not yet implemented."),
        }
    }

    result
        .into_iter()
        .map(|(file, changes)| DidChangeTextDocumentParams {
            // TODO: We need custom version numbering...
            text_document: VersionedTextDocumentIdentifier::new(
                Url::from_file_path(file).unwrap(),
                params.text_document.version,
            ),
            content_changes: changes,
        })
        .collect()
}

pub fn handle_did_close_text_document(
    state: &mut GlobalState,
    params: DidCloseTextDocumentParams,
) -> Vec<DidCloseTextDocumentParams> {
    if let Some(source) = standalone_source(state, &params.text_document.uri) {
        // Unsaved changes are discarded: regenerate from the file on disk.
        state.standalone.as_mut().unwrap().close(&source);
        regenerate(state, &source);
        close_outputs(state, &source);
        return Vec::new();
    }
    let doc = &params.text_document;
    if doc.uri.scheme() != "file" {
        info!("DidCloseTextDocument: Encountered unsupported scheme {}.", doc.uri);
        return vec![params];
    }

    let files = state.source_mapping.map_files(ToPreprocess, doc.uri.path());
    if files.is_empty() {
        warn!("DidCloseTextDocument: Encountered unknown file {}.", doc.uri.path());
        return vec![params];
    }

    let mut result = Vec::new();
    for file in files {
        match state.open_files.get_mut(file) {
            Some(count) => {
                if *count > 1 {
                    // Opened from other source file, do not send a close
                    // notification, just decrement the open count.
                    *count -= 1;
                    continue;
                }
            }
            None => {
                error!("DidCloseTextDocument: Tried to close non-open file {}.", file.display());
                continue;
            }
        }

        // Remove from opened files.
        state.open_files.remove(file);
        result.push(DidCloseTextDocumentParams {
            text_document: TextDocumentIdentifier { uri: Url::from_file_path(file).unwrap() },
        });
    }
    result
}

pub fn handle_did_save_text_document(
    state: &mut GlobalState,
    params: DidSaveTextDocumentParams,
) -> Vec<DidSaveTextDocumentParams> {
    // In standalone mode, generated files are always kept up to date.
    if standalone_source(state, &params.text_document.uri).is_some() {
        return Vec::new();
    }
    vec![params]
}
