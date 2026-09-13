use std::{
    cell::RefCell,
    rc::Rc,
    string::String,
    vec,
    vec::Vec,
};

use popui::{
    FileKind, FileView, FileViewMode, Image, Label, Menu, MenuEntry, MenuId, TextInput,
    ToolbarButton, TreeView, Ui, UiRuntime, WidgetId, Window, WindowError,
};
use taffy::prelude::FromLength;

const WINDOW_W: u32 = 720;
const WINDOW_H: u32 = 500;
const MAX_HISTORY: usize = 32;

const ACT_OPEN: u32 = 1;
const ACT_REFRESH: u32 = 2;
const ACT_EXIT: u32 = 3;
const ACT_OPEN_TREE: u32 = 4;
const ACT_COPY_PATH: u32 = 10;
const ACT_PROPERTIES: u32 = 11;
const ACT_VIEW_ICONS: u32 = 20;
const ACT_VIEW_LIST: u32 = 21;
const ACT_VIEW_DETAILS: u32 = 22;
const ACT_ABOUT: u32 = 30;

const FOLDER_PNG: &[u8] = include_bytes!("../../icons/icons8-folder-48.png");
const OPEN_FOLDER_PNG: &[u8] = include_bytes!("../../icons/icons8-opened-folder-48.png");
const FILE_PNG: &[u8] = include_bytes!("../../icons/icons8-file-48.png");
const BINARY_PNG: &[u8] = include_bytes!("../../icons/icons8-binary-file-48.png");
const CONSOLE_PNG: &[u8] = include_bytes!("../../icons/icons8-console-48.png");
const TXT_PNG: &[u8] = include_bytes!("../../icons/icons8-txt-48.png");
const CSV_PNG: &[u8] = include_bytes!("../../icons/icons8-csv-48.png");
const CODE_PNG: &[u8] = include_bytes!("../../icons/icons8-code-file-48.png");
const IMAGE_PNG: &[u8] = include_bytes!("../../icons/icons8-image-file-48.png");
const AUDIO_PNG: &[u8] = include_bytes!("../../icons/icons8-audio-file-48.png");
const VIDEO_PNG: &[u8] = include_bytes!("../../icons/icons8-video-file-48.png");
const AVI_PNG: &[u8] = include_bytes!("../../icons/icons8-avi-48.png");
const MOV_PNG: &[u8] = include_bytes!("../../icons/icons8-mov-48.png");
const MPG_PNG: &[u8] = include_bytes!("../../icons/icons8-mpg-48.png");
const FLV_PNG: &[u8] = include_bytes!("../../icons/icons8-flv-48.png");
const REFRESH_PNG: &[u8] = include_bytes!("../../icons/icons8-available-updates-48.png");
const ICONS_VIEW_PNG: &[u8] = include_bytes!("../../icons/icons8-application-window-48.png");
const DETAILS_VIEW_PNG: &[u8] = include_bytes!("../../icons/icons8-document-48.png");

#[derive(Clone, Copy)]
struct ExplorerWidgets {
    files: WidgetId,
    tree: WidgetId,
    address: WidgetId,
    status: WidgetId,
    back: WidgetId,
    forward: WidgetId,
    up: WidgetId,
    refresh: WidgetId,
    icons_view: WidgetId,
    details_view: WidgetId,
}

struct NavState {
    history: Vec<String>,
    pos: usize,
}

impl NavState {
    fn new(path: &str) -> Self {
        Self { history: vec![normalize_path(path)], pos: 0 }
    }
    fn current(&self) -> &str { self.history.get(self.pos).map(String::as_str).unwrap_or("/") }
    fn can_back(&self) -> bool { self.pos > 0 }
    fn can_forward(&self) -> bool { self.pos + 1 < self.history.len() }
    fn record(&mut self, path: &str) {
        let path = normalize_path(path);
        if self.current() == path { return; }
        if self.pos + 1 < self.history.len() { self.history.truncate(self.pos + 1); }
        if self.history.len() == MAX_HISTORY {
            self.history.remove(0);
            self.pos = self.pos.saturating_sub(1);
        }
        self.history.push(path);
        self.pos = self.history.len() - 1;
    }
    fn back(&mut self) -> Option<String> {
        if !self.can_back() { return None; }
        self.pos -= 1;
        self.history.get(self.pos).cloned()
    }
    fn forward(&mut self) -> Option<String> {
        if !self.can_forward() { return None; }
        self.pos += 1;
        self.history.get(self.pos).cloned()
    }
}

#[derive(Clone, Copy, Debug)]
enum AppMessage {
    Exit,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;
    runtime.block_on(run())?;
    Ok(())
}

async fn run() -> Result<(), WindowError> {
    let window = Window::builder()
        .title("PopugOS File Manager")
        .position(8, 8)
        .size(WINDOW_W, WINDOW_H)
        .build()?;

    let mut ui = Ui::with_size(window.client_width(), window.client_height());
    let root = ui.root();
    ui.style(root, |s| s.flex_direction = popui::FlexDirection::Column);

    let menu = ui.menu(root, Menu::new(vec![
        MenuEntry::submenu("File", vec![
            MenuEntry::item(ACT_OPEN, "Open"),
            MenuEntry::item(ACT_REFRESH, "Refresh"),
            MenuEntry::separator(),
            MenuEntry::item(ACT_EXIT, "Exit"),
        ]),
        MenuEntry::submenu("Edit", vec![
            MenuEntry::item(ACT_COPY_PATH, "Copy path"),
            MenuEntry::item(ACT_PROPERTIES, "Properties"),
        ]),
        MenuEntry::submenu("View", vec![
            MenuEntry::item(ACT_VIEW_ICONS, "Icons"),
            MenuEntry::item(ACT_VIEW_LIST, "List"),
            MenuEntry::item(ACT_VIEW_DETAILS, "Details"),
        ]),
        MenuEntry::submenu("Help", vec![
            MenuEntry::item(ACT_ABOUT, "About"),
        ]),
    ]));

    let toolbar = ui.toolbar(root);
    let back = ui.toolbar_button(toolbar, "Back");
    let forward = ui.toolbar_button(toolbar, "Forward");
    let up = ui.toolbar_button(toolbar, "Up");
    let refresh = ui.toolbar_button(toolbar, "Refresh");
    ui.spacer(toolbar);
    let icons_view = ui.toolbar_button(toolbar, "Icons");
    let details_view = ui.toolbar_button(toolbar, "Details");

    let address_row = ui.row(root);
    ui.style(address_row, |s| {
        s.min_size.height = taffy::prelude::LengthPercentageAuto::length(34.0);
        s.flex_shrink = 0.0;
        s.align_items = Some(popui::AlignItems::CENTER);
        s.gap = taffy::geometry::Size::from_length(5.0);
        s.padding = taffy::geometry::Rect::length(3.0);
    });
    ui.label(address_row, "Address:");
    let address = ui.text_input_with(address_row, "/");
    if let Some(input) = ui.widget_mut::<TextInput>(address) { input.set_max_len(255); }
    if let Some(node) = ui.node_of(address) {
        ui.style(node, |s| {
            s.flex_grow = 1.0;
            s.flex_shrink = 1.0;
            s.min_size.width = taffy::prelude::LengthPercentageAuto::length(80.0);
        });
    }
    let go = ui.toolbar_button(address_row, "Go");

    let body = ui.row(root);
    ui.style(body, |s| {
        s.flex_grow = 1.0;
        s.flex_shrink = 1.0;
        s.min_size.width = taffy::prelude::LengthPercentageAuto::length(0.0);
        s.min_size.height = taffy::prelude::LengthPercentageAuto::length(0.0);
        s.gap = taffy::geometry::Size::from_length(1.0);
    });
    let tree = ui.tree_view(body);
    if let Some(node) = ui.node_of(tree) {
        ui.style(node, |s| {
            s.size.width = taffy::prelude::Dimension::length(190.0);
            s.min_size.width = taffy::prelude::LengthPercentageAuto::length(140.0);
            s.flex_grow = 0.0;
            s.flex_shrink = 0.0;
        });
    }
    let files = ui.file_view(body);

    let status_row = ui.row(root);
    ui.style(status_row, |s| {
        s.min_size.height = taffy::prelude::LengthPercentageAuto::length(25.0);
        s.flex_shrink = 0.0;
        s.align_items = Some(popui::AlignItems::CENTER);
        s.padding = taffy::geometry::Rect::length(3.0);
    });
    let status = ui.label(status_row, "Ready");

    let widgets = ExplorerWidgets {
        files,
        tree,
        address,
        status,
        back,
        forward,
        up,
        refresh,
        icons_view,
        details_view,
    };
    install_icons(&mut ui, widgets);

    if let Some(view) = ui.widget_mut::<TreeView>(tree) { let _ = view.load_root("/"); }
    if let Some(view) = ui.widget_mut::<FileView>(files) { let _ = view.load_dir("/"); }

    let nav = Rc::new(RefCell::new(NavState::new("/")));
    sync_navigation_buttons(&mut ui, widgets, &nav.borrow());
    update_status(&mut ui, widgets);

    let (mut runtime, tx) = UiRuntime::new(window, ui);

    {
        let nav = nav.clone();
        runtime.ui_mut().on_click(tree, move |ui| {
            let path = ui.widget_mut::<TreeView>(tree).and_then(TreeView::take_selected_path);
            if let Some(path) = path { let _ = navigate(ui, widgets, &nav, &path, true); }
        });
    }

    {
        let nav = nav.clone();
        runtime.ui_mut().on_click(files, move |ui| {
            let activated = ui.widget_mut::<FileView>(files).and_then(|view| {
                let index = view.take_activated()?;
                let item = view.items().get(index)?;
                Some((item.kind, view.item_path(index)?))
            });
            if let Some((kind, path)) = activated {
                if matches!(kind, FileKind::Directory | FileKind::Mount) {
                    let _ = navigate(ui, widgets, &nav, &path, true);
                } else {
                    set_label(ui, status, &format!("Selected: {path}"));
                }
            }
        });
    }

    {
        let nav = nav.clone();
        runtime.ui_mut().on_click(back, move |ui| {
            let old_pos = nav.borrow().pos;
            let target = nav.borrow_mut().back();
            if let Some(path) = target {
                if !navigate(ui, widgets, &nav, &path, false) {
                    nav.borrow_mut().pos = old_pos;
                    sync_navigation_buttons(ui, widgets, &nav.borrow());
                }
            }
        });
    }
    {
        let nav = nav.clone();
        runtime.ui_mut().on_click(forward, move |ui| {
            let old_pos = nav.borrow().pos;
            let target = nav.borrow_mut().forward();
            if let Some(path) = target {
                if !navigate(ui, widgets, &nav, &path, false) {
                    nav.borrow_mut().pos = old_pos;
                    sync_navigation_buttons(ui, widgets, &nav.borrow());
                }
            }
        });
    }
    {
        let nav = nav.clone();
        runtime.ui_mut().on_click(up, move |ui| {
            let current = String::from(nav.borrow().current());
            if let Some(parent) = parent_path(&current) { let _ = navigate(ui, widgets, &nav, &parent, true); }
        });
    }
    {
        let nav = nav.clone();
        runtime.ui_mut().on_click(refresh, move |ui| refresh_current(ui, widgets, &nav));
    }
    {
        let nav = nav.clone();
        runtime.ui_mut().on_click(go, move |ui| {
            if let Some(path) = ui.widget::<TextInput>(address).map(|input| input.text().to_owned()) {
                let _ = navigate(ui, widgets, &nav, &path, true);
            }
        });
    }
    {
        let nav = nav.clone();
        runtime.ui_mut().on_click(address, move |ui| {
            if let Some(path) = ui.widget::<TextInput>(address).map(|input| input.text().to_owned()) {
                let _ = navigate(ui, widgets, &nav, &path, true);
            }
        });
    }

    runtime.ui_mut().on_click(icons_view, move |ui| set_view_mode(ui, files, FileViewMode::Icons));
    runtime.ui_mut().on_click(details_view, move |ui| set_view_mode(ui, files, FileViewMode::Details));

    {
        let nav = nav.clone();
        let exit_tx = tx.clone();
        runtime.ui_mut().on_click(menu, move |ui| {
            let action = ui.widget_mut::<Menu>(menu).and_then(Menu::take_action);
            if let Some(action) = action {
                handle_menu_action(ui, widgets, &nav, action, &exit_tx);
            }
        });
    }

    runtime.ui_mut().on_context(files, move |ui, x, y| {
        let has_item = ui.widget::<FileView>(files).and_then(FileView::selected_item).is_some();
        let entries = if has_item {
            vec![
                MenuEntry::item(ACT_OPEN, "Open"),
                MenuEntry::item(ACT_COPY_PATH, "Copy path"),
                MenuEntry::item(ACT_PROPERTIES, "Properties"),
                MenuEntry::separator(),
                MenuEntry::item(ACT_REFRESH, "Refresh"),
            ]
        } else {
            vec![
                MenuEntry::item(ACT_REFRESH, "Refresh"),
                MenuEntry::separator(),
                MenuEntry::item(ACT_VIEW_ICONS, "Icons"),
                MenuEntry::item(ACT_VIEW_LIST, "List"),
                MenuEntry::item(ACT_VIEW_DETAILS, "Details"),
            ]
        };
        if let Some(menu) = ui.widget_mut::<Menu>(menu) { menu.show_context(x, y, entries); }
    });

    runtime.ui_mut().on_context(tree, move |ui, x, y| {
        let has_node = ui.widget::<TreeView>(tree).and_then(TreeView::selected_path).is_some();
        let entries = if has_node {
            vec![
                MenuEntry::item(ACT_OPEN_TREE, "Open"),
                MenuEntry::item(ACT_REFRESH, "Refresh"),
            ]
        } else {
            vec![MenuEntry::item(ACT_REFRESH, "Refresh")]
        };
        if let Some(menu) = ui.widget_mut::<Menu>(menu) { menu.show_context(x, y, entries); }
    });

    runtime
        .run(move |_ui, message| match message {
            AppMessage::Exit => popui::Control::Exit,
        })
        .await
}

fn handle_menu_action(
    ui: &mut Ui,
    widgets: ExplorerWidgets,
    nav: &Rc<RefCell<NavState>>,
    action: MenuId,
    exit_tx: &popui::UiSender<AppMessage>,
) {
    match action.0 {
        ACT_OPEN => open_selected(ui, widgets, nav),
        ACT_OPEN_TREE => open_selected_tree(ui, widgets, nav),
        ACT_REFRESH => refresh_current(ui, widgets, nav),
        ACT_EXIT => { let _ = exit_tx.send(AppMessage::Exit); }
        ACT_COPY_PATH => {
            let path = selected_path(ui, widgets).unwrap_or_else(|| String::from(nav.borrow().current()));
            set_label(ui, widgets.status, &format!("Path: {path}"));
        }
        ACT_PROPERTIES => show_properties(ui, widgets),
        ACT_VIEW_ICONS => set_view_mode(ui, widgets.files, FileViewMode::Icons),
        ACT_VIEW_LIST => set_view_mode(ui, widgets.files, FileViewMode::List),
        ACT_VIEW_DETAILS => set_view_mode(ui, widgets.files, FileViewMode::Details),
        ACT_ABOUT => set_label(ui, widgets.status, "PopugOS File Manager — std + PopUI + Tokio"),
        _ => {}
    }
}

fn open_selected_tree(ui: &mut Ui, widgets: ExplorerWidgets, nav: &Rc<RefCell<NavState>>) {
    if let Some(path) = ui
        .widget::<TreeView>(widgets.tree)
        .and_then(TreeView::selected_path)
        .map(String::from)
    {
        let _ = navigate(ui, widgets, nav, &path, true);
    }
}

fn open_selected(ui: &mut Ui, widgets: ExplorerWidgets, nav: &Rc<RefCell<NavState>>) {
    let selected = ui.widget::<FileView>(widgets.files).and_then(|view| {
        let item = view.selected_item()?;
        Some((item.kind, view.selected_path()?))
    });
    if let Some((kind, path)) = selected {
        if matches!(kind, FileKind::Directory | FileKind::Mount) {
            let _ = navigate(ui, widgets, nav, &path, true);
        } else {
            set_label(ui, widgets.status, &format!("Selected: {path}"));
        }
        return;
    }

    if let Some(path) = ui.widget::<TreeView>(widgets.tree).and_then(|tree| tree.selected_path()).map(String::from) {
        let _ = navigate(ui, widgets, nav, &path, true);
    }
}

fn show_properties(ui: &mut Ui, widgets: ExplorerWidgets) {
    let text = ui.widget::<FileView>(widgets.files).and_then(|view| {
        let item = view.selected_item()?;
        Some(format!("{} — {} — {} bytes", item.name, item.kind.label(), item.size))
    });
    set_label(ui, widgets.status, text.as_deref().unwrap_or("No item selected"));
}

fn selected_path(ui: &Ui, widgets: ExplorerWidgets) -> Option<String> {
    ui.widget::<FileView>(widgets.files).and_then(FileView::selected_path)
}

fn set_view_mode(ui: &mut Ui, files: WidgetId, mode: FileViewMode) {
    if let Some(view) = ui.widget_mut::<FileView>(files) { view.set_mode(mode); }
}

fn navigate(
    ui: &mut Ui,
    widgets: ExplorerWidgets,
    nav: &Rc<RefCell<NavState>>,
    requested_path: &str,
    record_history: bool,
) -> bool {
    let path = normalize_path(requested_path);
    let loaded = ui.widget_mut::<FileView>(widgets.files)
        .map(|view| view.load_dir(&path).is_ok())
        .unwrap_or(false);
    if !loaded {
        set_label(ui, widgets.status, "Cannot open directory");
        return false;
    }
    if record_history { nav.borrow_mut().record(&path); }
    if let Some(tree) = ui.widget_mut::<TreeView>(widgets.tree) { let _ = tree.select_path(&path); }
    if let Some(input) = ui.widget_mut::<TextInput>(widgets.address) { input.set_text(&path); }
    update_status(ui, widgets);
    sync_navigation_buttons(ui, widgets, &nav.borrow());
    true
}

fn refresh_current(ui: &mut Ui, widgets: ExplorerWidgets, nav: &Rc<RefCell<NavState>>) {
    let current = String::from(nav.borrow().current());
    let files_ok = ui.widget_mut::<FileView>(widgets.files)
        .map(|view| view.reload().is_ok())
        .unwrap_or(false);
    if let Some(tree) = ui.widget_mut::<TreeView>(widgets.tree) {
        let _ = tree.reload();
        let _ = tree.select_path(&current);
    }
    if files_ok { update_status(ui, widgets); } else { set_label(ui, widgets.status, "Refresh failed"); }
}

fn update_status(ui: &mut Ui, widgets: ExplorerWidgets) {
    let count = ui.widget::<FileView>(widgets.files).map(|view| view.items().len()).unwrap_or(0);
    set_label(ui, widgets.status, &format!("{count} object{}", if count == 1 { "" } else { "s" }));
}

fn sync_navigation_buttons(ui: &mut Ui, widgets: ExplorerWidgets, nav: &NavState) {
    if let Some(button) = ui.widget_mut::<ToolbarButton>(widgets.back) { button.set_enabled(nav.can_back()); }
    if let Some(button) = ui.widget_mut::<ToolbarButton>(widgets.forward) { button.set_enabled(nav.can_forward()); }
    if let Some(button) = ui.widget_mut::<ToolbarButton>(widgets.up) { button.set_enabled(nav.current() != "/"); }
}

fn set_label(ui: &mut Ui, id: WidgetId, text: &str) {
    if let Some(label) = ui.widget_mut::<Label>(id) { label.set_text(text); }
}

fn install_icons(ui: &mut Ui, widgets: ExplorerWidgets) {
    let folder = Image::from_png_bytes(FOLDER_PNG).ok();
    let opened = Image::from_png_bytes(OPEN_FOLDER_PNG).ok();
    let file = Image::from_png_bytes(FILE_PNG).ok();
    let binary = Image::from_png_bytes(BINARY_PNG).ok();
    let console = Image::from_png_bytes(CONSOLE_PNG).ok();
    let txt = Image::from_png_bytes(TXT_PNG).ok();
    let csv = Image::from_png_bytes(CSV_PNG).ok();
    let code = Image::from_png_bytes(CODE_PNG).ok();
    let image = Image::from_png_bytes(IMAGE_PNG).ok();
    let audio = Image::from_png_bytes(AUDIO_PNG).ok();
    let video = Image::from_png_bytes(VIDEO_PNG).ok();
    let avi = Image::from_png_bytes(AVI_PNG).ok();
    let mov = Image::from_png_bytes(MOV_PNG).ok();
    let mpg = Image::from_png_bytes(MPG_PNG).ok();
    let flv = Image::from_png_bytes(FLV_PNG).ok();

    for (id, bytes) in [
        (widgets.refresh, REFRESH_PNG),
        (widgets.icons_view, ICONS_VIEW_PNG),
        (widgets.details_view, DETAILS_VIEW_PNG),
    ] {
        if let (Some(button), Ok(icon)) = (ui.widget_mut::<ToolbarButton>(id), Image::from_png_bytes(bytes)) {
            button.set_icon(icon);
        }
    }
    if let (Some(button), Some(icon)) = (ui.widget_mut::<ToolbarButton>(widgets.up), opened.clone()) {
        button.set_icon(icon);
    }

    if let Some(view) = ui.widget_mut::<FileView>(widgets.files) {
        if let Some(icon) = folder.clone() { view.set_kind_icon(FileKind::Directory, icon); }
        if let Some(icon) = file.clone() { view.set_kind_icon(FileKind::File, icon); }
        if let Some(icon) = binary { view.set_kind_icon(FileKind::Executable, icon); }
        if let Some(icon) = console { view.set_kind_icon(FileKind::Device, icon); }
        if let Some(icon) = opened.clone() { view.set_kind_icon(FileKind::Mount, icon); }
        if let Some(icon) = file { view.set_kind_icon(FileKind::Unknown, icon); }

        for ext in ["txt", "log", "md"] {
            if let Some(icon) = txt.clone() { view.set_extension_icon(ext, icon); }
        }
        if let Some(icon) = csv { view.set_extension_icon("csv", icon); }
        for ext in ["rs", "c", "h", "cpp", "toml", "json", "xml", "html", "css", "js", "sh"] {
            if let Some(icon) = code.clone() { view.set_extension_icon(ext, icon); }
        }
        for ext in ["png", "jpg", "jpeg", "gif", "bmp"] {
            if let Some(icon) = image.clone() { view.set_extension_icon(ext, icon); }
        }
        for ext in ["mp3", "wav", "ogg", "flac"] {
            if let Some(icon) = audio.clone() { view.set_extension_icon(ext, icon); }
        }
        for ext in ["mp4", "mkv", "webm"] {
            if let Some(icon) = video.clone() { view.set_extension_icon(ext, icon); }
        }
        if let Some(icon) = avi { view.set_extension_icon("avi", icon); }
        if let Some(icon) = mov { view.set_extension_icon("mov", icon); }
        if let Some(icon) = mpg { view.set_extension_icon("mpg", icon.clone()); view.set_extension_icon("mpeg", icon); }
        if let Some(icon) = flv { view.set_extension_icon("flv", icon); }
    }
    if let Some(view) = ui.widget_mut::<TreeView>(widgets.tree) {
        if let Some(icon) = folder.clone() { view.set_folder_icon(icon); }
        if let Some(icon) = opened.clone() { view.set_folder_open_icon(icon.clone()); view.set_root_icon(icon); }
    }
}

fn normalize_path(path: &str) -> String {
    let path = path.trim();
    if path.is_empty() || path == "/" { return String::from("/"); }
    if path.starts_with('/') {
        let trimmed = path.trim_end_matches('/');
        if trimmed.is_empty() { String::from("/") } else { String::from(trimmed) }
    } else {
        format!("/{}", path.trim_matches('/'))
    }
}

fn parent_path(path: &str) -> Option<String> {
    let path = normalize_path(path);
    if path == "/" { return None; }
    let pos = path.rfind('/').unwrap_or(0);
    if pos == 0 { Some(String::from("/")) } else { Some(String::from(&path[..pos])) }
}
