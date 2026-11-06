//! Native dialogs used by typed app commands; no dialog permission is granted to the webview.

use std::path::PathBuf;

use tauri::AppHandle;
use tauri_plugin_dialog::DialogExt;

/// Resolves a selected native file or reports a conversion failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum NativeDialogError {
    /// The native dialog returned a path that could not become a local path.
    #[error("the selected path is not available as a local filesystem path")]
    InvalidPath,
}

/// Opens a native save dialog and returns `None` when the user cancels.
///
/// Run this function inside a blocking worker; native dialogs block until dismissed.
///
/// # Errors
/// Returns [`NativeDialogError::InvalidPath`] if the native result cannot be resolved.
pub fn choose_save_file(
    app: &AppHandle,
    title: &str,
    file_name: &str,
    extension: &str,
) -> Result<Option<PathBuf>, NativeDialogError> {
    let selected = app
        .dialog()
        .file()
        .set_title(title)
        .set_file_name(file_name)
        .add_filter("Archive", &[extension])
        .blocking_save_file();
    selected
        .map(|path| path.into_path().map_err(|_| NativeDialogError::InvalidPath))
        .transpose()
}

/// Opens a native folder picker and returns `None` when the user cancels.
///
/// Run this function inside a blocking worker; native dialogs block until dismissed.
///
/// # Errors
/// Returns [`NativeDialogError::InvalidPath`] if the native result cannot be resolved.
pub fn choose_folder(app: &AppHandle, title: &str) -> Result<Option<PathBuf>, NativeDialogError> {
    let selected = app.dialog().file().set_title(title).blocking_pick_folder();
    selected
        .map(|path| path.into_path().map_err(|_| NativeDialogError::InvalidPath))
        .transpose()
}
