use std::{fs::{self, File, OpenOptions}, process::{Child, Command, Stdio}, rc::Rc, time::Duration};

use felix_rhai::system_engine;
use popui::{Control, EditorDiagnostic, Label, Menu, MenuEntry, TextArea, TextInput, Ui, UiRuntime, WidgetId, Window, WindowError};
use taffy::prelude::{Dimension, LengthPercentageAuto};

const ACT_NEW: u32 = 1;
const ACT_OPEN: u32 = 2;
const ACT_SAVE: u32 = 3;
const ACT_RUN: u32 = 4;
const ACT_EXIT: u32 = 5;
const ACT_CHECK: u32 = 6;
const ACT_API: u32 = 7;
const ACT_STOP: u32 = 8;
const ACT_FORMAT: u32 = 9;

const STARTER: &str = "";
const RUN_OUTPUT: &str = "/tmp/rhai-ide-output.log";

const COMPLETIONS: &[&str] = &[
    "append_text", "ARGS", "blob", "body", "break", "button", "catch", "column", "const", "continue",
    "create_dir", "cwd", "delete", "else", "env", "exists", "export", "false", "fn", "for", "gap", "get",
    "grow", "header", "headers", "height", "http", "if", "import", "in", "is_dir", "is_file", "json", "label",
    "let", "list", "loop", "metadata", "ok", "OS", "padding", "panel", "parse", "patch", "poll", "post",
    "present", "pretty", "private", "PROGRAM", "put", "query", "read_bytes", "read_text", "remove", "request",
    "return", "root", "row", "run", "SCRIPT", "send", "sleep", "spacer", "status", "stringify", "switch", "text",
    "text_area", "text_input", "throw", "true", "try", "ui", "while", "width", "window", "write_bytes", "write_text",
];

const SIGNATURES: &[&str] = &[
    "append_text(path: string, contents: string)",
    "body(contents: string) -> HttpRequest",
    "button(text: string) -> UiElement",
    "column() -> UiContainer",
    "create_dir(path: string)",
    "cwd() -> string",
    "delete(url: string) -> HttpResponse",
    "env(name: string) -> string",
    "exists(path: string) -> bool",
    "get(url: string) -> HttpResponse",
    "grow(weight: int) -> dynamic",
    "header(name: string, value: string) -> HttpRequest",
    "height(pixels: int) -> dynamic",
    "is_dir(path: string) -> bool",
    "is_file(path: string) -> bool",
    "json(value: dynamic) -> HttpRequest",
    "label(text: string) -> UiElement",
    "list(path: string) -> array<DirEntry>",
    "metadata(path: string) -> FileMetadata",
    "padding(pixels: int) -> UiContainer",
    "parse(text: string) -> dynamic",
    "patch(url: string, value: dynamic) -> HttpResponse",
    "poll() -> map",
    "post(url: string, value: dynamic) -> HttpResponse",
    "present()",
    "pretty(value: dynamic) -> string",
    "print(value: dynamic)",
    "put(url: string, value: dynamic) -> HttpResponse",
    "query(name: string, value: dynamic) -> HttpRequest",
    "read_bytes(path: string) -> blob",
    "read_text(path: string) -> string",
    "remove(path: string)",
    "request(method: string, url: string) -> HttpRequest",
    "row() -> UiContainer",
    "run(program: string, arguments: array) -> int",
    "send() -> HttpResponse",
    "sleep(milliseconds: int)",
    "stringify(value: dynamic) -> string",
    "text_area(text: string) -> UiElement",
    "text_input(text: string) -> UiElement",
    "width(pixels: int) -> dynamic",
    "window(title: string, width: int, height: int) -> UiApp",
    "write_bytes(path: string, contents: blob)",
    "write_text(path: string, contents: string)",
];

const TYPE_MEMBERS: &[(&str, &str, &str)] = &[
    ("Fs", "read_text", "read_text(path: string) -> string"),
    ("Fs", "read_bytes", "read_bytes(path: string) -> blob"),
    ("Fs", "write_text", "write_text(path: string, contents: string)"),
    ("Fs", "write_bytes", "write_bytes(path: string, contents: blob)"),
    ("Fs", "append_text", "append_text(path: string, contents: string)"),
    ("Fs", "list", "list(path: string) -> array<DirEntry>"),
    ("Fs", "metadata", "metadata(path: string) -> FileMetadata"),
    ("Fs", "exists", "exists(path: string) -> bool"),
    ("Fs", "is_file", "is_file(path: string) -> bool"),
    ("Fs", "is_dir", "is_dir(path: string) -> bool"),
    ("Fs", "create_dir", "create_dir(path: string)"),
    ("Fs", "remove", "remove(path: string)"),
    ("Http", "get", "get(url: string) -> HttpResponse"),
    ("Http", "post", "post(url: string, value: dynamic) -> HttpResponse"),
    ("Http", "put", "put(url: string, value: dynamic) -> HttpResponse"),
    ("Http", "patch", "patch(url: string, value: dynamic) -> HttpResponse"),
    ("Http", "delete", "delete(url: string) -> HttpResponse"),
    ("Http", "request", "request(method: string, url: string) -> HttpRequest"),
    ("HttpRequest", "header", "header(name: string, value: string) -> HttpRequest"),
    ("HttpRequest", "query", "query(name: string, value: dynamic) -> HttpRequest"),
    ("HttpRequest", "json", "json(value: dynamic) -> HttpRequest"),
    ("HttpRequest", "body", "body(contents: string | blob) -> HttpRequest"),
    ("HttpRequest", "send", "send() -> HttpResponse"),
    ("HttpResponse", "status", "status: int"),
    ("HttpResponse", "body", "body: string"),
    ("HttpResponse", "bytes", "bytes: blob"),
    ("HttpResponse", "headers", "headers: map"),
    ("HttpResponse", "ok", "ok: bool"),
    ("HttpResponse", "json", "json() -> dynamic"),
    ("Json", "parse", "parse(text: string) -> dynamic"),
    ("Json", "stringify", "stringify(value: dynamic) -> string"),
    ("Json", "pretty", "pretty(value: dynamic) -> string"),
    ("Ui", "window", "window(title: string, width: int, height: int) -> UiApp"),
    ("UiApp", "root", "root() -> UiContainer"),
    ("UiApp", "row", "row() -> UiContainer"),
    ("UiApp", "column", "column() -> UiContainer"),
    ("UiApp", "present", "present()"),
    ("UiApp", "poll", "poll() -> map"),
    ("UiContainer", "row", "row() -> UiContainer"),
    ("UiContainer", "column", "column() -> UiContainer"),
    ("UiContainer", "panel", "panel() -> UiContainer"),
    ("UiContainer", "spacer", "spacer() -> UiContainer"),
    ("UiContainer", "label", "label(text: string) -> UiElement"),
    ("UiContainer", "button", "button(text: string) -> UiElement"),
    ("UiContainer", "text_input", "text_input(text: string) -> UiElement"),
    ("UiContainer", "text_area", "text_area(text: string) -> UiElement"),
    ("UiContainer", "grow", "grow(weight: int) -> UiContainer"),
    ("UiContainer", "width", "width(pixels: int) -> UiContainer"),
    ("UiContainer", "height", "height(pixels: int) -> UiContainer"),
    ("UiContainer", "padding", "padding(pixels: int) -> UiContainer"),
    ("UiContainer", "gap", "gap(pixels: int) -> UiContainer"),
    ("UiElement", "text", "text: string"),
    ("UiElement", "id", "id: int"),
    ("UiElement", "grow", "grow(weight: int) -> UiElement"),
    ("UiElement", "width", "width(pixels: int) -> UiElement"),
    ("UiElement", "height", "height(pixels: int) -> UiElement"),
    ("DirEntry", "name", "name: string"),
    ("DirEntry", "path", "path: string"),
    ("DirEntry", "is_file", "is_file: bool"),
    ("DirEntry", "is_dir", "is_dir: bool"),
    ("DirEntry", "size", "size: int"),
    ("FileMetadata", "is_file", "is_file: bool"),
    ("FileMetadata", "is_dir", "is_dir: bool"),
    ("FileMetadata", "size", "size: int"),
    ("FileMetadata", "readonly", "readonly: bool"),
];

#[derive(Debug)]
enum Message { Run, Stop, Tick, Exit }

#[derive(Clone, Copy)]
struct Widgets { path: WidgetId, editor: WidgetId, output: WidgetId, status: WidgetId }

struct ManagedChild(Child);

impl ManagedChild {
    fn kill_and_wait(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) { self.kill_and_wait(); }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tokio::runtime::Builder::new_current_thread().enable_time().build()?.block_on(run())?;
    Ok(())
}

async fn run() -> Result<(), WindowError> {
    let window = Window::builder().title("Rhai IDE").position(30, 24).size(840, 620).build()?;
    let mut ui = Ui::with_size(window.client_width(), window.client_height());
    let root = ui.root();
    ui.style(root, |s| s.flex_direction = popui::FlexDirection::Column);

    let menu = ui.menu(root, Menu::new(vec![
        MenuEntry::submenu("File", vec![MenuEntry::item(ACT_NEW, "New"), MenuEntry::item(ACT_OPEN, "Open path"), MenuEntry::item(ACT_SAVE, "Save"), MenuEntry::separator(), MenuEntry::item(ACT_EXIT, "Exit")]),
        MenuEntry::submenu("Edit", vec![MenuEntry::item(ACT_FORMAT, "Format document")]),
        MenuEntry::submenu("Run", vec![MenuEntry::item(ACT_RUN, "Run script"), MenuEntry::item(ACT_STOP, "Stop script"), MenuEntry::item(ACT_CHECK, "Check syntax")]),
        MenuEntry::submenu("Help", vec![MenuEntry::item(ACT_API, "System API")]),
    ]));

    let toolbar = ui.toolbar(root);
    let new_button = ui.toolbar_button(toolbar, "New");
    let open_button = ui.toolbar_button(toolbar, "Open");
    let save_button = ui.toolbar_button(toolbar, "Save");
    let run_button = ui.toolbar_button(toolbar, "Run");
    let stop_button = ui.toolbar_button(toolbar, "Stop");
    let format_button = ui.toolbar_button(toolbar, "Format");
    ui.spacer(toolbar);
    let check_button = ui.toolbar_button(toolbar, "Check");

    let path_row = ui.row(root);
    ui.style(path_row, |s| { s.size.height = Dimension::length(32.0); s.flex_shrink = 0.0; s.align_items = Some(popui::AlignItems::CENTER); });
    ui.label(path_row, " File: ");
    let path = ui.text_input_with(path_row, "/home/user/untitled.rhai");
    if let Some(node) = ui.node_of(path) { ui.style(node, |s| { s.flex_grow = 1.0; s.min_size.width = LengthPercentageAuto::length(0.0); }); }

    let body = ui.column(root);
    ui.style(body, |s| { s.flex_grow = 1.0; s.flex_shrink = 1.0; s.min_size.height = LengthPercentageAuto::length(0.0); });
    let editor = ui.text_area_with(body, STARTER);
    if let Some(node) = ui.node_of(editor) { ui.style(node, |s| { s.flex_grow = 3.0; s.flex_basis = Dimension::length(0.0); s.min_size.height = LengthPercentageAuto::length(120.0); }); }
    if let Some(editor) = ui.widget_mut::<TextArea>(editor) {
        editor.set_rhai_mode(true);
        editor.set_completions(COMPLETIONS.iter().copied());
        editor.set_signatures(SIGNATURES.iter().copied());
        editor.set_type_members(TYPE_MEMBERS.iter().copied());
        editor.set_type_hints([
            ("fs", "Fs"), ("http", "Http"), ("json", "Json"), ("ui", "Ui"),
            ("OS", "string"), ("PROGRAM", "string"), ("SCRIPT", "string"), ("ARGS", "array"),
            ("status", "int"), ("body", "string"), ("headers", "map"), ("ok", "bool"),
        ]);
    }
    let output = ui.text_area_with(body, "Ready. Output and runtime errors appear here.");
    if let Some(node) = ui.node_of(output) { ui.style(node, |s| { s.flex_grow = 1.0; s.flex_basis = Dimension::length(0.0); s.min_size.height = LengthPercentageAuto::length(72.0); }); }

    let status_row = ui.row(root);
    ui.style(status_row, |s| { s.size.height = Dimension::length(24.0); s.flex_shrink = 0.0; s.align_items = Some(popui::AlignItems::CENTER); });
    let status = ui.label(status_row, "Syntax: checking...");
    let widgets = Widgets { path, editor, output, status };
    let engine = Rc::new(system_engine());
    let (mut runtime, tx) = UiRuntime::new(window, ui);

    for button in [run_button] { let tx = tx.clone(); runtime.ui_mut().on_click(button, move |_| { let _ = tx.send(Message::Run); }); }
    { let tx = tx.clone(); runtime.ui_mut().on_click(stop_button, move |_| { let _ = tx.send(Message::Stop); }); }
    { runtime.ui_mut().on_click(format_button, move |ui| { format_editor(ui, widgets); }); }
    { let widgets = widgets; runtime.ui_mut().on_click(save_button, move |ui| { save(ui, widgets); }); }
    { let widgets = widgets; runtime.ui_mut().on_click(open_button, move |ui| { open(ui, widgets); }); }
    { let widgets = widgets; runtime.ui_mut().on_click(new_button, move |ui| { new_file(ui, widgets); }); }
    { let engine = engine.clone(); runtime.ui_mut().on_click(check_button, move |ui| { validate(ui, widgets, &engine); }); }
    { let engine = engine.clone(); runtime.ui_mut().on_change(editor, move |ui| { validate(ui, widgets, &engine); }); }
    {
        let tx = tx.clone(); let engine = engine.clone();
        runtime.ui_mut().on_click(menu, move |ui| {
            let action = ui.widget_mut::<Menu>(menu).and_then(Menu::take_action).map(|id| id.0);
            match action {
                Some(ACT_NEW) => new_file(ui, widgets), Some(ACT_OPEN) => open(ui, widgets), Some(ACT_SAVE) => { save(ui, widgets); },
                Some(ACT_RUN) => { let _ = tx.send(Message::Run); }, Some(ACT_STOP) => { let _ = tx.send(Message::Stop); },
                Some(ACT_FORMAT) => format_editor(ui, widgets), Some(ACT_CHECK) => validate(ui, widgets, &engine),
                Some(ACT_API) => set_output(ui, widgets, "Types: int, bool, string, array, map, blob, Fs, DirEntry, FileMetadata, Http, HttpRequest, HttpResponse, Json, Ui, UiApp, UiContainer, UiElement.\nFilesystem: fs.read_text/read_bytes/write_text/write_bytes/append_text/list/metadata/exists/is_file/is_dir/create_dir/remove.\nNetwork: http.get/post/put/patch/delete or http.request(method, url).header(...).query(...).json(...).send(). Responses expose status/body/headers/ok/json().\nGUI: ui.window(title, w, h); app.root/row/column/present/poll; containers provide row/column/panel/spacer/label/button/text_input/text_area plus grow/width/height/padding/gap.\nModules: import files from /lib/rhai (resolver cache is disabled).\nSystem: env, cwd, sleep, run. Constants: fs, http, json, ui, OS, PROGRAM, SCRIPT, ARGS."),
                Some(ACT_EXIT) => { let _ = tx.send(Message::Exit); }, _ => {}
            }
        });
    }

    {
        let tx = tx.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(50)).await;
                if tx.send(Message::Tick).is_err() { break; }
            }
        });
    }

    let mut child: Option<ManagedChild> = None;
    runtime.run(move |ui, message| {
        match message {
            Message::Run => {
                if child.is_some() {
                    set_output(ui, widgets, "A script is already running. Stop it before running again.");
                    return Control::Continue;
                }
                if !save(ui, widgets) { return Control::Continue; }
                validate(ui, widgets, &engine);
                if ui.widget::<TextArea>(widgets.editor).and_then(TextArea::diagnostic).is_some() {
                    set_output(ui, widgets, "Run cancelled: fix the syntax error first.");
                    return Control::Continue;
                }
                match start_script(&path_text(ui, widgets)) {
                    Ok(process) => {
                        child = Some(process);
                        set_label(ui, widgets.status, "Running...");
                        set_output(ui, widgets, "Script started. Output will appear when it exits.");
                    }
                    Err(error) => {
                        set_label(ui, widgets.status, "Run failed");
                        set_output(ui, widgets, &format!("Cannot start script: {error}"));
                    }
                }
            }
            Message::Stop => {
                if let Some(mut process) = child.take() {
                    process.kill_and_wait();
                    finish_run(ui, widgets, "Stopped");
                }
            }
            Message::Tick => {
                let finished = match child.as_mut() {
                    Some(process) => match process.0.try_wait() {
                        Ok(Some(status)) => Some(format!("Exited with status {}", status.code().unwrap_or(1))),
                        Ok(None) => None,
                        Err(error) => Some(format!("Wait failed: {error}")),
                    },
                    None => None,
                };
                if let Some(status) = finished {
                    child = None;
                    finish_run(ui, widgets, &status);
                }
            }
            Message::Exit => {
                if let Some(mut process) = child.take() { process.kill_and_wait(); }
                return Control::Exit;
            }
        }
        Control::Continue
    }).await
}

fn start_script(path: &str) -> std::io::Result<ManagedChild> {
    let stdout = File::create(RUN_OUTPUT)?;
    let stderr = OpenOptions::new().create(true).append(true).open(RUN_OUTPUT)?;
    Command::new("/bin/rhai")
        .arg(path)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .map(ManagedChild)
}

fn finish_run(ui: &mut Ui, widgets: Widgets, status: &str) {
    let output = fs::read_to_string(RUN_OUTPUT).unwrap_or_default();
    let text = if output.is_empty() { format!("[{status}]") } else { format!("{output}\n[{status}]") };
    set_output(ui, widgets, &text);
    set_label(ui, widgets.status, status);
}

fn format_editor(ui: &mut Ui, widgets: Widgets) {
    if let Some(editor) = ui.widget_mut::<TextArea>(widgets.editor) { editor.format_rhai(); }
    set_label(ui, widgets.status, "Formatted");
}

fn path_text(ui: &Ui, widgets: Widgets) -> String { ui.widget::<TextInput>(widgets.path).map(|p| p.text().to_owned()).unwrap_or_else(|| "/home/user/untitled.rhai".into()) }
fn set_label(ui: &mut Ui, id: WidgetId, text: &str) { if let Some(label) = ui.widget_mut::<Label>(id) { label.set_text(text); } }
fn set_output(ui: &mut Ui, widgets: Widgets, text: &str) { if let Some(output) = ui.widget_mut::<TextArea>(widgets.output) { output.set_text(text); } }

fn new_file(ui: &mut Ui, widgets: Widgets) {
    if let Some(path) = ui.widget_mut::<TextInput>(widgets.path) { path.set_text("/home/user/untitled.rhai"); }
    if let Some(editor) = ui.widget_mut::<TextArea>(widgets.editor) { editor.set_text(STARTER); }
    set_label(ui, widgets.status, "New file");
}

fn open(ui: &mut Ui, widgets: Widgets) {
    let path = path_text(ui, widgets);
    match fs::read_to_string(&path) {
        Ok(source) => { if let Some(editor) = ui.widget_mut::<TextArea>(widgets.editor) { editor.set_owned_text(source); } set_label(ui, widgets.status, "Opened"); }
        Err(error) => set_output(ui, widgets, &format!("Cannot open {path}: {error}")),
    }
}

fn save(ui: &mut Ui, widgets: Widgets) -> bool {
    let path = path_text(ui, widgets);
    let source = ui.widget::<TextArea>(widgets.editor).map(|e| e.text().to_owned()).unwrap_or_default();
    match fs::write(&path, source) {
        Ok(()) => { if let Some(editor) = ui.widget_mut::<TextArea>(widgets.editor) { editor.mark_saved(); } set_label(ui, widgets.status, "Saved"); true }
        Err(error) => { set_output(ui, widgets, &format!("Cannot save {path}: {error}")); false }
    }
}

fn validate(ui: &mut Ui, widgets: Widgets, engine: &rhai::Engine) {
    let source = ui.widget::<TextArea>(widgets.editor).map(|e| e.text().to_owned()).unwrap_or_default();
    match engine.compile(&source) {
        Ok(_) => { if let Some(editor) = ui.widget_mut::<TextArea>(widgets.editor) { editor.set_diagnostic(None); } set_label(ui, widgets.status, "Syntax: OK"); }
        Err(error) => {
            let position = error.position();
            let line = position.line().unwrap_or(1); let column = position.position().unwrap_or(1); let message = error.to_string();
            if let Some(editor) = ui.widget_mut::<TextArea>(widgets.editor) { editor.set_diagnostic(Some(EditorDiagnostic { line, column, message: message.clone() })); }
            set_label(ui, widgets.status, &format!("Error {line}:{column}: {message}"));
        }
    }
}
