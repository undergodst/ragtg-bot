pub mod commands;
pub mod handlers;

use teloxide::dispatching::{Dispatcher, UpdateFilterExt};
use teloxide::prelude::*;

use crate::deps::Deps;

/// Build the teloxide dispatcher: command handlers (`/start`, `/ping`, `/stats`)
/// + a catch-all that persists every other message.
pub fn build_dispatcher(
    bot: Bot,
    deps: Deps,
) -> Dispatcher<Bot, teloxide::RequestError, teloxide::dispatching::DefaultKey> {
    let handler = Update::filter_message()
        .branch(
            dptree::entry()
                .filter_command::<commands::Command>()
                .endpoint(commands::handle),
        )
        .endpoint(handlers::handle_message);

    // No `enable_ctrlc_handler()`: main.rs owns the single Ctrl+C wait, drives
    // graceful shutdown via `shutdown_token`, and runs the 5s timeout. Two
    // racing handlers would let teloxide's win, fire `shutdown()` first, and
    // then main.rs's `if let Ok(fut)` branch would be skipped (Err = "already
    // shut down"), forcing an immediate `abort()` with no graceful window.
    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![deps])
        .build()
}
