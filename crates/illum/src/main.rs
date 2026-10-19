// SPDX-License-Identifier: GPL-3.0-only
//! `illum`: one binary, every role (frontend, service, backend, control).

use illum_core::Role;
use std::process::ExitCode;

fn main() -> ExitCode {
    let role = match Role::from_args(std::env::args().skip(1)) {
        Ok(role) => role,
        Err(err) => {
            eprintln!("illum: {err}");
            eprintln!("usage: illum [--service | --backend | --control]");
            return ExitCode::from(2);
        }
    };
    eprintln!("illum {} — role {role:?} is not implemented yet", env!("CARGO_PKG_VERSION"));
    ExitCode::SUCCESS
}
