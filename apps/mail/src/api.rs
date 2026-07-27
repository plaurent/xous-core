#![allow(dead_code)]

/// GAM/xous-names server name for this app. Must be unique, < 64 chars.
pub(crate) const SERVER_NAME_MAIL: &str = "_Mail IMAP/SMTP client_";

/// Opcodes the Chat UI dispatches back to this app's server. The numeric
/// values matter only in that they must be distinct; `FromPrimitive` maps
/// the incoming message id back to a variant.
#[derive(Debug, num_derive::FromPrimitive, num_derive::ToPrimitive)]
pub enum MailOp {
    /// A Chat UI event (Focus, F1..F4, arrows, ...). The scalar arg is a
    /// `chat::Event` discriminant.
    Event = 0,
    /// An app-menu item was clicked (payload is a `MenuOp`).
    Menu,
    /// The user committed a Post in the Chat UI input box. Unused here
    /// (compose happens through a modal form under F2) but wired up so the
    /// Chat UI has a valid opcode to send to.
    Post,
    /// A raw keystroke forwarded by the Chat UI. Unused.
    Rawkeys,
    /// Exit the application.
    Quit,
}

/// App-menu actions handled by this app (as opposed to the Chat UI). Only
/// a no-op "close" entry today, matching the sigchat reference.
#[derive(Debug, num_derive::FromPrimitive, num_derive::ToPrimitive)]
pub enum MenuOp {
    Noop,
}
