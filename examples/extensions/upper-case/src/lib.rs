//! Example Kureha Code extension: upper-cases the selection.
use kureha_extension::{Reply, Request};

fn handle(request: Request) -> Reply {
    match request {
        Request::Activate => Reply::ok(),
        Request::Command {
            id: "upper-case.selection",
            document,
        } => {
            let selection = document.selection.clone();
            if selection.is_empty() {
                return Reply::status("Select some text to upper-case");
            }
            match document.read(selection.clone()) {
                Ok(text) => Reply::edit(selection, text.to_uppercase())
                    .with_status(format!("Upper-cased {} characters", text.chars().count())),
                Err(e) => Reply::error(e),
            }
        }
        _ => Reply::unsupported(),
    }
}

kureha_extension::export!(handle);
