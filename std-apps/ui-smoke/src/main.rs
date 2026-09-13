use std::time::Duration;

use popui::{layout, Button, Control, Label, Style, TextInput, Ui, UiRuntime, Window};

#[derive(Clone, Copy, Debug)]
enum Message {
    StartTimer,
    TimerDone,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;

    runtime.block_on(run())?;
    Ok(())
}

async fn run() -> Result<(), popui::WindowError> {
    let window = Window::builder()
        .title("PopUI async smoke")
        .size(520, 260)
        .build()?;

    let mut ui = Ui::with_size(window.client_width(), window.client_height());
    let root = ui.root();
    let column = ui.column(root);
    let _ = ui.set_style(column, layout::fill());
    let _ = ui.set_style(column, layout::padding(16.0));
    let _ = ui.set_style(column, layout::gap(10.0));

    let status = ui.add_widget(column, Label::new("Tokio UI runtime is ready"), Style::default());
    let input = ui.add_widget(column, TextInput::new("type here"), Style::default());
    let button = ui.add_widget(column, Button::new("Run async timer"), Style::default());

    let (mut runtime, tx) = UiRuntime::new(window, ui);

    let click_tx = tx.clone();
    runtime.ui_mut().on_click(button, move |_| {
        let _ = click_tx.send(Message::StartTimer);
    });

    let worker_tx = tx.clone();
    runtime
        .run(move |ui, message| {
            match message {
                Message::StartTimer => {
                    if let Some(label) = ui.widget_mut::<Label>(status) {
                        label.set_text("Waiting asynchronously for 1 second...");
                    }

                    let tx = worker_tx.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        let _ = tx.send(Message::TimerDone);
                    });
                }
                Message::TimerDone => {
                    let current = ui
                        .widget::<TextInput>(input)
                        .map(|input| input.text().to_owned())
                        .unwrap_or_default();
                    if let Some(label) = ui.widget_mut::<Label>(status) {
                        label.set_text(&format!("Timer done; input = {current}"));
                    }
                }
            }
            Control::Continue
        })
        .await
}
