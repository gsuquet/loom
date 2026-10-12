//! Exit codes of `loomd` (ADR-0010).

/// How the daemon stopped after serving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shutdown {
    /// `SIGTERM`: a service manager (launchd, systemd) or `kill` stopped it.
    Terminated,
    /// `SIGINT`: Ctrl-C.
    Interrupted,
    /// The auto-upgrade replaced the binaries: a service manager or the watchdog starts the
    /// new loomd (ADR-0011).
    Upgraded,
}

impl Shutdown {
    /// Exit code after a graceful shutdown: 0 on `SIGTERM`, which service managers expect, and
    /// after an upgrade, and 130 (128 + 2) on `SIGINT`.
    #[must_use]
    pub const fn exit_code(self) -> u8 {
        match self {
            Self::Terminated | Self::Upgraded => 0,
            Self::Interrupted => 130,
        }
    }
}

/// Exit code of an error: invalid configuration, lock taken, bind failure.
pub const FAILURE: u8 = 1;

#[cfg(test)]
mod tests {
    use super::{FAILURE, Shutdown};

    #[test]
    fn exit_codes_follow_the_conventions() {
        assert_eq!(Shutdown::Terminated.exit_code(), 0);
        assert_eq!(Shutdown::Interrupted.exit_code(), 130);
        assert_eq!(Shutdown::Upgraded.exit_code(), 0);
        assert_eq!(FAILURE, 1);
    }
}
