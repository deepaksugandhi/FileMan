//! Native Windows drag-out support: lets users drag files FROM FileMan's
//! listings INTO other applications (chat clients, mail, Explorer…).
//!
//! egui's built-in drag & drop is app-internal only — other processes never
//! see it. Handing a drag to the OS requires an OLE data source carrying
//! `CF_HDROP`, which is what `start_drag_out` builds: one PIDL per path via
//! `SHParseDisplayName`, wrapped in a shell `IDataObject`
//! (`SHCreateDataObject`), driven by a minimal `IDropSource` through
//! `DoDragDrop`.
//!
//! `DoDragDrop` runs a nested modal loop and BLOCKS until the user drops or
//! cancels; the caller (app.rs) only invokes it once the pointer has already
//! left FileMan's window mid-drag, so the internal pane/tab DnD keeps
//! working inside the window and the OS takes over at its edge.

/// Result of an out-of-app drag operation.
#[derive(Debug)]
pub enum DragOutOutcome {
    /// A target accepted the drop. `moved` means the target requested a
    /// MOVE, so the caller must delete the source files itself.
    Dropped { moved: bool },
    /// Released with no taker, or cancelled (Esc / right-click).
    Cancelled,
    /// Could not even start (COM mode mismatch, path resolution failure…).
    Failed(String),
}

/// Initializes OLE on the main thread. Required before `DoDragDrop`; call
/// once at startup. Safe to call when COM is already initialized.
pub fn init_ole() {
    #[cfg(windows)]
    unsafe {
        // S_FALSE ("already initialized") and RPC_E_CHANGED_MODE are both
        // fine to ignore: either OLE is up, or COM is pinned to a mode this
        // thread can't change — DoDragDrop then reports a clean failure.
        let _ = windows::Win32::System::Ole::OleInitialize(None);
    }
}

/// Starts an OS-level drag of `paths`. Blocks until the drag resolves.
#[allow(clippy::unused_async)]
pub fn start_drag_out(paths: &[std::path::PathBuf]) -> DragOutOutcome {
    #[cfg(windows)]
    {
        imp::start_drag_out(paths)
    }
    #[cfg(not(windows))]
    {
        let _ = paths;
        DragOutOutcome::Failed("Drag-out is only supported on Windows".to_string())
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    use windows::core::{implement, PCWSTR};
    use windows::Win32::Foundation::{
        DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, S_OK,
    };
    use windows::Win32::System::Com::{CoTaskMemFree, IDataObject};
    use windows::Win32::System::Ole::{
        DoDragDrop, IDropSource, IDropSource_Impl, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_LINK,
        DROPEFFECT_MOVE,
    };
    use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
    use windows::Win32::UI::Shell::Common::ITEMIDLIST;
    use windows::Win32::UI::Shell::{SHCreateDataObject, SHParseDisplayName};

    /// Drop source that ends the OLE loop on Esc or button release and uses
    /// the OS-provided drag cursors.
    #[implement(IDropSource)]
    struct FileDropSource;

    impl IDropSource_Impl for FileDropSource_Impl {
        fn QueryContinueDrag(
            &self,
            fescapepressed: windows_core::BOOL,
            grfkeystate: MODIFIERKEYS_FLAGS,
        ) -> windows_core::HRESULT {
            if fescapepressed.as_bool() {
                DRAGDROP_S_CANCEL
            } else if grfkeystate & MK_LBUTTON == MODIFIERKEYS_FLAGS(0) {
                DRAGDROP_S_DROP
            } else {
                S_OK
            }
        }

        fn GiveFeedback(&self, _dweffect: DROPEFFECT) -> windows_core::HRESULT {
            DRAGDROP_S_USEDEFAULTCURSORS
        }
    }

    pub(super) fn start_drag_out(paths: &[std::path::PathBuf]) -> DragOutOutcome {
        unsafe {
            // One shell PIDL per path — SHCreateDataObject turns the list
            // into a data object exposing CF_HDROP (plus shell extras like
            // file contents on demand).
            let mut pidls: Vec<*mut ITEMIDLIST> = Vec::with_capacity(paths.len());
            for path in paths {
                let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
                let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
                if SHParseDisplayName(PCWSTR(wide.as_ptr()), None, &mut pidl, 0, None).is_err()
                    || pidl.is_null()
                {
                    continue;
                }
                pidls.push(pidl);
            }
            if pidls.is_empty() {
                return DragOutOutcome::Failed("Could not resolve the dragged file(s)".to_string());
            }

            let refs: Vec<*const ITEMIDLIST> =
                pidls.iter().map(|p| *p as *const ITEMIDLIST).collect();
            let outcome = match SHCreateDataObject(None, Some(&refs), None::<&IDataObject>) {
                Ok(data_object) => {
                    let source: IDropSource = FileDropSource.into();
                    let mut effect = DROPEFFECT(0);
                    let hr = DoDragDrop(
                        &data_object as &IDataObject,
                        &source as &IDropSource,
                        DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK,
                        &mut effect,
                    );
                    if hr.is_err() {
                        DragOutOutcome::Failed(format!("Drag failed: {hr}"))
                    } else if effect == DROPEFFECT(0) {
                        DragOutOutcome::Cancelled
                    } else {
                        DragOutOutcome::Dropped {
                            moved: effect & DROPEFFECT_MOVE == DROPEFFECT_MOVE,
                        }
                    }
                }
                Err(e) => DragOutOutcome::Failed(format!("Drag setup failed: {e}")),
            };

            for pidl in pidls {
                CoTaskMemFree(Some(pidl.cast()));
            }
            outcome
        }
    }
}
