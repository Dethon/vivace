//! Previous, play/pause and next buttons under the window's preview, the
//! one Windows shows while the pointer rests on the taskbar button. Other
//! systems have no such buttons.

use crate::media::MediaCommand;

/// What each button does, by the id Windows reports when it is clicked.
#[cfg_attr(not(windows), allow(dead_code))]
const COMMANDS: [MediaCommand; 3] = [
    MediaCommand::Previous,
    MediaCommand::Toggle,
    MediaCommand::Next,
];

/// A button's picture: the player bar's icon (`Icons` in theme.slint), whose
/// shapes all have straight edges, so they are listed here as polygons on
/// the same 24x24 grid.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Glyph {
    Previous,
    Play,
    Pause,
    Next,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl Glyph {
    const ALL: [Glyph; 4] = [Glyph::Previous, Glyph::Play, Glyph::Pause, Glyph::Next];

    fn shapes(self) -> &'static [&'static [(f32, f32)]] {
        match self {
            Glyph::Previous => &[
                &[(6.0, 6.0), (8.0, 6.0), (8.0, 18.0), (6.0, 18.0)],
                &[(9.5, 12.0), (18.0, 18.0), (18.0, 6.0)],
            ],
            Glyph::Play => &[&[(8.0, 5.0), (8.0, 19.0), (19.0, 12.0)]],
            Glyph::Pause => &[
                &[(6.0, 5.0), (10.0, 5.0), (10.0, 19.0), (6.0, 19.0)],
                &[(14.0, 5.0), (18.0, 5.0), (18.0, 19.0), (14.0, 19.0)],
            ],
            Glyph::Next => &[
                &[(6.0, 18.0), (14.5, 12.0), (6.0, 6.0)],
                &[(16.0, 6.0), (18.0, 6.0), (18.0, 18.0), (16.0, 18.0)],
            ],
        }
    }

    /// The glyph in `colour` on a transparent square `size` pixels wide, as
    /// RGBA rows from the top, its edges smoothed by sampling each pixel
    /// several times.
    fn render(self, size: u32, [red, green, blue]: [u8; 3]) -> Vec<u8> {
        // The part of the grid the glyphs cover, so they fill the small
        // button; a grid unit is a whole pixel at 16 and 32 pixels.
        const FROM: f32 = 4.0;
        const SPAN: f32 = 16.0;
        const SAMPLES: u32 = 4;

        let scale = SPAN / size as f32;
        let mut pixels = Vec::with_capacity((size * size * 4) as usize);
        for y in 0..size {
            for x in 0..size {
                let mut covered = 0;
                for sample_y in 0..SAMPLES {
                    for sample_x in 0..SAMPLES {
                        let offset = |pixel: u32, sample: u32| {
                            FROM + (pixel as f32 + (sample as f32 + 0.5) / SAMPLES as f32) * scale
                        };
                        let point = (offset(x, sample_x), offset(y, sample_y));
                        if self.shapes().iter().any(|shape| inside(shape, point)) {
                            covered += 1;
                        }
                    }
                }
                let alpha = covered * u32::from(u8::MAX) / (SAMPLES * SAMPLES);
                pixels.extend([red, green, blue, alpha as u8]);
            }
        }
        pixels
    }
}

/// Whether a point lies inside a polygon, by counting the edges a ray from
/// it crosses.
fn inside(polygon: &[(f32, f32)], (x, y): (f32, f32)) -> bool {
    let mut inside = false;
    let mut previous = polygon[polygon.len() - 1];
    for &point in polygon {
        let ((x1, y1), (x2, y2)) = (previous, point);
        if (y1 > y) != (y2 > y) && x < x1 + (y - y1) * (x2 - x1) / (y2 - y1) {
            inside = !inside;
        }
        previous = point;
    }
    inside
}

#[cfg(windows)]
pub use platform::ThumbBar;

#[cfg(windows)]
mod platform {
    use std::cell::Cell;
    use std::rc::Rc;

    use ::windows::Win32::Foundation::{ERROR_SUCCESS, HWND, LPARAM, LRESULT, WPARAM};
    use ::windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    };
    use ::windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    use ::windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
    use ::windows::Win32::UI::Shell::{
        DefSubclassProc, ITaskbarList3, RemoveWindowSubclass, SetWindowSubclass, THB_FLAGS,
        THB_ICON, THB_TOOLTIP, THBF_ENABLED, THBN_CLICKED, THUMBBUTTON, TaskbarList,
    };
    use ::windows::Win32::UI::WindowsAndMessaging::{
        CreateIcon, DestroyIcon, HICON, RegisterWindowMessageW, SM_CXSMICON, WM_COMMAND,
        WM_NCDESTROY,
    };
    use ::windows::core::w;
    use tokio::sync::mpsc::UnboundedSender;

    use super::{COMMANDS, Glyph};
    use crate::backend::Update;

    // Tells this subclass apart from any other on the window.
    const SUBCLASS_ID: usize = 1;

    pub struct ThumbBar {
        state: Rc<State>,
    }

    /// Shared with the window procedure, which runs on the same thread.
    struct State {
        window: HWND,
        taskbar: ITaskbarList3,
        updates: UnboundedSender<Update>,
        /// The message Explorer sends once it has made the taskbar button.
        button_created: u32,
        playing: Cell<bool>,
        /// Whether the buttons are on the current taskbar button.
        added: Cell<bool>,
        /// In [`Glyph::ALL`] order.
        icons: Cell<Option<[HICON; 4]>>,
    }

    impl ThumbBar {
        pub fn new(
            window: *mut std::ffi::c_void,
            updates: UnboundedSender<Update>,
        ) -> Result<Self, ::windows::core::Error> {
            let window = HWND(window);
            // SAFETY: plain calls; `window` is the app's window, alive on
            // this thread.
            let state = unsafe {
                // Already done by the window for drag and drop, which makes
                // this a no-op or a mismatch either way usable.
                _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                let taskbar: ITaskbarList3 =
                    CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER)?;
                taskbar.HrInit()?;
                Rc::new(State {
                    window,
                    taskbar,
                    updates,
                    button_created: RegisterWindowMessageW(w!("TaskbarButtonCreated")),
                    playing: Cell::new(false),
                    added: Cell::new(false),
                    icons: Cell::new(None),
                })
            };
            // SAFETY: the state outlives the subclass, which `Drop` removes
            // before letting go of it.
            let subclassed = unsafe {
                SetWindowSubclass(
                    window,
                    Some(subclass),
                    SUBCLASS_ID,
                    Rc::as_ptr(&state) as usize,
                )
            };
            subclassed.ok()?;
            // Explorer may have made the button before the window was
            // subclassed; if not yet, its message brings the buttons later.
            if let Err(error) = state.add() {
                log::debug!("no taskbar button for the thumbnail buttons yet: {error}");
            }
            Ok(Self { state })
        }

        pub fn set_playing(&self, playing: bool) {
            let state = &self.state;
            if state.playing.replace(playing) == playing || !state.added.get() {
                return;
            }
            let Some(icons) = state.icons.get() else {
                return;
            };
            // SAFETY: the window and the icons are alive.
            if let Err(error) = unsafe {
                state
                    .taskbar
                    .ThumbBarUpdateButtons(state.window, &state.buttons(icons))
            } {
                // As happens while the window is in the tray, without a
                // taskbar button.
                log::debug!("couldn't update the thumbnail buttons: {error}");
            }
        }
    }

    impl Drop for ThumbBar {
        fn drop(&mut self) {
            // SAFETY: removes the subclass added in `new`; harmless if the
            // window already went.
            unsafe {
                _ = RemoveWindowSubclass(self.state.window, Some(subclass), SUBCLASS_ID);
            }
            if let Some(icons) = self.state.icons.take() {
                destroy(icons);
            }
        }
    }

    impl State {
        /// Puts the buttons on the taskbar button, with icons drawn for the
        /// system's current theme and scale.
        fn add(&self) -> ::windows::core::Result<()> {
            let icons = self.make_icons()?;
            // SAFETY: the window and the icons are alive.
            let added = unsafe {
                self.taskbar
                    .ThumbBarAddButtons(self.window, &self.buttons(icons))
            };
            if let Err(error) = added {
                destroy(icons);
                return Err(error);
            }
            if let Some(old) = self.icons.replace(Some(icons)) {
                destroy(old);
            }
            self.added.set(true);
            log::info!("added the thumbnail buttons");
            Ok(())
        }

        /// A new taskbar button wants the buttons, while the one they were
        /// just added to, its message arriving late, already has them.
        fn on_button_created(&self) {
            let Err(error) = self.add() else { return };
            let present = self.icons.get().is_some_and(|icons| {
                // SAFETY: the window and the icons are alive.
                unsafe {
                    self.taskbar
                        .ThumbBarUpdateButtons(self.window, &self.buttons(icons))
                }
                .is_ok()
            });
            if !present {
                self.added.set(false);
                log::warn!("couldn't add the thumbnail buttons: {error}");
            }
        }

        fn buttons(&self, [previous, play, pause, next]: [HICON; 4]) -> [THUMBBUTTON; 3] {
            let playing = self.playing.get();
            let toggle = if playing {
                (pause, "Pause")
            } else {
                (play, "Play")
            };
            let looks = [(previous, "Previous"), toggle, (next, "Next")];
            std::array::from_fn(|index| {
                let (icon, tip) = looks[index];
                let mut button = THUMBBUTTON {
                    dwMask: THB_ICON | THB_TOOLTIP | THB_FLAGS,
                    iId: index as u32,
                    hIcon: icon,
                    dwFlags: THBF_ENABLED,
                    ..Default::default()
                };
                for (slot, unit) in button.szTip.iter_mut().zip(tip.encode_utf16()) {
                    *slot = unit;
                }
                button
            })
        }

        fn make_icons(&self) -> ::windows::core::Result<[HICON; 4]> {
            // SAFETY: plain queries about a live window.
            let size = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForWindow(self.window)) };
            let size = u32::try_from(size).unwrap_or(0).max(16);
            // The buttons sit on the taskbar's colour, which can be light.
            let colour = if light_taskbar() {
                [0; 3]
            } else {
                [u8::MAX; 3]
            };
            let mut icons = Vec::with_capacity(Glyph::ALL.len());
            for glyph in Glyph::ALL {
                match icon(&glyph.render(size, colour), size) {
                    Ok(icon) => icons.push(icon),
                    Err(error) => {
                        for icon in icons {
                            // SAFETY: made above and on no button yet.
                            unsafe { _ = DestroyIcon(icon) };
                        }
                        return Err(error);
                    }
                }
            }
            Ok(icons.try_into().expect("one icon per glyph"))
        }
    }

    /// Receives the clicks, which arrive as commands to the window, and
    /// adds the buttons again whenever Explorer makes a new taskbar button:
    /// when the window comes back from the tray and when Explorer restarts.
    unsafe extern "system" fn subclass(
        window: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _: usize,
        state: usize,
    ) -> LRESULT {
        // SAFETY: `state` is the one `ThumbBar` keeps until it removes this
        // subclass.
        let state = unsafe { &*(state as *const State) };
        if message == WM_COMMAND && (wparam.0 >> 16) & 0xFFFF == THBN_CLICKED as usize {
            if let Some(&command) = COMMANDS.get(wparam.0 & 0xFFFF) {
                // The receiver is gone only while the app shuts down.
                _ = state.updates.send(Update::Media(command));
                return LRESULT(0);
            }
        } else if message == state.button_created {
            state.on_button_created();
        } else if message == WM_NCDESTROY {
            // SAFETY: the window is still valid while it handles this.
            unsafe { _ = RemoveWindowSubclass(window, Some(subclass), SUBCLASS_ID) };
        }
        // SAFETY: passes the message on as received.
        unsafe { DefSubclassProc(window, message, wparam, lparam) }
    }

    /// Whether the taskbar uses the light theme, where white would vanish.
    fn light_taskbar() -> bool {
        let mut light = 0u32;
        let mut size = size_of::<u32>() as u32;
        // SAFETY: `light` and `size` describe a buffer the size of the value.
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
                w!("SystemUsesLightTheme"),
                RRF_RT_REG_DWORD,
                None,
                Some((&raw mut light).cast()),
                Some(&mut size),
            )
        };
        status == ERROR_SUCCESS && light != 0
    }

    /// An icon from RGBA pixels, which Windows takes as BGRA.
    fn icon(rgba: &[u8], size: u32) -> ::windows::core::Result<HICON> {
        let bgra: Vec<u8> = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|&[red, green, blue, alpha]| [blue, green, red, alpha])
            .collect();
        // Alpha decides what shows, so the mask hides nothing; its rows are
        // padded to 16 bits.
        let mask = vec![0u8; size.div_ceil(16) as usize * 2 * size as usize];
        let size = size as i32;
        // SAFETY: both buffers hold `size` rows of `size` pixels.
        unsafe { CreateIcon(None, size, size, 1, 32, mask.as_ptr(), bgra.as_ptr()) }
    }

    fn destroy(icons: [HICON; 4]) {
        for icon in icons {
            // SAFETY: made by `icon` and no longer on any button.
            unsafe { _ = DestroyIcon(icon) };
        }
    }
}

/// Stands in where there are no thumbnail buttons; never constructed.
#[cfg(not(windows))]
pub struct ThumbBar(std::convert::Infallible);

#[cfg(not(windows))]
impl ThumbBar {
    pub fn set_playing(&self, _: bool) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha(pixels: &[u8], size: u32, x: u32, y: u32) -> u8 {
        pixels[((y * size + x) * 4 + 3) as usize]
    }

    #[test]
    fn glyphs_fill_their_shapes_and_leave_the_rest_clear() {
        const SIZE: u32 = 16;
        for glyph in Glyph::ALL {
            let pixels = glyph.render(SIZE, [255; 3]);
            assert_eq!(pixels.len(), (SIZE * SIZE * 4) as usize);
            for (x, y) in [(0, 0), (SIZE - 1, 0), (0, SIZE - 1), (SIZE - 1, SIZE - 1)] {
                assert_eq!(alpha(&pixels, SIZE, x, y), 0, "{glyph:?} corner {x},{y}");
            }
            let solid = pixels
                .chunks_exact(4)
                .filter(|pixel| pixel[3] == 255)
                .count();
            assert!(solid > 20, "{glyph:?} has {solid} solid pixels");
        }
        // Grid point 12 falls on pixel 8 when 4..20 spans 16 pixels.
        let play = Glyph::Play.render(SIZE, [255; 3]);
        assert_eq!(alpha(&play, SIZE, 8, 8), 255);
        // Pause has a gap between its bars; play does not.
        let pause = Glyph::Pause.render(SIZE, [255; 3]);
        assert_eq!(alpha(&pause, SIZE, 8, 8), 0);
    }

    #[test]
    fn button_ids_follow_the_player_bar_order() {
        assert_eq!(
            COMMANDS,
            [
                MediaCommand::Previous,
                MediaCommand::Toggle,
                MediaCommand::Next
            ]
        );
    }
}
