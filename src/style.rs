//! Terminal styles; print through `anstream` so they are stripped when not a TTY.

use anstyle::{AnsiColor, Style};

use crate::policy::{Access, Network};

pub const OK: Style = AnsiColor::Green.on_default().bold();
pub const ERROR: Style = AnsiColor::Red.on_default().bold();
pub const WARN: Style = AnsiColor::Yellow.on_default().bold();
pub const LABEL: Style = Style::new().bold();
pub const DIM: Style = Style::new().dimmed();

pub fn access(access: Access) -> Style {
    match access {
        Access::Read => AnsiColor::Green.on_default(),
        Access::Write | Access::ReadWrite => AnsiColor::Yellow.on_default(),
    }
}

pub fn network(network: Network) -> Style {
    match network {
        Network::Allow => AnsiColor::Yellow.on_default(),
        Network::Block => AnsiColor::Green.on_default(),
    }
}

/// `--help` and clap error styles.
pub const HELP: clap::builder::Styles = clap::builder::Styles::styled()
    .header(AnsiColor::Green.on_default().bold())
    .usage(AnsiColor::Green.on_default().bold())
    .literal(AnsiColor::Cyan.on_default().bold())
    .placeholder(AnsiColor::Cyan.on_default());
