use lsp_types::{GotoDefinitionResponse, Location};

use crate::global_state::{GlobalState, ReqContext};
use crate::source_mapping::MapDirection::FromPreprocess;

pub fn handle_res_goto(
    state: &mut GlobalState,
    req_context: &mut ReqContext,
    res: Option<GotoDefinitionResponse>,
) -> Option<GotoDefinitionResponse> {
    debug!(target: "goto", "Raw response: {}", to_json(&res));
    let (source_path, mapped_file) = match req_context.take_value::<(String, String)>() {
        None => {
            warn!(target: "goto", "No request context (source/mapped file) found, passing response through unmapped.");
            return res;
        }
        Some(t) => t,
    };
    debug!(target: "goto", "Request source file: {}, mapped (generated) file: {}", source_path, mapped_file);
    let mut result = match res {
        None => {
            debug!(target: "goto", "Server returned no result.");
            return None;
        }
        Some(r) => r,
    };
    let mapping = &state.source_mapping;
    match &mut result {
        GotoDefinitionResponse::Scalar(location) => {
            // TODO: Pass-through or ignore on map failure?
            let _ = mapping.map_server_location_or_file(location);
        }
        GotoDefinitionResponse::Array(vec) => {
            vec.retain_mut(|location| mapping.map_server_location_or_file(location))
        }
        GotoDefinitionResponse::Link(vec) => vec.retain_mut(|link| {
            // The origin selection always lies in the requesting (generated)
            // file, independent of where the target is.
            if let Some(origin_selection_range) = link.origin_selection_range.as_mut() {
                let mut path = mapped_file.clone();
                if mapping.map_range(FromPreprocess, &mut path, origin_selection_range).is_err() {
                    // Better no highlight than one at a wrong position.
                    link.origin_selection_range = None;
                } else if source_path != path {
                    warn!(
                        target: "goto",
                        "Origin selection mapped to different file ({}) than source file specified in request ({}).",
                        &path, &source_path
                    );
                }
            }

            let mut target = Location::new(link.target_uri.clone(), link.target_range);
            let mut selection = Location::new(link.target_uri.clone(), link.target_selection_range);
            if !mapping.map_server_location_or_file(&mut target)
                || !mapping.map_server_location_or_file(&mut selection)
            {
                warn!(target: "goto", "Dropping link with unmappable target: {:?}", &link);
                return false;
            }

            if target.uri != selection.uri {
                warn!(
                    target: "goto",
                    "Dropping link: target_range {:?} mapped to different file than target_selection_range {:?}.",
                    &target, &selection
                );
                return false;
            }

            link.target_uri = target.uri;
            link.target_range = target.range;
            link.target_selection_range = selection.range;
            true
        }),
    }
    debug!(target: "goto", "Mapped response: {}", to_json(&result));
    Some(result)
}

/// Serialize for logging; never fails.
fn to_json<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|e| format!("<unserializable: {}>", e))
}

pub fn handle_res_references(
    state: &mut GlobalState,
    _req_context: &mut ReqContext,
    res: Option<Vec<Location>>,
) -> Option<Vec<Location>> {
    let mut result = res?;
    result.retain_mut(|location| state.source_mapping.map_server_location(location));
    Some(result)
}
