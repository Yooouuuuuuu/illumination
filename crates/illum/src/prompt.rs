// SPDX-License-Identifier: GPL-3.0-only
//! Console prompts for trusting a host and pairing a device. Stand-in until the accept panel
//! (backend) and the app window exist.

use illum_net::{Prompt, PromptKind, PromptRequest};
use std::io::{BufRead, Write};
use std::sync::Arc;
use tokio::sync::Mutex;

pub fn console_prompt() -> Prompt {
    // One question on screen at a time, even with several connections arriving.
    let turn = Arc::new(Mutex::new(()));
    Arc::new(move |req: PromptRequest| {
        let turn = turn.clone();
        Box::pin(async move {
            let _guard = turn.lock().await;
            tokio::task::spawn_blocking(move || ask(&req))
                .await
                .unwrap_or(false)
        })
    })
}

fn ask(req: &PromptRequest) -> bool {
    let question = match req.kind {
        PromptKind::TrustHost => format!(
            "\nFirst connection to {}. The host's key fingerprint is:\n    {}\n\
             Check that the host shows the same fingerprint. Trust this host? [y/N] ",
            req.address, req.fingerprint
        ),
        PromptKind::PairPeer => format!(
            "\nDevice \"{}\" at {} wants to connect. Its key fingerprint is:\n    {}\n\
             Compare it with what that device shows. Accept and pair it? [y/N] ",
            req.name, req.address, req.fingerprint
        ),
    };
    let mut out = std::io::stdout();
    let _ = out.write_all(question.as_bytes());
    let _ = out.flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}
