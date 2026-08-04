//! View construction: the top bar, status bar, the three columns and their
//! canvases, the jump dialog and the shared chrome helpers. These are all
//! `impl ParallHexApp` methods (or free helpers they call), split out of
//! `app.rs` so the state/handler module stays navigable.

use gpui::{
    App, AsyncApp, BorderStyle, Bounds, ClickEvent, Context, Corner, CursorStyle, Edges, Entity,
    IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    ScrollWheelEvent, WeakEntity, Window, anchored, canvas, div, point, prelude::*, px, quad, rgb,
    rgba, size, transparent_black,
};

use crate::core::color::Colormap;
use crate::core::{geom, thumb};
use crate::gui::paint;

use super::*;

/// Size of the horizontal whole-file preview strip in the top bar.
const STRIP_W: f32 = 320.0;
const STRIP_H: f32 = 36.0;

/// Wire one handler to both `on_mouse_up` and `on_mouse_up_out`.
///
/// Every drag in this UI has to end on release *anywhere*, not just inside the
/// element that started it — otherwise letting go past the edge of a pane
/// leaves the drag latched on. Both halves therefore always run the same
/// handler, and attaching them separately at each call site only created pairs
/// that could drift apart.
trait MouseUpAnywhere: InteractiveElement + Sized {
    fn on_mouse_up_anywhere(
        self,
        cx: &mut Context<ParallHexApp>,
        button: MouseButton,
        handler: impl Fn(&mut ParallHexApp, &MouseUpEvent, &mut Context<ParallHexApp>) + Clone + 'static,
    ) -> Self {
        let outside = handler.clone();
        self.on_mouse_up(
            button,
            cx.listener(move |this, ev: &MouseUpEvent, _, cx| handler(this, ev, cx)),
        )
        .on_mouse_up_out(
            button,
            cx.listener(move |this, ev: &MouseUpEvent, _, cx| outside(this, ev, cx)),
        )
    }
}

impl<E: InteractiveElement> MouseUpAnywhere for E {}

/// Button labels naming the `secondary` accelerator, which `main.rs` binds to
/// Cmd on macOS and Ctrl elsewhere — the label has to follow the binding.
const JUMP_BUTTON_LABEL: &str = if cfg!(target_os = "macos") {
    "Jump to offset… (Cmd+G)"
} else {
    "Jump to offset… (Ctrl+G)"
};

impl ParallHexApp {
    /// The hex column's scrollbar: the whole file as a track, the visible rows
    /// as a thumb. The hex column is the scroll reference, so this drives the
    /// shared anchor and the other panels follow.
    fn hex_scrollbar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let anchor = self.scroll_offset;
        let visible = self.hex_view.len();
        let len = self.file_size;
        let last = geom::max_anchor(len, self.hex_bpr.max(8));
        div()
            .w(px(paint::SCROLLBAR_W))
            .h_full()
            .flex_shrink_0()
            .cursor(CursorStyle::Arrow)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                    this.scrollbar_dragging = true;
                    this.on_scrollbar_drag(ev.position);
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _, cx| {
                if this.scrollbar_dragging && ev.dragging() {
                    this.on_scrollbar_drag(ev.position);
                    cx.notify();
                }
            }))
            .on_mouse_up_anywhere(cx, MouseButton::Left, |this, _, cx| {
                this.scrollbar_dragging = false;
                cx.notify();
            })
            .child(pane_canvas(
                &entity,
                |this, bounds, _cx| this.scrollbar_bounds = bounds,
                move |bounds, window, _cx| {
                    paint::paint_scrollbar(window, bounds, anchor, last, visible, len);
                },
            ))
    }

    /// The central area: the no-file placeholder, or the three columns with
    /// their drag dividers. It must be a dedicated child of the column root —
    /// mutating the root itself with `.when(...).flex_row()` would flatten the
    /// columns next to the top and status bars.
    pub(crate) fn central_area(
        &mut self,
        cx: &mut Context<Self>,
        no_file: bool,
    ) -> impl IntoElement {
        div()
            .flex_1()
            .min_h_0()
            .when(no_file, |d| {
                d.flex().items_center().justify_center().child(
                    div()
                        .text_color(rgb(0x565f89))
                        .child("No file loaded.\n\nOpen a binary file to explore its bytes."),
                )
            })
            .when(!no_file, |d| {
                d.flex()
                    .flex_row()
                    .min_h_0()
                    .child(self.overview_column(cx))
                    .child(Self::column_divider(cx, DividerKind::OverviewZoom))
                    .child(self.pixels_column(cx))
                    .child(Self::column_divider(cx, DividerKind::ZoomHex))
                    .child(self.hex_column(cx))
            })
    }

    /// Bottom status bar: the live offset/byte/entropy readout, selection
    /// and scroll summaries, the zoom state, the jump preview while typing
    /// in the jump dialog, and transient messages.
    pub(crate) fn status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let readout = self.byte_readout();
        let jump_preview = self.jump_preview(cx);
        let selection = self.selection_summary();
        let scroll = self.scroll_summary();

        div()
            .w_full()
            .flex()
            .items_center()
            .gap_4()
            .px_3()
            .py_1()
            .bg(rgb(0x1a1b26))
            .border_t_1()
            .border_color(rgb(0x2a2f45))
            .text_size(px(11.))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .gap_4()
                    .child(
                        div()
                            .text_color(rgb(0x9ece6a))
                            .child(readout.unwrap_or_else(|| "no file loaded".to_owned())),
                    )
                    .when_some(selection, |d, s| d.child(div().child(s)))
                    .when_some(jump_preview, |d, (text, is_err)| {
                        d.child(
                            div()
                                .text_color(if is_err { rgb(0xe0af68) } else { rgb(0x9ece6a) })
                                .child(text),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_4()
                    .child(
                        div()
                            .text_color(rgb(0x565f89))
                            .child(format!("px {}", self.pixel_zoom.round() as u32)),
                    )
                    .when_some(scroll, |d, s| {
                        d.child(div().text_color(rgb(0x565f89)).child(s))
                    })
                    .when_some(self.message.clone(), |d, m| {
                        d.child(div().text_color(rgb(0xe0af68)).child(m))
                    })
                    .child(
                        div()
                            .text_color(rgb(0x565f89))
                            .child(format!("build {:.2} ms", self.last_render_ms)),
                    )
                    .child(
                        div()
                            .text_color(rgb(0x565f89))
                            .child(format!("frame {:.2} ms", self.last_frame_ms)),
                    ),
            )
    }

    /// Selection range summary for the status bar.
    fn selection_summary(&self) -> Option<String> {
        let range = self.selection_range.as_ref()?;
        let len = self.file_size;
        let start = range.start.min(len);
        let end = range.end.min(len);
        (start < end).then(|| format!("sel 0x{start:X}–0x{end:X} ({} B)", end - start))
    }

    /// Visible row range + file percentage for the status bar.
    fn scroll_summary(&self) -> Option<String> {
        if self.file_size == 0 {
            return None;
        }
        let bpr = self.hex_bpr.max(8);
        let total_rows = self.file_size.div_ceil(bpr);
        let first = self.hex_view.start / bpr;
        let vis = geom::visible_rows(self.view_height, paint::BLOCK_H);
        let last = (first + vis).min(total_rows);
        let pct = (self.view_frac * 100.0).round() as u32;
        Some(format!(
            "{bpr} B/row · rows {first}–{last} / {total_rows} · {pct}%"
        ))
    }

    // ----- top info bar -----

    /// The top info bar: app title, file name/size and the action controls
    /// (open, entropy window, reset/jump) plus the horizontal
    /// whole-file preview strip. Live readouts live in the bottom status bar.
    #[allow(clippy::too_many_lines)] // single-purpose element builder
    pub(crate) fn top_bar(
        &mut self,
        cx: &mut Context<Self>,
        client_side: bool,
    ) -> impl IntoElement {
        let file_size = self.file_size;
        let file_name = self.file_name_str();
        let has_file = self.mmap.is_some();

        let row2 = div()
            .flex()
            .items_center()
            .gap_2()
            .child(button(cx, "Open File…", |this, window, cx| {
                this.on_open_file(&OpenFile, window, cx);
            }))
            .child(self.slider(cx, SliderKind::EntropyWindow))
            .child(div().child("Entropy win"))
            .child(button(cx, "Reset view", |this, window, cx| {
                this.on_reset_view(&ResetView, window, cx);
            }))
            .child(button(cx, JUMP_BUTTON_LABEL, |this, window, cx| {
                this.on_jump_to_offset(&JumpToOffset, window, cx);
            }))
            .child(button(cx, "Reset all settings", |this, window, cx| {
                this.on_reset_settings(&ResetSettings, window, cx);
            }));

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_1()
            .px_3()
            .py_2()
            .bg(rgb(0x1a1b26))
            .border_b_1()
            .border_color(rgb(0x2a2f45))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    // The title / file-name area doubles as the titlebar drag
                    // handle. The strip and the window buttons are siblings, not
                    // children, so their own clicks never start a window move.
                    .child(
                        div()
                            .id("titlebar-drag")
                            .flex()
                            .flex_1()
                            // Let the title text be the thing that gives way in
                            // a narrow window: without min_w_0 this grow-1 area
                            // refuses to shrink below its text and squeezes the
                            // fixed-size preview strip out of the row instead.
                            .min_w_0()
                            .overflow_hidden()
                            .items_center()
                            .gap_3()
                            .when(client_side, |d| {
                                d.on_mouse_down(
                                    MouseButton::Left,
                                    |_: &MouseDownEvent, window: &mut Window, _: &mut App| {
                                        window.start_window_move();
                                    },
                                )
                                .on_click(
                                    |ev: &ClickEvent, window: &mut Window, _: &mut App| {
                                        if ev.is_right_click() {
                                            window.show_window_menu(ev.position());
                                        } else if ev.click_count() >= 2 {
                                            window.zoom_window();
                                        }
                                    },
                                )
                            })
                            .child(
                                div()
                                    .text_xl()
                                    .text_color(rgb(0x7aa2f7))
                                    .child("Parall-Hex"),
                            )
                            .child(div().child(format!(
                                "{file_name} · {file_size} bytes ({})",
                                color::human_size(file_size)
                            ))),
                    )
                    .when(has_file, |d| d.child(self.strip(cx)))
                    .when(client_side, |d| d.child(window_buttons(cx))),
            )
            .child(row2)
    }

    /// Hovered / selected byte readout shown in the bottom status bar.
    fn byte_readout(&self) -> Option<String> {
        let off = self
            .overview_hover_offset
            .or(self.hovered_offset)
            .or(self.selected_offset)?;
        let d = self.data()?;
        if off >= d.len() {
            return None;
        }
        let b = d[off];
        let h = geom::entropy_at(&self.entropies, self.entropy_window, off);
        Some(format!(
            "0x{off:08X} · 0x{b:02X} '{}' · H={h:.3}",
            color::printable(b)
        ))
    }

    /// Live jump-dialog preview while typing.
    fn jump_preview(&self, cx: &mut Context<Self>) -> Option<(String, bool)> {
        if !self.show_jump_dialog {
            return None;
        }
        let content = self.jump_field.read(cx).content().to_owned();
        match geom::parse_offset(&content) {
            Some(o) if o < self.file_size => {
                let d = self.data();
                let b = d.map_or(0, |d| d[o]);
                let h = geom::entropy_at(&self.entropies, self.entropy_window, o);
                Some((
                    format!(
                        "Jump: 0x{o:08X}  Byte: 0x{b:02X} '{}'  H={h:.3}",
                        color::printable(b)
                    ),
                    false,
                ))
            }
            Some(o) => Some((
                format!(
                    "Out of range: 0x{o:X} (file is 0x{:X} bytes).",
                    self.file_size
                ),
                true,
            )),
            None => Some(("Jump: invalid offset".to_owned(), true)),
        }
    }

    /// The horizontal whole-file preview strip (greyscale / entropy).
    fn strip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let strip_image = self.strip_image.clone();
        let file_size = self.file_size;
        // The strip is a second whole-file map, so it marks the same range.
        let mark = self.zoom_view.clone();
        div()
            // `on_hover` below needs a stateful element, hence the id.
            .id("preview-strip")
            .w(px(STRIP_W))
            .h(px(STRIP_H))
            // Fixed size: never let a long file name shrink the preview away.
            .flex_shrink_0()
            .rounded_md()
            .overflow_hidden()
            .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _, cx| {
                this.on_strip_move(ev.position, ev.dragging());
                cx.notify();
            }))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                this.on_overview_hover(*hovered, cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                    this.on_strip_mouse_down(ev.position);
                    cx.notify();
                }),
            )
            .on_mouse_up_anywhere(cx, MouseButton::Left, |this, _, cx| {
                this.on_strip_mouse_up();
                cx.notify();
            })
            .child(pane_canvas(
                &entity,
                |this, bounds, _cx| this.strip_bounds = bounds,
                move |bounds, window, _cx| {
                    if let Some(img) = &strip_image {
                        paint::paint_strip(
                            window,
                            bounds,
                            img,
                            file_size,
                            (!mark.is_empty()).then_some(&mark),
                        );
                    } else {
                        window.paint_quad(quad_dark(bounds));
                    }
                },
            ))
    }

    // ----- column builders -----

    /// A draggable 6px divider between two columns. The pointer-down starts
    /// the resize; the root's mouse-move handler continues it while the
    /// pointer is anywhere in the window; pointer-up (on or off the strip)
    /// ends it.
    fn column_divider(cx: &mut Context<Self>, kind: DividerKind) -> impl IntoElement {
        div()
            .id(("divider", kind as usize))
            .w(px(6.))
            .h_full()
            .flex_shrink_0()
            .cursor(CursorStyle::ResizeLeftRight)
            .bg(rgb(0x1a1b26))
            .hover(|s| s.bg(rgb(0x3b4261)))
            .active(|s| s.bg(rgb(0x3b4261)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                    this.on_divider_mouse_down(kind, ev.position);
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _, cx| {
                if this.on_divider_mouse_move(ev.position) {
                    cx.notify();
                }
            }))
            .on_mouse_up_anywhere(cx, MouseButton::Left, |this, _, cx| {
                this.on_divider_mouse_up();
                cx.notify();
            })
    }

    /// Left column: a vertical whole-file overview.
    #[allow(clippy::too_many_lines)] // single-purpose element builder
    fn overview_column(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let file_size = self.file_size;
        // The overview marks the range the zoom column is showing.
        let mark = self.zoom_view.clone();

        let overview_width = self.overview_width;
        let header = column_header(
            "Overview",
            (file_size > 0).then(|| geom::range_label(0, file_size)),
            self.colormap_picker(cx, Panel::Overview),
        );

        div()
            .w(px(overview_width))
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(0x12121c))
            .border_r_1()
            .border_color(rgb(0x232740))
            .child(header)
            .child(
                div()
                    // `on_hover` below needs a stateful element, hence the id.
                    .id("overview-canvas")
                    .flex_1()
                    .min_h_0()
                    .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _, cx| {
                        this.on_overview_move(ev.position, ev.dragging());
                        cx.notify();
                    }))
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        this.on_overview_hover(*hovered, cx);
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                            this.on_overview_mouse_down(ev.position);
                            cx.notify();
                        }),
                    )
                    .on_mouse_up_anywhere(cx, MouseButton::Left, |this, _, cx| {
                        this.on_overview_mouse_up();
                        cx.notify();
                    })
                    .child({
                        let paint_entity = entity.clone();
                        pane_canvas(
                            &entity,
                            |this, bounds, cx| {
                                // Regenerate the thumbnail when its inputs change,
                                // on the background executor so downsampling a huge
                                // file never stalls the frame. While a build is in
                                // flight the key mismatch is left alone; if the
                                // inputs changed during the build, the landing's
                                // stale key triggers another build.
                                let w = (bounds.size.width.to_f64() as usize).clamp(64, 512);
                                let h = (bounds.size.height.to_f64() as usize).clamp(32, 1024);
                                let key = OverviewKey {
                                    w,
                                    h,
                                    colormap: this.overview_colormap,
                                    entropy_window: this.entropy_window,
                                };
                                if this.overview_key != Some(key)
                                    && !this.overview_computing
                                    && this.resizing_divider.is_none()
                                {
                                    // Snapshot the inputs for the task.
                                    this.overview_computing = true;
                                    let data = this.mmap.clone();
                                    let entropies = this.entropies.clone();
                                    cx.spawn(
                                        async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                                            let rgba = cx
                                                .background_executor()
                                                .spawn(async move {
                                                    data.as_deref().map(|d| {
                                                        thumb::build_overview_rgba(
                                                            &geom::ByteSource {
                                                                data: d,
                                                                entropies: &entropies,
                                                                entropy_window: key.entropy_window,
                                                                colormap: key.colormap,
                                                            },
                                                            key.w,
                                                            key.h,
                                                        )
                                                    })
                                                })
                                                .await;
                                            this.update(cx, |this, cx| {
                                                if let Some(rgba) = rgba {
                                                    this.overview_image =
                                                        Some(paint::render_image_from_rgba(
                                                            key.w, key.h, rgba,
                                                        ));
                                                    this.overview_cells = Some((key.w, key.h));
                                                } else {
                                                    this.overview_image = None;
                                                    this.overview_cells = None;
                                                }
                                                this.overview_key = Some(key);
                                                this.overview_computing = false;
                                                cx.notify();
                                            })
                                            .ok();
                                        },
                                    )
                                    .detach();
                                }
                                this.overview_bounds = bounds;
                            },
                            move |bounds, window, cx| {
                                let image = paint_entity.read(cx).overview_image.clone();
                                match image {
                                    Some(img) => paint::paint_overview(
                                        window,
                                        bounds,
                                        &img,
                                        file_size,
                                        (!mark.is_empty()).then_some(&mark),
                                    ),
                                    None => {
                                        window.paint_quad(quad_dark(bounds));
                                    }
                                }
                            },
                        )
                    })
                    .size_full(),
            )
    }

    /// Middle column: per-byte colormap + entropy bands.
    fn pixels_column(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let data = self.mmap.clone();
        let bpr = self.zoom_bpr.max(1);
        let len = self.file_size;
        let first_row_start = self.zoom_view.start;
        let block = self.zoom_row_h();
        // The zoom column marks the range the hex column is showing.
        let mark = self.hex_view.clone();
        let hovered = self.hovered_offset;
        let sel = self.selection_range.clone();

        let range = (len > 0).then(|| {
            let rows = geom::visible_rows(self.view_height, self.zoom_row_h());
            geom::range_label(first_row_start, (first_row_start + rows * bpr).min(len))
        });

        div()
            .w(px(self.zoom_width))
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(0x10101a))
            .border_r_1()
            .border_color(rgb(0x232740))
            .child(self.zoom_header(cx, range))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                            this.on_pixels_mouse_down(ev);
                            cx.notify();
                        }),
                    )
                    .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _, cx| {
                        this.on_pixels_mouse_move(ev);
                        cx.notify();
                    }))
                    .on_mouse_up_anywhere(cx, MouseButton::Left, |this, _, cx| {
                        this.on_pixels_mouse_up();
                        cx.notify();
                    })
                    .on_scroll_wheel(cx.listener(move |this, ev: &ScrollWheelEvent, _, cx| {
                        this.on_pixels_scroll(ev, cx);
                        cx.notify();
                    }))
                    .child({
                        let paint_entity = entity.clone();
                        pane_canvas(
                            &entity,
                            super::ParallHexApp::measure_zoom,
                            move |bounds, window, cx| {
                                // The texture is rebuilt in `measure_zoom`
                                // (prepaint) when its inputs change; the paint
                                // closure just blits it and draws the overlays.
                                if data.is_some() {
                                    let image = paint_entity.read(cx).zoom_image.clone();
                                    paint::paint_zoom(
                                        window,
                                        bounds,
                                        image.as_ref(),
                                        bpr,
                                        first_row_start,
                                        block,
                                        hovered,
                                        sel.as_ref(),
                                        (!mark.is_empty()).then_some(&mark),
                                        len,
                                    );
                                }
                            },
                        )
                    })
                    .size_full(),
            )
    }

    /// The zoom column's header: title, the zoom readout / slider / reset, the
    /// visible byte range and this panel's colormap picker.
    fn zoom_header(&mut self, cx: &mut Context<Self>, range: Option<String>) -> impl IntoElement {
        let zoom_controls = div()
            .flex()
            .items_center()
            .gap_1()
            .child(
                div()
                    .text_color(rgb(0x565f89))
                    .child(format!("{} px", self.pixel_zoom.round() as u32)),
            )
            .child(self.slider(cx, SliderKind::PixelZoom))
            .child(button(cx, "Reset", move |this, _window, cx| {
                this.pixel_zoom = geom::PIXEL_ZOOM_DEFAULT;
                cx.notify();
            }))
            .child(self.colormap_picker(cx, Panel::Zoom));
        column_header("Zoom", range, zoom_controls)
    }
    /// A panel's colormap control: a compact `Map: … ▾` toggle that opens a
    /// floating option menu (`colormap_menu`). The pills cannot live inline in
    /// the header row — the Overview/Zoom columns are narrow fixed widths, so
    /// an expanded row overflows the column and gets painted over by the next
    /// one. Instead a transparent prepaint canvas inside the picker records
    /// the toggle's window-space bounds, and the menu is rendered at the root
    /// level (like the jump dialog), anchored just below the toggle.
    fn colormap_picker(&mut self, cx: &mut Context<Self>, panel: Panel) -> impl IntoElement {
        let open = self.open_colormap_menu == Some(panel);
        let current = self.colormap(panel);
        let entity = cx.entity();

        let toggle = div()
            .id(("colormap-toggle", panel as usize))
            .px_2()
            .py_1()
            .rounded_md()
            .flex()
            .items_center()
            .gap_1()
            .bg(if open { rgb(0x3b4261) } else { rgb(0x24283b) })
            .text_color(rgb(0xc0caf5))
            .cursor_pointer()
            .active(|s| s.opacity(0.7))
            .hover(|s| s.bg(rgb(0x3b4261)))
            .on_click(
                cx.listener(move |this, _: &ClickEvent, _: &mut Window, cx| {
                    this.open_colormap_menu = if this.open_colormap_menu == Some(panel) {
                        None
                    } else {
                        Some(panel)
                    };
                    cx.notify();
                }),
            )
            .child(swatch(current))
            .child(div().child(format!("Color: {}", current.label())))
            .child(div().child("▾"));

        div()
            .relative()
            .flex()
            .items_center()
            .gap_1()
            // Any press inside the picker (toggle or menu) counts as "inside",
            // so the root's outside-click handler leaves it alone. The menu is
            // a root-level child, so this handler must live on the picker *and*
            // the menu container.
            .on_any_mouse_down(
                cx.listener(move |this, _: &MouseDownEvent, _: &mut Window, _cx| {
                    this.colormap_click_inside = true;
                }),
            )
            .child(
                canvas(
                    move |bounds, _window, cx| {
                        entity.update(cx, |this, _| {
                            this.set_colormap_anchor(panel, bounds);
                        });
                    },
                    |_bounds, (), _window, _cx| {},
                )
                .absolute()
                .inset_0(),
            )
            .child(toggle)
    }

    /// The floating colormap option menu. Rendered from `render` at the root
    /// level so it paints above the columns and can overflow them; positioned
    /// with gpui's `anchored` element at the toggle's recorded bounds.
    pub(crate) fn colormap_menu(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let panel = self
            .open_colormap_menu
            .expect("menu renders only when open");
        let current = self.colormap(panel);
        let anchor = self.colormap_anchor(panel);

        let menu = div()
            .bg(rgb(0x1f2335))
            .border_1()
            .border_color(rgb(0x414868))
            .rounded_lg()
            .p_1()
            .flex()
            .flex_col()
            .gap_0p5()
            .on_any_mouse_down(
                cx.listener(move |this, _: &MouseDownEvent, _: &mut Window, _cx| {
                    this.colormap_click_inside = true;
                }),
            )
            .children(Colormap::ALL.into_iter().enumerate().map(|(idx, cm)| {
                let mut pill = div()
                    .id(("colormap", panel as usize * Colormap::ALL.len() + idx))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_size(px(11.))
                    .cursor_pointer()
                    .on_click(
                        cx.listener(move |this, _: &ClickEvent, _: &mut Window, cx| {
                            this.set_colormap(panel, cm);
                            this.open_colormap_menu = None;
                            cx.notify();
                        }),
                    );
                pill = if cm == current {
                    pill.bg(rgb(0x7aa2f7)).text_color(rgb(0x0f1017))
                } else {
                    pill.bg(rgb(0x24283b))
                        .text_color(rgb(0xc0caf5))
                        .hover(|s| s.bg(rgb(0x3b4261)))
                };
                pill.child(cm.label())
            }));

        anchored()
            .anchor(Corner::TopLeft)
            .position(point(anchor.left(), anchor.bottom()))
            .offset(point(px(0.), px(4.)))
            .snap_to_window_with_margin(Edges::all(px(8.)))
            .child(menu)
    }

    /// Right column: colormap-backed hex + ASCII cells. Its row length comes
    /// from its own width, and it is the scroll reference: its visible height is
    /// what clamps the shared anchor, sizes a page and centres a jump target.
    #[allow(clippy::too_many_lines)] // single-purpose element builder
    fn hex_column(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let data = self.mmap.clone();
        let bpr = self.hex_bpr.max(8);
        let len = self.file_size;
        // The anchor is the byte in the middle of the viewport, so panels align
        // on their centre line; the prepaint recorded the row.
        let first_row_start = self.hex_view.start;
        let hovered = self.hovered_offset;
        let sel = self.selection_range.clone();
        let font = paint::mono_font(&self.mono_family);
        let char_w = self.hex_char_w;
        let hex_entropies = self.entropies.clone();
        let entropy_window = self.entropy_window;
        let hex_colormap = self.hex_colormap;

        let range = (len > 0).then(|| {
            let rows = geom::visible_rows(self.view_height, paint::BLOCK_H);
            geom::range_label(first_row_start, (first_row_start + rows * bpr).min(len))
        });

        // The hex text size is fixed, so this header carries no zoom controls.
        let header_extra = self.colormap_picker(cx, Panel::Hex);

        div()
            .flex_1()
            .min_w(px(200.))
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(0x0c0d14))
            .child(column_header("Hex", range, header_extra))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            // `on_any_mouse_down` covers left, middle and right (the
                            // handler dispatches on the button itself); adding a
                            // separate left binding would run it twice per click.
                            .on_any_mouse_down(cx.listener(
                                move |this, ev: &MouseDownEvent, _window, cx| {
                                    this.on_hex_mouse_down(ev);
                                    if let Some(copy) = this.pending_copy.take() {
                                        cx.write_to_clipboard(ClipboardItem::new_string(copy));
                                    }
                                    cx.notify();
                                },
                            ))
                            .on_mouse_move(cx.listener(
                                move |this, ev: &MouseMoveEvent, _window, cx| {
                                    this.on_hex_mouse_move(ev);
                                    cx.notify();
                                },
                            ))
                            .on_mouse_up_anywhere(cx, MouseButton::Left, |this, ev, cx| {
                                this.on_hex_mouse_up(ev);
                                cx.notify();
                            })
                            .on_mouse_up_anywhere(cx, MouseButton::Middle, |this, ev, cx| {
                                this.on_hex_mouse_up(ev);
                                cx.notify();
                            })
                            .on_scroll_wheel(cx.listener(
                                move |this, ev: &ScrollWheelEvent, _, cx| {
                                    this.on_hex_scroll(ev, cx);
                                    cx.notify();
                                },
                            ))
                            .child(pane_canvas(
                                &entity,
                                |this, bounds, cx| {
                                    // `hex_char_w` is measured in `render` only
                                    // when the window scale changes.
                                    this.hex_bounds = bounds;
                                    this.view_height = bounds.size.height.to_f64() as f32;
                                    // Content fits the panel: as many whole
                                    // 8-byte groups as the width allows.
                                    let new_bpr = geom::hex_bytes_per_row(
                                        bounds.size.width.to_f64() as f32,
                                        this.hex_char_w,
                                        paint::ADDR_X,
                                    );
                                    let bpr_changed = new_bpr != this.hex_bpr;
                                    this.hex_bpr = new_bpr;
                                    let bpr = this.hex_bpr.max(8);
                                    let before = this.scroll_offset;
                                    // The anchor *is* the centre, so a jump is
                                    // just an assignment.
                                    if let Some(off) = this.scroll_to_offset.take() {
                                        this.scroll_offset = off;
                                    }
                                    this.clamp_anchor();
                                    let rows = geom::visible_rows(this.view_height, paint::BLOCK_H);
                                    let first =
                                        geom::first_row_centred(this.scroll_offset, bpr, rows);
                                    this.hex_view = first..(first + rows * bpr).min(this.file_size);
                                    if this.file_size > 0 {
                                        this.view_frac = (this.scroll_offset as f32
                                            / this.file_size as f32)
                                            .clamp(0.0, 1.0);
                                    }
                                    if this.scroll_offset != before || bpr_changed {
                                        cx.notify();
                                    }
                                },
                                move |bounds, window, cx| {
                                    if let Some(d) = &data {
                                        paint::paint_hex(
                                            window,
                                            cx,
                                            bounds,
                                            &geom::ByteSource {
                                                data: d,
                                                entropies: &hex_entropies,
                                                entropy_window,
                                                colormap: hex_colormap,
                                            },
                                            &font,
                                            char_w,
                                            bpr,
                                            first_row_start,
                                            hovered,
                                            sel.as_ref(),
                                        );
                                    } else {
                                        window.paint_quad(quad_dark(bounds));
                                    }
                                },
                            ))
                            .size_full(),
                    )
                    .child(self.hex_scrollbar(cx)),
            )
    }

    /// The jump dialog as an overlay covering the window.
    pub(crate) fn jump_dialog(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let jump_field = self.jump_field.clone();
        let file_size = self.file_size;
        let error = self.jump_error.clone();
        div()
            .absolute()
            .inset_0()
            .bg(rgba(0x000000a0))
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _: &mut Window, cx| {
                    // Clicking the backdrop dismisses the dialog.
                    this.show_jump_dialog = false;
                    this.jump_error = None;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .w(px(380.))
                    .bg(rgb(0x1f2335))
                    .border_1()
                    .border_color(rgb(0x414868))
                    .rounded_lg()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(
                            |_: &mut ParallHexApp, _: &MouseDownEvent, _: &mut Window, cx| {
                                // Swallow clicks on the card so the backdrop stays.
                                cx.stop_propagation();
                            },
                        ),
                    )
                    .child(div().text_color(rgb(0x7aa2f7)).child("Jump to Offset"))
                    .child(div().child(format!(
                        "Offset (hex, 0x… up to 0x{:X}):",
                        file_size.saturating_sub(1)
                    )))
                    .child(jump_field.clone())
                    .when_some(error, |d, e| {
                        d.child(div().text_color(rgb(0xe0af68)).child(e))
                    })
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .child(button(cx, "Cancel", move |this, _window, cx| {
                                this.show_jump_dialog = false;
                                this.jump_error = None;
                                cx.notify();
                            }))
                            .child(button(cx, "Jump", move |this, _window, cx| {
                                let text = this.jump_field.read(cx).content().to_owned();
                                this.jump_submit(&text, cx);
                            })),
                    ),
            )
    }

    /// A compact slider used by the column headers and the entropy-window
    /// control. The track/thumb are painted on a canvas that records its own
    /// bounds (via `entity.update`) for the pointer handlers.
    fn slider(&mut self, cx: &mut Context<Self>, kind: SliderKind) -> impl IntoElement {
        let entity = cx.entity();
        let value = match kind {
            SliderKind::PixelZoom => self.pixel_zoom,
            SliderKind::EntropyWindow => self.entropy_window as f32,
        };
        let (min, max) = kind.range();

        div()
            .w(px(90.))
            .h(px(16.))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                    this.dragging_slider = Some(kind);
                    if let Some(v) = slider_value_at(kind, ev.position, this.slider_bounds(kind)) {
                        this.set_slider_value(kind, v, cx);
                    }
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _, cx| {
                if this.dragging_slider == Some(kind)
                    && ev.dragging()
                    && let Some(v) = slider_value_at(kind, ev.position, this.slider_bounds(kind))
                {
                    this.set_slider_value(kind, v, cx);
                }
                cx.notify();
            }))
            .on_mouse_up_anywhere(cx, MouseButton::Left, move |this, _, _cx| {
                if this.dragging_slider == Some(kind) {
                    this.dragging_slider = None;
                }
            })
            .child(canvas(
                move |bounds, _window, cx| {
                    entity.update(cx, |this, _| {
                        this.set_slider_bounds(kind, bounds);
                    });
                },
                move |bounds, (), window, _cx| {
                    let w = bounds.size.width.to_f64() as f32;
                    let h = bounds.size.height.to_f64() as f32;
                    let t = slider_t(value, min, max);
                    let track = Bounds::new(
                        point(bounds.left() + px(2.), bounds.top() + px(h * 0.5 - 2.)),
                        size(px(w - 4.), px(4.)),
                    );
                    window.paint_quad(quad(
                        track,
                        px(2.),
                        rgb(0x2a2f45),
                        px(0.),
                        transparent_black(),
                        BorderStyle::default(),
                    ));
                    let thumb_x = slider_thumb_left(t, w);
                    let thumb = Bounds::new(
                        point(
                            bounds.left() + px(thumb_x),
                            bounds.top() + px(h * 0.5 - SLIDER_THUMB_W * 0.5),
                        ),
                        size(px(SLIDER_THUMB_W), px(SLIDER_THUMB_W)),
                    );
                    window.paint_quad(quad(
                        thumb,
                        px(6.),
                        rgb(0x7aa2f7),
                        px(0.),
                        transparent_black(),
                        BorderStyle::default(),
                    ));
                },
            ))
    }

    fn slider_bounds(&self, kind: SliderKind) -> Bounds<Pixels> {
        match kind {
            SliderKind::PixelZoom => self.pixels_slider_bounds,
            SliderKind::EntropyWindow => self.entropy_slider_bounds,
        }
    }

    fn set_slider_bounds(&mut self, kind: SliderKind, bounds: Bounds<Pixels>) {
        match kind {
            SliderKind::PixelZoom => self.pixels_slider_bounds = bounds,
            SliderKind::EntropyWindow => self.entropy_slider_bounds = bounds,
        }
    }

    fn set_slider_value(&mut self, kind: SliderKind, v: f32, cx: &mut Context<Self>) {
        match kind {
            SliderKind::PixelZoom => {
                self.pixel_zoom = v.clamp(geom::PIXEL_ZOOM_MIN, geom::PIXEL_ZOOM_MAX);
            }
            SliderKind::EntropyWindow => {
                let w =
                    (v.round() as usize).clamp(geom::ENTROPY_WINDOW_MIN, geom::ENTROPY_WINDOW_MAX);
                if w != self.entropy_window {
                    self.entropy_window = w;
                    self.recompute_entropies_async(cx, false);
                    self.overview_key = None;
                    self.strip_dirty = true;
                }
            }
        }
        cx.notify();
    }
}

// ---------------------------------------------------------------------------
// Shared chrome helpers
// ---------------------------------------------------------------------------

/// A reusable button wired up through `on_click` (so it works without the
/// element holding keyboard focus). The callback receives the view, the
/// window and a context, mirroring the `Context::listener` signature.
fn button(
    cx: &mut Context<ParallHexApp>,
    label: &'static str,
    on_click: impl Fn(&mut ParallHexApp, &mut Window, &mut Context<ParallHexApp>) + 'static,
) -> impl IntoElement {
    div()
        .id(label)
        .px_2()
        .py_1()
        .rounded_md()
        .bg(rgb(0x24283b))
        .text_color(rgb(0xc0caf5))
        .cursor_pointer()
        .active(|s| s.opacity(0.7))
        .hover(|s| s.bg(rgb(0x3b4261)))
        .on_click(cx.listener(move |this, _ev: &ClickEvent, window, cx| {
            on_click(this, window, cx);
        }))
        .child(label)
}

/// A small color swatch previewing what a colormap looks like, shown in each
/// column header's dropdown toggle.
fn swatch(cm: Colormap) -> impl IntoElement {
    let color = match cm {
        Colormap::None => rgb(0x3b4261),
        Colormap::Value => rgb(0x9aa5ce),
        Colormap::Class => paint::to_rgba(color::class_color(0x41)),
        Colormap::Entropy => paint::to_rgba(color::entropy_color(4.0)),
    };
    div().w(px(10.)).h(px(10.)).rounded_md().bg(color)
}

/// A dark background quad for empty canvas areas.
fn quad_dark(bounds: Bounds<Pixels>) -> gpui::PaintQuad {
    paint::filled_quad(bounds, rgb(0x0c0d14))
}

/// A full-size canvas whose prepaint runs `prepaint` against the app state and
/// its measured bounds, and whose paint closure is a plain free function. Every
/// pane canvas is exactly this shape; the `canvas` boilerplate (and the
/// `.size_full()` a canvas needs because it has no intrinsic size) lives here
/// rather than at each call site.
fn pane_canvas<P, F>(entity: &Entity<ParallHexApp>, prepaint: P, paint: F) -> impl IntoElement
where
    P: Fn(&mut ParallHexApp, Bounds<Pixels>, &mut Context<ParallHexApp>) + 'static,
    F: Fn(Bounds<Pixels>, &mut Window, &mut App) + 'static,
{
    let entity = entity.clone();
    canvas(
        move |bounds, _window, cx| {
            entity.update(cx, |this, cx| prepaint(this, bounds, cx));
        },
        move |bounds, (), window, cx| paint(bounds, window, cx),
    )
    .size_full()
}

/// Draw a column header: bold title, muted range label, trailing widgets.
fn column_header(
    title: &'static str,
    range: Option<String>,
    trailing: impl IntoElement,
) -> impl IntoElement {
    div()
        .w_full()
        .px_2()
        .py_1()
        .flex()
        .flex_col()
        .gap_1()
        .border_b_1()
        .border_color(rgb(0x232740))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(div().text_color(rgb(0x9d7cd8)).child(title))
                .child(trailing),
        )
        .when_some(range, |d, r| {
            d.child(div().text_color(rgb(0x565f89)).text_size(px(11.)).child(r))
        })
}
