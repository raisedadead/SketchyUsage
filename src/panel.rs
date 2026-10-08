use std::{ptr::NonNull, sync::Arc, time::Duration};

use block2::RcBlock;
use futures::{
    StreamExt,
    channel::mpsc::{UnboundedReceiver, UnboundedSender},
};
use gpui::{
    ActivationPolicy, App, AsyncApp, Bounds, Context, Div, FocusHandle, FontWeight, KeyBinding,
    QuitMode, Render, Subscription, Task, Window, WindowBackgroundAppearance, WindowBounds,
    WindowHandle, WindowKind, WindowOptions, actions, div, point, prelude::*, px, rgb, size,
};
use objc2::{rc::Retained, runtime::AnyObject};
use objc2_app_kit::{NSEvent, NSEventMask, NSScreen};
use objc2_foundation::MainThreadMarker;

use crate::{
    bar,
    format::{self, Line, ProviderView, Tone},
    geometry::{self, Rect},
    server::{self, PROVIDERS, Server},
    state::State,
};

actions!(sketchyusage, [Close]);

const WIDTH: f64 = 420.0;
const PADDING: f32 = 16.0;
const LINE: f32 = 20.0;
const SECTION_GAP: f32 = 12.0;
const BORDER: f32 = 1.0;
const FONT: &str = "Berkeley Mono";

const MANTLE: u32 = 0x181825;
const SURFACE2: u32 = 0x585b70;
const TEXT: u32 = 0xcdd6f4;
const SUBTEXT: u32 = 0xa6adc8;
const YELLOW: u32 = 0xf9e2af;
const RED: u32 = 0xf38ba8;
const PEACH: u32 = 0xfab387;
const TEAL: u32 = 0x94e2d5;

type Handler = RcBlock<dyn Fn(NonNull<NSEvent>)>;

pub enum Event {
    Toggle,
    MouseDown(f64, f64),
    Close(u64),
}

struct Open {
    generation: u64,
    handle: WindowHandle<Panel>,
    rect: Rect,
    segments: Vec<Rect>,
    monitor: Option<Retained<AnyObject>>,
    _handler: Handler,
}

impl Open {
    fn ignores(&self, x: f64, y: f64) -> bool {
        self.rect.contains(x, y) || self.segments.iter().any(|rect| rect.contains(x, y))
    }
}

struct Panel {
    server: Arc<Server>,
    sender: UnboundedSender<Event>,
    generation: u64,
    font: Option<&'static str>,
    focus: FocusHandle,
    _activation: Subscription,
    _ticker: Task<()>,
}

pub fn run(
    server: Arc<Server>,
    sender: UnboundedSender<Event>,
    mut events: UnboundedReceiver<Event>,
) {
    gpui_platform::application()
        .with_quit_mode(QuitMode::Explicit)
        .run(move |cx: &mut App| {
            cx.set_activation_policy(ActivationPolicy::Accessory);
            cx.bind_keys([KeyBinding::new("escape", Close, None)]);
            let font = cx
                .text_system()
                .all_font_names()
                .iter()
                .any(|name| name == FONT)
                .then_some(FONT);
            cx.spawn(async move |cx| {
                let mut open: Option<Open> = None;
                let mut generation = 0;
                while let Some(event) = events.next().await {
                    match event {
                        Event::Toggle => {
                            if let Some(current) = open.take()
                                && cx.update(|cx| close(current, cx))
                            {
                                continue;
                            }
                            generation += 1;
                            open = show(cx, &server, &sender, generation, font).await;
                        }
                        Event::MouseDown(x, y) => {
                            if let Some(current) = open.take_if(|current| !current.ignores(x, y)) {
                                cx.update(|cx| close(current, cx));
                            }
                        }
                        Event::Close(id) => {
                            if let Some(current) = open.take_if(|current| current.generation == id)
                            {
                                cx.update(|cx| close(current, cx));
                            }
                        }
                    }
                }
            })
            .detach();
        });
}

async fn show(
    cx: &mut AsyncApp,
    server: &Arc<Server>,
    sender: &UnboundedSender<Event>,
    generation: u64,
    font: Option<&'static str>,
) -> Option<Open> {
    let mouse = mouse().unwrap_or_default();
    let segments = cx
        .background_executor()
        .spawn(async {
            bar::ITEMS
                .iter()
                .filter_map(|item| bar::query(item))
                .flat_map(|query| geometry::segments(&query))
                .collect::<Vec<_>>()
        })
        .await;
    let height = height(&views(&server.snapshot(), server::now()));
    let Some(rect) = geometry::anchor(&segments, mouse, WIDTH, height) else {
        eprintln!("sketchyusage: no bar segment to anchor the panel");
        return None;
    };
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(rect.x as f32), px(rect.y as f32)),
            size: size(px(rect.width as f32), px(rect.height as f32)),
        })),
        titlebar: None,
        focus: true,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        window_background: WindowBackgroundAppearance::Blurred,
        ..Default::default()
    };
    let server = server.clone();
    let events = sender.clone();
    let opened = cx.update(|cx| {
        cx.open_window(options, move |window, cx| {
            cx.new(|cx| Panel::new(server, events, generation, font, window, cx))
        })
    });
    let handle = match opened {
        Ok(handle) => handle,
        Err(error) => {
            eprintln!("sketchyusage: panel failed to open: {error:#}");
            return None;
        }
    };
    let (monitor, handler) = monitor(sender.clone());
    Some(Open {
        generation,
        handle,
        rect,
        segments: segments.into_iter().map(|segment| segment.rect).collect(),
        monitor,
        _handler: handler,
    })
}

fn close(open: Open, cx: &mut App) -> bool {
    if let Some(monitor) = &open.monitor {
        unsafe { NSEvent::removeMonitor(monitor) };
    }
    open.handle
        .update(cx, |_, window, _| window.remove_window())
        .is_ok()
}

fn monitor(sender: UnboundedSender<Event>) -> (Option<Retained<AnyObject>>, Handler) {
    let handler: Handler = RcBlock::new(move |_: NonNull<NSEvent>| {
        if let Some((x, y)) = mouse() {
            let _ = sender.unbounded_send(Event::MouseDown(x, y));
        }
    });
    let monitor = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(
        NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown,
        &handler,
    );
    (monitor, handler)
}

fn mouse() -> Option<(f64, f64)> {
    let mtm = MainThreadMarker::new()?;
    let primary = NSScreen::screens(mtm).firstObject()?.frame().size.height;
    let location = NSEvent::mouseLocation();
    Some(geometry::from_cocoa(location.x, location.y, primary))
}

fn views(state: &State, now: i64) -> Vec<(&'static str, ProviderView)> {
    PROVIDERS
        .iter()
        .map(|id| (*id, format::provider(id, state.providers.get(*id), now)))
        .collect()
}

fn height(views: &[(&str, ProviderView)]) -> f64 {
    let lines: usize = views.iter().map(|(_, view)| view.lines()).sum();
    let gaps = views.len().saturating_sub(1) as f32;
    f64::from(2.0 * (PADDING + BORDER) + lines as f32 * LINE + gaps * SECTION_GAP)
}

impl Panel {
    fn new(
        server: Arc<Server>,
        sender: UnboundedSender<Event>,
        generation: u64,
        font: Option<&'static str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let resigned = sender.clone();
        let activation = cx.observe_window_activation(window, move |_, window, _| {
            if !window.is_window_active() {
                let _ = resigned.unbounded_send(Event::Close(generation));
            }
        });
        let ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        Self {
            server,
            sender,
            generation,
            font,
            focus,
            _activation: activation,
            _ticker: ticker,
        }
    }
}

impl Render for Panel {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let views = views(&self.server.snapshot(), server::now());
        let wanted = px(height(&views) as f32);
        if window.bounds().size.height != wanted {
            window.resize(size(px(WIDTH as f32), wanted));
        }
        let sender = self.sender.clone();
        let generation = self.generation;
        let root = div()
            .track_focus(&self.focus)
            .key_context("Panel")
            .on_action(move |_: &Close, _, _| {
                let _ = sender.unbounded_send(Event::Close(generation));
            })
            .size_full()
            .flex()
            .flex_col()
            .gap(px(SECTION_GAP))
            .p(px(PADDING))
            .bg(rgb(MANTLE))
            .border(px(BORDER))
            .border_color(rgb(SURFACE2))
            .rounded_lg()
            .overflow_hidden()
            .text_color(rgb(TEXT))
            .text_size(px(13.0))
            .line_height(px(LINE));
        let root = match self.font {
            Some(font) => root.font_family(font),
            None => root,
        };
        root.children(views.iter().map(|(id, view)| section(id, view)))
    }
}

fn section(id: &str, view: &ProviderView) -> Div {
    let accent = if id == "claude" { PEACH } else { TEAL };
    div()
        .flex()
        .flex_col()
        .child(
            div()
                .text_color(rgb(accent))
                .font_weight(FontWeight::BOLD)
                .child(view.name.clone()),
        )
        .children(view.windows.iter().map(|window| {
            div()
                .child(row(
                    div().child(window.label.clone()),
                    text(&window.remaining),
                ))
                .child(row(
                    div().text_color(rgb(SUBTEXT)).child(window.reset.clone()),
                    text(&window.burn),
                ))
        }))
        .children(
            view.credits
                .clone()
                .map(|credits| div().text_color(rgb(SUBTEXT)).truncate().child(credits)),
        )
        .children(view.notes.iter().map(|note| text(note).truncate()))
}

fn row(left: Div, right: Div) -> Div {
    div()
        .flex()
        .justify_between()
        .gap_2()
        .whitespace_nowrap()
        .child(left)
        .child(right)
}

fn text(line: &Line) -> Div {
    let color = match line.tone {
        Tone::Text => TEXT,
        Tone::Subtle => SUBTEXT,
        Tone::Warn => YELLOW,
        Tone::Alert => RED,
    };
    div()
        .whitespace_nowrap()
        .text_color(rgb(color))
        .child(line.text.clone())
}
