// Copyright 2026 tappunk
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicU8, Ordering};

const VERBOSITY_QUIET: u8 = 0;
const VERBOSITY_NORMAL: u8 = 1;
const VERBOSITY_VERBOSE: u8 = 2;

static GLOBAL_VERBOSITY: AtomicU8 = AtomicU8::new(VERBOSITY_NORMAL);

pub fn set_quiet() {
    GLOBAL_VERBOSITY.store(VERBOSITY_QUIET, Ordering::Relaxed);
}

pub fn set_verbose() {
    GLOBAL_VERBOSITY.store(VERBOSITY_VERBOSE, Ordering::Relaxed);
}

pub fn is_quiet() -> bool {
    GLOBAL_VERBOSITY.load(Ordering::Relaxed) == VERBOSITY_QUIET
}

pub fn is_verbose() -> bool {
    GLOBAL_VERBOSITY.load(Ordering::Relaxed) == VERBOSITY_VERBOSE
}

pub fn log_info(message: &str) {
    if GLOBAL_VERBOSITY.load(Ordering::Relaxed) >= VERBOSITY_NORMAL {
        eprintln!("info: {}", message);
    }
}

pub fn log_verbose(message: &str) {
    if GLOBAL_VERBOSITY.load(Ordering::Relaxed) == VERBOSITY_VERBOSE {
        eprintln!("info: {}", message);
    }
}

pub fn log_warn(message: &str) {
    if GLOBAL_VERBOSITY.load(Ordering::Relaxed) >= VERBOSITY_NORMAL {
        eprintln!("warning: {}", message);
    }
}

pub fn select_list(items: &[&str]) -> Option<usize> {
    if items.is_empty() {
        return None;
    }

    loop {
        let stderr = io::stderr();
        let mut handle = stderr.lock();

        writeln!(handle).ok();
        for (i, item) in items.iter().enumerate() {
            writeln!(handle, "  {}) {}", i + 1, item).ok();
        }
        write!(handle, "Select preset (1-{}) or q to quit: ", items.len()).ok();
        handle.flush().ok();

        let mut input = String::new();
        io::stdin().read_line(&mut input).ok();

        let trimmed = input.trim();
        if trimmed == "q" || trimmed.is_empty() {
            return None;
        }

        if let Some(n) = trimmed
            .parse::<usize>()
            .ok()
            .filter(|n| *n > 0 && *n <= items.len())
        {
            return Some(n - 1);
        }
    }
}

pub fn is_human_output(output: crate::OutputFormat) -> bool {
    if output != crate::OutputFormat::Text {
        return false;
    }
    if std::env::var("NO_COLOR").is_ok() {
        return false;
    }
    if let Ok(v) = std::env::var("CLICOLOR")
        && v == "0"
    {
        return false;
    }
    if let Ok(v) = std::env::var("CLICOLOR_FORCE")
        && v == "1"
    {
        return true;
    }
    io::stderr().is_terminal()
}

#[derive(Copy, Clone, PartialEq)]
pub enum ExitCode {
    Usage = 64,
    NotFound = 66,
    PermissionDenied = 77,
    Error = 1,
}

impl ExitCode {
    pub fn as_i32(&self) -> i32 {
        *self as i32
    }
}

pub fn exit_with(code: ExitCode, msg: &str) -> ! {
    eprintln!("error: {msg}");
    std::process::exit(code.as_i32());
}

pub fn exit_code_for_error(err: &color_eyre::Report) -> ExitCode {
    let msg = err.to_string().to_lowercase();
    if msg.contains("outside the configured workspace root") {
        ExitCode::Usage
    } else if msg.contains("permission denied") {
        ExitCode::PermissionDenied
    } else {
        ExitCode::Error
    }
}
