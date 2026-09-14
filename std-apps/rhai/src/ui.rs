use std::{cell::RefCell, collections::VecDeque, fmt, rc::Rc};

use popui::{
    Button, Label, TextArea, TextInput, Ui as NativeUi, UiEvent, WidgetId, Window,
};
use rhai::{CustomType, Dynamic, Engine, EvalAltResult, Map, TypeBuilder};
use taffy::{
    geometry::{Rect as TRect, Size as TSize},
    prelude::{Dimension, LengthPercentage},
    tree::NodeId,
};

use crate::runtime_error;

#[derive(Debug, Clone, CustomType)]
#[rhai_type(name = "Ui", extra = Self::build_rhai_api)]
pub struct UiFactory;

impl UiFactory {
    fn build_rhai_api(builder: &mut TypeBuilder<Self>) {
        builder.with_fn("window", create_app);
    }
}

struct UiSignal {
    kind: &'static str,
    widget: WidgetId,
}

struct UiState {
    window: Window,
    ui: NativeUi,
    signals: Rc<RefCell<VecDeque<UiSignal>>>,
}

#[derive(Clone, CustomType)]
#[rhai_type(name = "UiApp", extra = Self::build_rhai_api)]
pub struct UiApp(#[rhai_type(skip)] Rc<RefCell<UiState>>);

impl fmt::Debug for UiApp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("UiApp") }
}

impl UiApp {
    fn build_rhai_api(builder: &mut TypeBuilder<Self>) {
        builder
            .with_fn("root", app_root)
            .with_fn("row", app_row)
            .with_fn("column", app_column)
            .with_fn("present", app_present)
            .with_fn("poll", app_poll)
            .on_print(|_| "UiApp".into())
            .on_debug(|_| "UiApp".into());
    }
}

#[derive(Clone, CustomType)]
#[rhai_type(name = "UiContainer", extra = Self::build_rhai_api)]
pub struct UiContainer {
    #[rhai_type(skip)] app: Rc<RefCell<UiState>>,
    #[rhai_type(skip)] node: NodeId,
}

impl fmt::Debug for UiContainer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("UiContainer") }
}

impl UiContainer {
    fn build_rhai_api(builder: &mut TypeBuilder<Self>) {
        builder
            .with_fn("row", container_row)
            .with_fn("column", container_column)
            .with_fn("panel", container_panel)
            .with_fn("spacer", container_spacer)
            .with_fn("label", container_label)
            .with_fn("button", container_button)
            .with_fn("text_input", container_text_input)
            .with_fn("text_area", container_text_area)
            .with_fn("grow", container_grow)
            .with_fn("width", container_width)
            .with_fn("height", container_height)
            .with_fn("padding", container_padding)
            .with_fn("gap", container_gap)
            .on_print(|_| "UiContainer".into())
            .on_debug(|_| "UiContainer".into());
    }
}

#[derive(Clone, CustomType)]
#[rhai_type(name = "UiElement", extra = Self::build_rhai_api)]
pub struct UiElement {
    #[rhai_type(skip)] app: Rc<RefCell<UiState>>,
    #[rhai_type(skip)] id: WidgetId,
}

impl fmt::Debug for UiElement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.debug_tuple("UiElement").field(&self.id.index()).finish() }
}

impl UiElement {
    fn build_rhai_api(builder: &mut TypeBuilder<Self>) {
        builder
            .with_get_set("text", element_text, element_set_text)
            .with_get("id", |element: &mut Self| element.id.index() as i32)
            .with_fn("grow", element_grow)
            .with_fn("width", element_width)
            .with_fn("height", element_height)
            .on_print(|element| format!("UiElement({})", element.id.index()))
            .on_debug(|element| format!("UiElement({})", element.id.index()));
    }
}

pub fn register(engine: &mut Engine) {
    engine
        .build_type::<UiFactory>()
        .build_type::<UiApp>()
        .build_type::<UiContainer>()
        .build_type::<UiElement>();
}

fn create_app(_: &mut UiFactory, title: &str, width: i32, height: i32) -> Result<UiApp, Box<EvalAltResult>> {
    let window = Window::builder().title(title).size(width.max(40) as u32, height.max(40) as u32).build().map_err(runtime_error)?;
    let ui = NativeUi::with_size(window.client_width(), window.client_height());
    Ok(UiApp(Rc::new(RefCell::new(UiState {
        window,
        ui,
        signals: Rc::new(RefCell::new(VecDeque::new())),
    }))))
}

fn app_root(app: &mut UiApp) -> UiContainer {
    let node = app.0.borrow().ui.root();
    UiContainer { app: app.0.clone(), node }
}

fn app_row(app: &mut UiApp) -> UiContainer {
    let root = app.0.borrow().ui.root();
    make_container(&app.0, root, false)
}

fn app_column(app: &mut UiApp) -> UiContainer {
    let root = app.0.borrow().ui.root();
    make_container(&app.0, root, true)
}

fn app_present(app: &mut UiApp) -> Result<(), Box<EvalAltResult>> {
    let mut state = app.0.borrow_mut();
    let UiState { window, ui, .. } = &mut *state;
    ui.frame(window).map(|_| ()).map_err(runtime_error)
}

fn app_poll(app: &mut UiApp) -> Result<Dynamic, Box<EvalAltResult>> {
    let mut state = app.0.borrow_mut();
    let pending = { state.signals.borrow_mut().pop_front() };
    if let Some(signal) = pending {
        return Ok(signal_map(signal, &app.0));
    }
    while let Some(event) = state.window.poll_event() {
        if matches!(event, popui::WindowEvent::Close) {
            let mut map = Map::new();
            map.insert("kind".into(), "close".into());
            return Ok(map.into());
        }
        if let Some(event) = UiEvent::from_window(event) { state.ui.dispatch(&event); }
        let pending = { state.signals.borrow_mut().pop_front() };
        if let Some(signal) = pending {
            let UiState { window, ui, .. } = &mut *state;
            ui.frame(window).map_err(runtime_error)?;
            return Ok(signal_map(signal, &app.0));
        }
    }
    let UiState { window, ui, .. } = &mut *state;
    ui.frame(window).map_err(runtime_error)?;
    Ok(Dynamic::UNIT)
}

fn signal_map(signal: UiSignal, app: &Rc<RefCell<UiState>>) -> Dynamic {
    let mut map = Map::new();
    map.insert("kind".into(), signal.kind.into());
    map.insert("target".into(), Dynamic::from(UiElement { app: app.clone(), id: signal.widget }));
    map.into()
}

fn make_container(app: &Rc<RefCell<UiState>>, parent: NodeId, column: bool) -> UiContainer {
    let node = if column { app.borrow_mut().ui.column(parent) } else { app.borrow_mut().ui.row(parent) };
    UiContainer { app: app.clone(), node }
}

fn container_row(container: &mut UiContainer) -> UiContainer { make_container(&container.app, container.node, false) }
fn container_column(container: &mut UiContainer) -> UiContainer { make_container(&container.app, container.node, true) }
fn container_panel(container: &mut UiContainer) -> UiContainer {
    let node = container.app.borrow_mut().ui.panel(container.node);
    UiContainer { app: container.app.clone(), node }
}
fn container_spacer(container: &mut UiContainer) -> UiContainer {
    let node = container.app.borrow_mut().ui.spacer(container.node);
    UiContainer { app: container.app.clone(), node }
}

fn container_label(container: &mut UiContainer, text: &str) -> UiElement {
    let id = container.app.borrow_mut().ui.label(container.node, text);
    UiElement { app: container.app.clone(), id }
}

fn container_button(container: &mut UiContainer, text: &str) -> UiElement {
    let mut state = container.app.borrow_mut();
    let id = state.ui.button(container.node, text);
    let signals = state.signals.clone();
    state.ui.on_click(id, move |_| signals.borrow_mut().push_back(UiSignal { kind: "click", widget: id }));
    UiElement { app: container.app.clone(), id }
}

fn container_text_input(container: &mut UiContainer, text: &str) -> UiElement {
    let mut state = container.app.borrow_mut();
    let id = state.ui.text_input_with(container.node, text);
    let signals = state.signals.clone();
    state.ui.on_change(id, move |_| signals.borrow_mut().push_back(UiSignal { kind: "change", widget: id }));
    UiElement { app: container.app.clone(), id }
}

fn container_text_area(container: &mut UiContainer, text: &str) -> UiElement {
    let mut state = container.app.borrow_mut();
    let id = state.ui.text_area_with(container.node, text);
    let signals = state.signals.clone();
    state.ui.on_change(id, move |_| signals.borrow_mut().push_back(UiSignal { kind: "change", widget: id }));
    UiElement { app: container.app.clone(), id }
}

fn update_node(app: &Rc<RefCell<UiState>>, node: NodeId, update: impl FnOnce(&mut taffy::Style)) {
    app.borrow_mut().ui.style(node, update);
}

fn container_grow(container: &mut UiContainer, value: i32) -> UiContainer {
    update_node(&container.app, container.node, |style| style.flex_grow = value.max(0) as f32);
    container.clone()
}
fn container_width(container: &mut UiContainer, value: i32) -> UiContainer {
    update_node(&container.app, container.node, |style| style.size.width = Dimension::length(value.max(0) as f32));
    container.clone()
}
fn container_height(container: &mut UiContainer, value: i32) -> UiContainer {
    update_node(&container.app, container.node, |style| style.size.height = Dimension::length(value.max(0) as f32));
    container.clone()
}
fn container_padding(container: &mut UiContainer, value: i32) -> UiContainer {
    let value = LengthPercentage::length(value.max(0) as f32);
    update_node(&container.app, container.node, |style| style.padding = TRect { left: value, right: value, top: value, bottom: value });
    container.clone()
}
fn container_gap(container: &mut UiContainer, value: i32) -> UiContainer {
    let value = LengthPercentage::length(value.max(0) as f32);
    update_node(&container.app, container.node, |style| style.gap = TSize { width: value, height: value });
    container.clone()
}

fn element_node(element: &UiElement) -> Option<NodeId> { element.app.borrow().ui.node_of(element.id) }
fn element_grow(element: &mut UiElement, value: i32) -> UiElement {
    if let Some(node) = element_node(element) { update_node(&element.app, node, |style| style.flex_grow = value.max(0) as f32); }
    element.clone()
}
fn element_width(element: &mut UiElement, value: i32) -> UiElement {
    if let Some(node) = element_node(element) { update_node(&element.app, node, |style| style.size.width = Dimension::length(value.max(0) as f32)); }
    element.clone()
}
fn element_height(element: &mut UiElement, value: i32) -> UiElement {
    if let Some(node) = element_node(element) { update_node(&element.app, node, |style| style.size.height = Dimension::length(value.max(0) as f32)); }
    element.clone()
}

fn element_text(element: &mut UiElement) -> String {
    let state = element.app.borrow();
    if let Some(widget) = state.ui.widget::<Label>(element.id) { return widget.text().into(); }
    if let Some(widget) = state.ui.widget::<Button>(element.id) { return widget.label().into(); }
    if let Some(widget) = state.ui.widget::<TextInput>(element.id) { return widget.text().into(); }
    if let Some(widget) = state.ui.widget::<TextArea>(element.id) { return widget.text().into(); }
    String::new()
}

fn element_set_text(element: &mut UiElement, value: String) {
    let mut state = element.app.borrow_mut();
    if let Some(widget) = state.ui.widget_mut::<Label>(element.id) { widget.set_text(&value); return; }
    if let Some(widget) = state.ui.widget_mut::<Button>(element.id) { widget.set_label(&value); return; }
    if let Some(widget) = state.ui.widget_mut::<TextInput>(element.id) { widget.set_text(&value); return; }
    if let Some(widget) = state.ui.widget_mut::<TextArea>(element.id) { widget.set_text(&value); }
}
