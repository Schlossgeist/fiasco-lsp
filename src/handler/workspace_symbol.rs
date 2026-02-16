use lsp_types::{OneOf, Url, WorkspaceSymbolResponse};

use crate::global_state::{GlobalState, ReqContext};
use crate::source_mapping::MapDirection::FromPreprocess;

pub fn handle_res_workspace_symbol(
    state: &mut GlobalState,
    _: &mut ReqContext,
    res: Option<WorkspaceSymbolResponse>,
) -> Option<WorkspaceSymbolResponse> {
    let mut result = res?;
    match &mut result {
        WorkspaceSymbolResponse::Flat(symbols) => {
            symbols.retain_mut(|symbol| {
                if state.source_mapping.map_location(FromPreprocess, &mut symbol.location).is_err()
                {
                    warn!("Drop {} due to map location error.", &symbol.name);
                    return false;
                }
                true
            });
        }
        WorkspaceSymbolResponse::Nested(symbols) => {
            symbols.retain_mut(|symbol| {
                match &mut symbol.location {
                    OneOf::Left(location) => {
                        if state.source_mapping.map_location(FromPreprocess, location).is_err() {
                            warn!("Drop {} due to map location error.", &symbol.name);
                            return false;
                        }
                    }
                    OneOf::Right(workspace_location) => {
                        if workspace_location.uri.scheme() != "file" {
                            info!("Encountered unsupported scheme {}.", workspace_location.uri);
                            return true;
                        }

                        let files = state
                            .source_mapping
                            .map_files(FromPreprocess, workspace_location.uri.path());
                        if files.len() != 1 {
                            warn!("Drop {} due to map location uri error.", &symbol.name);
                            return false;
                        }

                        workspace_location.uri = Url::from_file_path(&files[0]).unwrap();
                    }
                }
                true
            });
        }
    };

    Some(result)
}
