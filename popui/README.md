# popui

UI crate for the PopugOS `std` userspace.

`libfelix::ui` remains the legacy native UI and is intentionally untouched.
PopUI is not a compatibility layer for it.

## Architecture

```text
std application
├── std::fs / std::net / std::time / ...
├── tokio
├── popugos
│   └── window / WM syscalls only
└── popui
    ├── widgets
    ├── Taffy layout
    ├── retained UI state
    └── Tokio UI runtime
```

The `popugos` crate contains only PopugOS-specific extensions which Rust `std`
does not have. PopUI therefore uses ordinary `std::fs` for filesystem work,
Tokio/`std::net` for networking, `std::time::Instant` for UI timing, etc. There
are no duplicate fs/net/time wrappers.

## UI + Tokio

Widgets, layout and drawing are synchronous. The UI task owns `Window` and `Ui`.
Background Tokio tasks communicate with it using `UiSender<M>`.

```rust,ignore
let window = popui::Window::builder()
    .title("Example")
    .size(640, 480)
    .build()?;

let ui = popui::Ui::with_size(640, 480);
let (runtime, tx) = popui::UiRuntime::<Message>::new(window, ui);

let worker_tx = tx.clone();
tokio::spawn(async move {
    let result = reqwest::get("https://example.com").await;
    let _ = worker_tx.send(Message::Loaded(result.is_ok()));
});

runtime.run(|ui, message| {
    // Update widgets synchronously here.
    popui::Control::Continue
}).await?;
```

Window event waiting is currently timer-backed non-busy polling around the
existing `SYS_WM_POLL` syscall. The API is deliberately isolated in the runtime:
when the WM exposes a pollable event handle, it can be wired into Mio/Tokio
readiness without changing application or widget code.

## Controls

The std PopUI implementation now contains:

- `Button`
- `Label`
- `TextInput`
- `TextArea`
- `Icon`
- `ToolbarButton`
- `FileView`
- `TreeView`

`FileView` and `TreeView` use `std::fs` directly. `FileView` uses
`std::time::Instant` for double-click timing. PNG loading in `Image` uses
`std::fs::read`.
