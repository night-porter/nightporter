//! Desktop notifications (native notification center). Best effort: failures are only logged.

use crate::config::Notifications;

#[derive(Clone, Copy, Debug)]
pub enum Kind {
    NeedsApproval,
    Failed,
    Done,
    NewAuthor,
}

pub fn send(n: &Notifications, kind: Kind, title: &str, body: &str) {
    let on = match kind {
        Kind::NeedsApproval => n.needs_approval,
        Kind::Failed => n.failed,
        Kind::Done => n.done,
        Kind::NewAuthor => n.new_author,
    };
    if !on || std::env::var_os("NIGHTPORTER_NO_NOTIFY").is_some() {
        return;
    }
    let (title, body) = (format!("Night Porter: {title}"), body.to_string());
    // notify-rust can block briefly (D-Bus / NSUserNotification); keep it off the async runtime.
    std::thread::spawn(move || {
        if let Err(e) = notify_rust::Notification::new().summary(&title).body(&body).appname("Night Porter").show() {
            tracing::debug!("notification failed: {e}");
        }
    });
}
