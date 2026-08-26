//! Native Windows drag & drop: lets users drag files FROM FileMan's listings
//! INTO other applications (chat clients, mail, Explorer…) and drop files
//! FROM other applications (or from FileMan itself) onto FileMan's panes.
//!
//! egui's built-in drag & drop is app-internal only — other processes never
//! see it. Handing a drag to the OS requires an OLE data source carrying
//! `CF_HDROP`, which is what `start_drag_out` builds: one PIDL per path via
//! `SHParseDisplayName`, wrapped in a shell `IDataObject`
//! (`SHCreateDataObject`), driven by a minimal `IDropSource` through
//! `DoDragDrop`.
//!
//! `DoDragDrop` must be called the moment the drag gesture starts (while the
//! mouse button is still down), not deferred until the cursor is observed
//! leaving the window: `IDropSource::QueryContinueDrag` is only invoked in
//! response to the *actual* `WM_LBUTTONUP`/`WM_MOUSEMOVE` messages arriving
//! while `DoDragDrop`'s own loop is pumping — if the real button-up message
//! already went to egui's normal input handling before `DoDragDrop` starts,
//! the call has nothing left telling it the button is up and can hang. So
//! this module calls `DoDragDrop` synchronously as soon as `app.rs` detects
//! a row-drag has started; it blocks until the drop resolves, which egui
//! tolerates fine (Windows keeps pumping `WM_PAINT` etc. through the nested
//! loop).
//!
//! Because `DoDragDrop` owns the mouse for the whole gesture, a drop back
//! onto our own window has to go through the same OS mechanism rather than
//! egui's `Sense::drag()` — so this module also registers FileMan's window
//! as an OLE drop target (`RegisterDragDrop`/`IDropTarget`). That target
//! doubles as the handler for genuinely external drops (dragging in from
//! Explorer): `main.rs` disables winit's own built-in drop-target
//! registration (`ViewportBuilder::with_drag_and_drop(false)`) so the two
//! don't collide — only one `IDropTarget` may be registered per HWND.
//!
//! `DndSharedState` is the hand-off point between the two: `app.rs` writes
//! this frame's pane/tab hit-test rects (in egui points) into it before
//! starting a drag, and `FileDropTarget`'s OLE callbacks — invoked directly
//! by Windows, off egui's normal per-frame update — read those rects to
//! resolve a drop position, and write the result back as a `PendingDrop`
//! for `app.rs` to pick up and actually perform (there's no `&mut
//! FileManApp` available from inside a COM callback).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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

/// A drop resolved onto one of FileMan's own panes/tabs, whether from a
/// drag that originated in this window (self-drop, reordering/copying
/// between panes) or a genuinely external one (Explorer, etc.) landing
/// directly on us. Written by `FileDropTarget::Drop`, consumed by `app.rs`.
pub struct PendingDrop {
    pub paths: Vec<PathBuf>,
    pub pane: usize,
    /// `Some` when the drop landed precisely on a tab header.
    pub tab: Option<usize>,
    /// MOVE vs COPY. Only ever true for a self-drop (Shift held); an
    /// external drop always copies — the source isn't ours to move from.
    pub is_move: bool,
}

/// Hit-testable geometry for this frame's panes/tabs, in egui points
/// (matching `FileManApp::dnd_pane_rects`/`dnd_tab_rects`), plus the
/// hand-off slot for a resolved drop. Refreshed by `app.rs` every frame so
/// `FileDropTarget`'s OLE callbacks can resolve drop positions without
/// needing `&mut FileManApp`.
#[derive(Default)]
pub struct DndSharedState {
    pub pixels_per_point: f32,
    pub pane_rects: [Option<(f32, f32, f32, f32)>; 2],
    pub tab_rects: Vec<((usize, usize), (f32, f32, f32, f32))>,
    /// True while a row-drag started by `app.rs` is in flight, so `Drop`
    /// knows whether Shift means MOVE (internal move) or to force COPY (an
    /// external drop always copies).
    pub own_drag: bool,
    pub pending_drop: Option<PendingDrop>,
}

pub type SharedDndState = Arc<Mutex<DndSharedState>>;

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

/// Registers FileMan's window as an OLE drop target so drops — whether from
/// an external app or from our own `start_drag_out` looping back onto our
/// own panes — resolve through `state`. Idempotent-per-hwnd is the caller's
/// responsibility (`RegisterDragDrop` errors if called twice on the same
/// window without a `RevokeDragDrop` in between).
pub fn register_drop_target(
    hwnd: windows::Win32::Foundation::HWND,
    state: SharedDndState,
) -> Result<(), String> {
    #[cfg(windows)]
    {
        imp::register_drop_target(hwnd, state)
    }
    #[cfg(not(windows))]
    {
        let _ = (hwnd, state);
        Ok(())
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
    use windows::Win32::Foundation::{
        DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, POINT, S_OK,
    };
    use windows::Win32::Graphics::Gdi::ScreenToClient;
    use windows::Win32::System::Com::{
        CoTaskMemFree, DVASPECT_CONTENT, FORMATETC, IDataObject, TYMED_HGLOBAL,
    };
    use windows::Win32::System::Ole::{
        CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE, DROPEFFECT_NONE,
        DoDragDrop, IDropSource, IDropSource_Impl, IDropTarget, IDropTarget_Impl, ReleaseStgMedium,
    };
    use windows::Win32::System::SystemServices::{MK_LBUTTON, MK_SHIFT, MODIFIERKEYS_FLAGS};
    use windows::Win32::UI::Shell::Common::ITEMIDLIST;
    use windows::Win32::UI::Shell::{
        BHID_DataObject, ILCreateFromPathW, SHCreateShellItemArrayFromIDLists,
    };
    use windows::core::{PCWSTR, implement};

    pub(super) fn register_drop_target(
        hwnd: windows::Win32::Foundation::HWND,
        state: SharedDndState,
    ) -> Result<(), String> {
        let target: IDropTarget = FileDropTarget { hwnd, state }.into();
        unsafe { windows::Win32::System::Ole::RegisterDragDrop(hwnd, &target) }
            .map_err(|e| e.to_string())
    }

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
            // One absolute shell PIDL per path, wrapped into an
            // IShellItemArray and bound to BHID_DataObject for a properly
            // shell-constructed IDataObject exposing CF_HDROP.
            //
            // NB: `SHCreateDataObject`'s `apidl` parameter wants *relative*
            // child PIDLs under a single `pidlfolder`, not one absolute PIDL
            // per (possibly differently-located) file — passing full PIDLs
            // with no parent folder builds a malformed data object that a
            // permissive drop target (e.g. a browser-based app treating it
            // as text) might accept, but a real shell target like Explorer's
            // listview validates and rejects (shown as the "no drop" cursor,
            // matching the reported bug).
            let mut pidls: Vec<*mut ITEMIDLIST> = Vec::with_capacity(paths.len());
            for path in paths {
                let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
                let pidl = ILCreateFromPathW(PCWSTR(wide.as_ptr()));
                if pidl.is_null() {
                    continue;
                }
                pidls.push(pidl);
            }
            if pidls.is_empty() {
                return DragOutOutcome::Failed("Could not resolve the dragged file(s)".to_string());
            }

            let refs: Vec<*const ITEMIDLIST> =
                pidls.iter().map(|p| *p as *const ITEMIDLIST).collect();
            let data_object: windows_core::Result<IDataObject> =
                SHCreateShellItemArrayFromIDLists(&refs)
                    .and_then(|arr| arr.BindToHandler(None, &BHID_DataObject));
            let outcome = match data_object {
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

    /// OLE drop target for FileMan's window — see the module docs for why
    /// this exists instead of egui's own drag & drop. `DragEnter`/`DragOver`
    /// hit-test against this frame's rects (written by `app.rs` just before
    /// starting a drag) purely to give the OS the right cursor; `Drop` does
    /// the same hit-test, pulls the file list out of the data object, and
    /// hands the result to `app.rs` via `pending_drop` — there's no `&mut
    /// FileManApp` reachable from here, since Windows calls this directly,
    /// off egui's normal update loop.
    #[implement(IDropTarget)]
    struct FileDropTarget {
        hwnd: windows::Win32::Foundation::HWND,
        state: SharedDndState,
    }

    impl FileDropTarget_Impl {
        /// Screen-space `pt` -> egui points -> the pane/tab under it, if any.
        fn hit_test(
            &self,
            pt: &windows::Win32::Foundation::POINTL,
        ) -> Option<(usize, Option<usize>)> {
            let mut p = POINT { x: pt.x, y: pt.y };
            unsafe {
                let _ = ScreenToClient(self.hwnd, &mut p);
            }
            let st = self.state.lock().unwrap();
            if st.pixels_per_point <= 0.0 {
                return None;
            }
            let (x, y) = (
                p.x as f32 / st.pixels_per_point,
                p.y as f32 / st.pixels_per_point,
            );
            for &((pane, tab), (l, t, r, b)) in &st.tab_rects {
                if x >= l && x <= r && y >= t && y <= b {
                    return Some((pane, Some(tab)));
                }
            }
            for (pane, rect) in st.pane_rects.iter().enumerate() {
                if let Some((l, t, r, b)) = rect {
                    if x >= *l && x <= *r && y >= *t && y <= *b {
                        return Some((pane, None));
                    }
                }
            }
            None
        }

        /// Reads the `CF_HDROP` file list out of a drop's data object.
        fn extract_paths(data_obj: &IDataObject) -> Vec<std::path::PathBuf> {
            unsafe {
                let fmt = FORMATETC {
                    cfFormat: CF_HDROP.0,
                    ptd: std::ptr::null_mut(),
                    dwAspect: DVASPECT_CONTENT.0,
                    lindex: -1,
                    tymed: TYMED_HGLOBAL.0 as u32,
                };
                let Ok(mut medium) = data_obj.GetData(&fmt) else {
                    return Vec::new();
                };
                let hdrop = windows::Win32::UI::Shell::HDROP(medium.u.hGlobal.0);
                let count = windows::Win32::UI::Shell::DragQueryFileW(hdrop, 0xFFFF_FFFF, None);
                let mut out = Vec::with_capacity(count as usize);
                for i in 0..count {
                    let len = windows::Win32::UI::Shell::DragQueryFileW(hdrop, i, None) as usize;
                    let mut buf = vec![0u16; len + 1];
                    windows::Win32::UI::Shell::DragQueryFileW(hdrop, i, Some(&mut buf));
                    let s = String::from_utf16_lossy(&buf[..len]);
                    if !s.is_empty() {
                        out.push(std::path::PathBuf::from(s));
                    }
                }
                ReleaseStgMedium(&mut medium);
                out
            }
        }
    }

    #[allow(non_snake_case)]
    impl IDropTarget_Impl for FileDropTarget_Impl {
        fn DragEnter(
            &self,
            _pdataobj: windows_core::Ref<'_, IDataObject>,
            grfkeystate: MODIFIERKEYS_FLAGS,
            pt: &windows::Win32::Foundation::POINTL,
            pdweffect: *mut DROPEFFECT,
        ) -> windows_core::Result<()> {
            self.DragOver(grfkeystate, pt, pdweffect)
        }

        fn DragOver(
            &self,
            grfkeystate: MODIFIERKEYS_FLAGS,
            pt: &windows::Win32::Foundation::POINTL,
            pdweffect: *mut DROPEFFECT,
        ) -> windows_core::Result<()> {
            let own_drag = self.state.lock().unwrap().own_drag;
            let effect = match self.hit_test(pt) {
                None => DROPEFFECT_NONE,
                Some(_) if own_drag && (grfkeystate & MK_SHIFT) != MODIFIERKEYS_FLAGS(0) => {
                    DROPEFFECT_MOVE
                }
                Some(_) => DROPEFFECT_COPY,
            };
            unsafe {
                *pdweffect = effect;
            }
            Ok(())
        }

        fn DragLeave(&self) -> windows_core::Result<()> {
            Ok(())
        }

        fn Drop(
            &self,
            pdataobj: windows_core::Ref<'_, IDataObject>,
            grfkeystate: MODIFIERKEYS_FLAGS,
            pt: &windows::Win32::Foundation::POINTL,
            pdweffect: *mut DROPEFFECT,
        ) -> windows_core::Result<()> {
            let target = self.hit_test(pt);
            let (Some(data_obj), Some((pane, tab))) = (pdataobj.as_ref(), target) else {
                unsafe {
                    *pdweffect = DROPEFFECT_NONE;
                }
                return Ok(());
            };
            let paths = Self::extract_paths(data_obj);
            if paths.is_empty() {
                unsafe {
                    *pdweffect = DROPEFFECT_NONE;
                }
                return Ok(());
            }
            let own_drag = self.state.lock().unwrap().own_drag;
            let is_move = own_drag && (grfkeystate & MK_SHIFT) != MODIFIERKEYS_FLAGS(0);
            self.state.lock().unwrap().pending_drop = Some(PendingDrop {
                paths,
                pane,
                tab,
                is_move,
            });
            unsafe {
                *pdweffect = if is_move {
                    DROPEFFECT_MOVE
                } else {
                    DROPEFFECT_COPY
                };
            }
            Ok(())
        }
    }
}
