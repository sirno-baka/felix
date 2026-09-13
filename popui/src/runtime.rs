use std::time::Duration;

use popugos::window::{Event as WindowEvent, Window, WindowError};
use tokio::sync::mpsc;

use crate::{Ui, UiEvent};

pub type UiSender<M> = mpsc::UnboundedSender<M>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    Continue,
    Exit,
}

enum Wake<M> {
    Window(WindowEvent),
    Message(M),
    MessagesClosed,
}

/// Tokio-driven owner of one window and one UI tree.
///
/// The UI itself is deliberately synchronous. Background Tokio tasks never
/// borrow widgets directly; they send application messages to this owner task.
pub struct UiRuntime<M> {
    window: Window,
    ui: Ui,
    messages: mpsc::UnboundedReceiver<M>,
    event_poll_interval: Duration,
}

impl<M> UiRuntime<M> {
    pub fn new(window: Window, ui: Ui) -> (Self, UiSender<M>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Self {
                window,
                ui,
                messages: rx,
                event_poll_interval: Duration::from_millis(4),
            },
            tx,
        )
    }

    /// Temporary WM polling cadence. The public runtime API does not depend on
    /// polling: when WM handles become pollable by Mio, only this wait primitive
    /// needs to change.
    pub fn set_event_poll_interval(&mut self, interval: Duration) {
        self.event_poll_interval = interval.max(Duration::from_millis(1));
    }

    pub fn ui(&self) -> &Ui { &self.ui }
    pub fn ui_mut(&mut self) -> &mut Ui { &mut self.ui }
    pub fn window(&self) -> &Window { &self.window }
    pub fn window_mut(&mut self) -> &mut Window { &mut self.window }

    pub fn into_parts(self) -> (Window, Ui) { (self.window, self.ui) }

    /// Run the UI owner task.
    ///
    /// `update` handles messages sent by background tasks. Network, file and
    /// timer work should use normal std/Tokio APIs and send small results back
    /// through `UiSender`.
    pub async fn run<F>(mut self, mut update: F) -> Result<(), WindowError>
    where
        F: FnMut(&mut Ui, M) -> Control,
    {
        let _ = self.ui.frame(&mut self.window)?;
        let mut messages_open = true;

        loop {
            let wake = tokio::select! {
                event = next_window_event(&mut self.window, self.event_poll_interval) => {
                    Wake::Window(event)
                }
                message = self.messages.recv(), if messages_open => {
                    match message {
                        Some(message) => Wake::Message(message),
                        None => Wake::MessagesClosed,
                    }
                }
            };

            match wake {
                Wake::Window(WindowEvent::Close) => break,
                Wake::Window(event) => {
                    if let Some(event) = UiEvent::from_window(event) {
                        self.ui.dispatch(&event);
                    }
                }
                Wake::Message(message) => {
                    if update(&mut self.ui, message) == Control::Exit {
                        break;
                    }
                }
                Wake::MessagesClosed => {
                    messages_open = false;
                }
            }

            let _ = self.ui.frame(&mut self.window)?;
        }

        Ok(())
    }
}

async fn next_window_event(window: &mut Window, interval: Duration) -> WindowEvent {
    loop {
        if let Some(event) = window.poll_event() {
            return event;
        }
        // Timer-backed polling is non-busy and keeps the UI responsive today.
        // Later this becomes a Mio readiness wait when WM exposes a pollable
        // window/event handle; UiRuntime and application code stay unchanged.
        tokio::time::sleep(interval).await;
    }
}
