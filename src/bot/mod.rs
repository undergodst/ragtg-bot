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

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![deps])
        .enable_ctrlc_handler()
        .build()
}
