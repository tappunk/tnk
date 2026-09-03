use std::sync::atomic::{AtomicU8, Ordering};

use clap::error::ErrorKind;

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

pub fn log_warn(message: &str) {
    if GLOBAL_VERBOSITY.load(Ordering::Relaxed) >= VERBOSITY_NORMAL {
        eprintln!("warning: {}", message);
    }
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

pub fn clap_exit_code_for_kind(kind: &ErrorKind) -> i32 {
    match kind {
        ErrorKind::DisplayHelp
        | ErrorKind::DisplayVersion
        | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => 0,
        ErrorKind::UnknownArgument
        | ErrorKind::InvalidSubcommand
        | ErrorKind::MissingRequiredArgument
        | ErrorKind::TooManyValues
        | ErrorKind::TooFewValues
        | ErrorKind::WrongNumberOfValues
        | ErrorKind::InvalidValue
        | ErrorKind::ValueValidation
        | ErrorKind::ArgumentConflict
        | ErrorKind::NoEquals
        | ErrorKind::MissingSubcommand
        | ErrorKind::InvalidUtf8 => 64,
        _ => 1,
    }
}

#[cfg(test)]
mod clap_exit_tests {
    use super::*;

    #[test]
    fn help_exits_zero() {
        assert_eq!(clap_exit_code_for_kind(&ErrorKind::DisplayHelp), 0);
    }

    #[test]
    fn version_exits_zero() {
        assert_eq!(clap_exit_code_for_kind(&ErrorKind::DisplayVersion), 0);
    }

    #[test]
    fn unknown_argument_exits_sixty_four() {
        assert_eq!(clap_exit_code_for_kind(&ErrorKind::UnknownArgument), 64);
    }

    #[test]
    fn invalid_subcommand_exits_sixty_four() {
        assert_eq!(clap_exit_code_for_kind(&ErrorKind::InvalidSubcommand), 64);
    }

    #[test]
    fn missing_required_exits_sixty_four() {
        assert_eq!(
            clap_exit_code_for_kind(&ErrorKind::MissingRequiredArgument),
            64
        );
    }
}
