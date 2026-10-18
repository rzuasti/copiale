//! Onboarding state: which screen to show, driven by core events, and the
//! commands user actions produce. No GPUI; the view renders it.

use copiale_core::storage::Passphrase;
use copiale_core::{Command, Event, StorageFailure};

/// Shortest passphrase accepted for a new database.
pub const MIN_PASSPHRASE_CHARS: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Screen {
    Starting,
    Passphrase {
        new: bool,
        error: Option<String>,
        /// Waiting for the core after submitting.
        busy: bool,
    },
    StorageFailed {
        failure: StorageFailure,
        confirm_start_over: bool,
        busy: bool,
    },
    Unlinked {
        device_name: String,
        error: Option<String>,
        busy: bool,
    },
    Linking {
        url: String,
    },
    Ready,
    /// The phone unlinked this device; nothing is wiped until confirmed.
    Revoked {
        busy: bool,
    },
    Stopped,
}

#[derive(Debug)]
pub struct Onboarding {
    screen: Screen,
    /// Shown on the next [`Screen::Unlinked`] or [`Screen::Passphrase`]:
    /// why linking failed, or local data couldn't be read or written.
    pending_error: Option<String>,
    /// Last name used or suggested, kept across link attempts.
    device_name: Option<String>,
}

impl Default for Onboarding {
    fn default() -> Self {
        Self {
            screen: Screen::Starting,
            pending_error: None,
            device_name: None,
        }
    }
}

impl Onboarding {
    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    pub fn apply(&mut self, event: &Event) {
        match event {
            Event::Locked { new_passphrase } => {
                self.screen = Screen::Passphrase {
                    new: *new_passphrase,
                    error: self.pending_error.take(),
                    busy: false,
                };
            }
            Event::WrongPassphrase => {
                if let Screen::Passphrase { error, busy, .. } = &mut self.screen {
                    *error = Some("Wrong passphrase".into());
                    *busy = false;
                }
            }
            Event::StorageFailed(failure) => {
                self.screen = Screen::StorageFailed {
                    failure: failure.clone(),
                    confirm_start_over: false,
                    busy: false,
                };
            }
            Event::Unlinked { device_name } => {
                let device_name = self.device_name.get_or_insert_with(|| device_name.clone());
                self.screen = Screen::Unlinked {
                    device_name: device_name.clone(),
                    error: self.pending_error.take(),
                    busy: false,
                };
            }
            Event::LinkUrl(url) => self.screen = Screen::Linking { url: url.clone() },
            Event::LinkFailed(error) => self.pending_error = Some(error.to_string()),
            // Once ready, the chats show it.
            Event::DatabaseError(error) if self.screen != Screen::Ready => {
                self.pending_error = Some(format!("Local data: {error}"));
            }
            Event::Ready => self.screen = Screen::Ready,
            Event::LinkRevoked => self.screen = Screen::Revoked { busy: false },
            // Trying again worked.
            Event::Connected if matches!(self.screen, Screen::Revoked { .. }) => {
                self.screen = Screen::Ready;
            }
            Event::Stopped => self.screen = Screen::Stopped,
            _ => {}
        }
    }

    /// Validates and submits; `confirmation` is ignored when unlocking.
    pub fn submit_passphrase(&mut self, passphrase: &str, confirmation: &str) -> Option<Command> {
        let Screen::Passphrase { new, error, busy } = &mut self.screen else {
            return None;
        };
        if *busy {
            return None;
        }
        let problem = if passphrase.is_empty() {
            Some("Enter a passphrase".to_string())
        } else if *new && passphrase.chars().count() < MIN_PASSPHRASE_CHARS {
            Some(format!("Use at least {MIN_PASSPHRASE_CHARS} characters"))
        } else if *new && passphrase != confirmation {
            Some("Passphrases don't match".into())
        } else {
            None
        };
        *error = problem;
        if error.is_some() {
            return None;
        }
        *busy = true;
        let passphrase = Passphrase::new(passphrase);
        Some(if *new {
            Command::SetPassphrase(passphrase)
        } else {
            Command::Unlock(passphrase)
        })
    }

    pub fn link(&mut self, device_name: &str) -> Option<Command> {
        let Screen::Unlinked {
            device_name: shown,
            error,
            busy,
        } = &mut self.screen
        else {
            return None;
        };
        let name = device_name.trim();
        if *busy {
            return None;
        }
        if name.is_empty() {
            *error = Some("Enter a device name".into());
            return None;
        }
        *shown = name.into();
        *error = None;
        *busy = true;
        self.device_name = Some(name.into());
        Some(Command::Link {
            device_name: name.into(),
        })
    }

    pub fn cancel_link(&mut self) -> Option<Command> {
        matches!(self.screen, Screen::Linking { .. }).then_some(Command::CancelLink)
    }

    /// After the phone unlinked this device: wipes and starts over.
    pub fn wipe(&mut self) -> Option<Command> {
        let Screen::Revoked { busy } = &mut self.screen else {
            return None;
        };
        if *busy {
            return None;
        }
        *busy = true;
        Some(Command::Unlink)
    }

    /// After the phone unlinked this device: in case it was a glitch.
    pub fn reconnect(&mut self) -> Option<Command> {
        matches!(self.screen, Screen::Revoked { busy: false }).then_some(Command::Reconnect)
    }

    /// Whether the account's chats belong on screen, now or once back.
    pub fn has_account(&self) -> bool {
        matches!(self.screen, Screen::Ready | Screen::Revoked { .. })
    }

    pub fn retry(&mut self) -> Option<Command> {
        let Screen::StorageFailed { busy, .. } = &mut self.screen else {
            return None;
        };
        if *busy {
            return None;
        }
        *busy = true;
        Some(Command::Retry)
    }

    /// Asks for confirmation first; see [`Self::confirm_start_over`].
    pub fn start_over(&mut self) {
        if let Screen::StorageFailed {
            confirm_start_over, ..
        } = &mut self.screen
        {
            *confirm_start_over = true;
        }
    }

    pub fn cancel_start_over(&mut self) {
        if let Screen::StorageFailed {
            confirm_start_over, ..
        } = &mut self.screen
        {
            *confirm_start_over = false;
        }
    }

    pub fn confirm_start_over(&mut self) -> Option<Command> {
        let Screen::StorageFailed {
            confirm_start_over: true,
            busy,
            ..
        } = &mut self.screen
        else {
            return None;
        };
        if *busy {
            return None;
        }
        *busy = true;
        Some(Command::StartOver)
    }
}

/// Title and explanation for a storage failure.
pub fn describe(failure: &StorageFailure) -> (&'static str, String) {
    match failure {
        StorageFailure::KeyMissing => (
            "Can't open your local data",
            "The key for Copiale's local data is missing from your keychain, so the data \
             can't be decrypted. Start over to delete it and link this device again. Your \
             messages stay on your phone."
                .into(),
        ),
        StorageFailure::KeychainUnavailable(detail) => (
            "Can't reach your keychain",
            format!(
                "Copiale keeps its database key in your system keychain. Unlock it, or start \
                 a keyring service such as GNOME Keyring or KeePassXC, then try again. \
                 ({detail})"
            ),
        ),
        StorageFailure::WrongKey => (
            "Can't open your local data",
            "The key in your keychain doesn't match Copiale's local data. Start over to \
             delete the data and link this device again."
                .into(),
        ),
        StorageFailure::Other(detail) => ("Something went wrong", detail.clone()),
    }
}

#[cfg(test)]
mod tests {
    use copiale_core::BackendError;

    use super::*;

    fn at(event: Event) -> Onboarding {
        let mut model = Onboarding::default();
        model.apply(&event);
        model
    }

    fn unlinked() -> Onboarding {
        at(Event::Unlinked {
            device_name: "Copiale on host".into(),
        })
    }

    #[test]
    fn revoked_device_wipes_only_when_asked() {
        let mut model = at(Event::Ready);
        model.apply(&Event::LinkRevoked);
        assert_eq!(model.screen(), &Screen::Revoked { busy: false });
        assert!(model.has_account());
        assert!(matches!(model.reconnect(), Some(Command::Reconnect)));
        assert!(matches!(model.wipe(), Some(Command::Unlink)));
        assert_eq!(model.screen(), &Screen::Revoked { busy: true });
        assert!(model.wipe().is_none());
        assert!(model.reconnect().is_none());
        model.apply(&Event::Unlinked {
            device_name: "Copiale on host".into(),
        });
        assert!(!model.has_account());
    }

    #[test]
    fn reconnecting_after_revoked_returns_to_the_chats() {
        let mut model = at(Event::LinkRevoked);
        model.apply(&Event::Connected);
        assert_eq!(model.screen(), &Screen::Ready);
        // Connecting otherwise doesn't change screens.
        let mut model = unlinked();
        model.apply(&Event::Connected);
        assert!(matches!(model.screen(), Screen::Unlinked { .. }));
    }

    #[test]
    fn storage_errors_show_on_the_next_screen() {
        let mut model = at(Event::LinkRevoked);
        model.wipe();
        model.apply(&Event::DatabaseError("permission denied".into()));
        model.apply(&Event::Unlinked {
            device_name: "Copiale on host".into(),
        });
        assert!(matches!(
            model.screen(),
            Screen::Unlinked { error: Some(error), .. } if error == "Local data: permission denied"
        ));

        let mut model = at(Event::DatabaseError("permission denied".into()));
        model.apply(&Event::Locked {
            new_passphrase: false,
        });
        assert!(matches!(
            model.screen(),
            Screen::Passphrase { error: Some(_), .. }
        ));

        // Once ready, the chats show them instead.
        let mut model = at(Event::Ready);
        model.apply(&Event::DatabaseError("disk full".into()));
        model.apply(&Event::Unlinked {
            device_name: "Copiale on host".into(),
        });
        assert!(matches!(
            model.screen(),
            Screen::Unlinked { error: None, .. }
        ));
    }

    #[test]
    fn starts_on_starting_screen() {
        assert_eq!(Onboarding::default().screen(), &Screen::Starting);
    }

    #[test]
    fn new_passphrase_is_validated() {
        let mut model = at(Event::Locked {
            new_passphrase: true,
        });

        assert!(model.submit_passphrase("", "").is_none());
        assert!(model.submit_passphrase("short", "short").is_none());
        assert!(
            matches!(model.screen(), Screen::Passphrase { error: Some(e), .. } if e.contains("8"))
        );
        assert!(
            model
                .submit_passphrase("long enough", "long enougj")
                .is_none()
        );
        assert!(
            matches!(model.screen(), Screen::Passphrase { error: Some(e), .. } if e.contains("match"))
        );

        let command = model.submit_passphrase("long enough", "long enough");
        assert!(matches!(command, Some(Command::SetPassphrase(_))));
        assert!(matches!(
            model.screen(),
            Screen::Passphrase {
                error: None,
                busy: true,
                ..
            }
        ));
    }

    #[test]
    fn unlock_ignores_confirmation_and_length() {
        let mut model = at(Event::Locked {
            new_passphrase: false,
        });

        let command = model.submit_passphrase("short", "");

        assert!(matches!(command, Some(Command::Unlock(_))));
    }

    #[test]
    fn busy_passphrase_screen_ignores_resubmits() {
        let mut model = at(Event::Locked {
            new_passphrase: false,
        });
        model.submit_passphrase("first", "");

        assert!(model.submit_passphrase("second", "").is_none());
    }

    #[test]
    fn wrong_passphrase_shows_error_and_unlocks_form() {
        let mut model = at(Event::Locked {
            new_passphrase: false,
        });
        model.submit_passphrase("nope", "");

        model.apply(&Event::WrongPassphrase);

        assert_eq!(
            model.screen(),
            &Screen::Passphrase {
                new: false,
                error: Some("Wrong passphrase".into()),
                busy: false,
            }
        );
    }

    #[test]
    fn link_uses_trimmed_name_and_waits() {
        let mut model = unlinked();

        let command = model.link("  My laptop ");

        assert!(
            matches!(command, Some(Command::Link { device_name }) if device_name == "My laptop")
        );
        assert!(matches!(
            model.screen(),
            Screen::Unlinked { busy: true, .. }
        ));
        assert!(model.link("again").is_none());
    }

    #[test]
    fn empty_device_name_is_rejected() {
        let mut model = unlinked();

        assert!(model.link("   ").is_none());
        assert!(matches!(
            model.screen(),
            Screen::Unlinked {
                error: Some(_),
                busy: false,
                ..
            }
        ));
    }

    #[test]
    fn link_url_shows_qr_and_can_cancel() {
        let mut model = unlinked();
        model.link("x");

        model.apply(&Event::LinkUrl("sgnl://linkdevice?x".into()));

        assert_eq!(
            model.screen(),
            &Screen::Linking {
                url: "sgnl://linkdevice?x".into()
            }
        );
        assert!(matches!(model.cancel_link(), Some(Command::CancelLink)));
    }

    #[test]
    fn link_failure_is_shown_with_the_last_name() {
        let mut model = unlinked();
        model.link("My laptop");
        model.apply(&Event::LinkUrl("u".into()));

        model.apply(&Event::LinkFailed(BackendError::new("timed out")));
        model.apply(&Event::Unlinked {
            device_name: "Copiale on host".into(),
        });

        assert_eq!(
            model.screen(),
            &Screen::Unlinked {
                device_name: "My laptop".into(),
                error: Some("timed out".into()),
                busy: false,
            }
        );
    }

    #[test]
    fn start_over_needs_confirmation() {
        let mut model = at(Event::StorageFailed(StorageFailure::KeyMissing));

        assert!(model.confirm_start_over().is_none());
        model.start_over();
        model.cancel_start_over();
        assert!(model.confirm_start_over().is_none());
        model.start_over();

        assert!(matches!(
            model.confirm_start_over(),
            Some(Command::StartOver)
        ));
        assert!(model.retry().is_none(), "busy until the core answers");
    }

    #[test]
    fn retry_from_storage_failure() {
        let mut model = at(Event::StorageFailed(StorageFailure::KeychainUnavailable(
            "locked".into(),
        )));

        assert!(matches!(model.retry(), Some(Command::Retry)));
    }

    #[test]
    fn actions_outside_their_screen_do_nothing() {
        let mut model = Onboarding::default();

        assert!(model.submit_passphrase("x", "x").is_none());
        assert!(model.link("x").is_none());
        assert!(model.cancel_link().is_none());
        assert!(model.retry().is_none());
        assert!(model.confirm_start_over().is_none());
        assert_eq!(model.screen(), &Screen::Starting);
    }

    #[test]
    fn ready_and_stopped() {
        let mut model = unlinked();
        model.apply(&Event::Ready);
        assert_eq!(model.screen(), &Screen::Ready);
        model.apply(&Event::Synced);
        assert_eq!(model.screen(), &Screen::Ready);
        model.apply(&Event::Stopped);
        assert_eq!(model.screen(), &Screen::Stopped);
    }

    #[test]
    fn every_failure_is_described() {
        for failure in [
            StorageFailure::KeyMissing,
            StorageFailure::KeychainUnavailable("locked".into()),
            StorageFailure::WrongKey,
            StorageFailure::Other("disk full".into()),
        ] {
            let (title, body) = describe(&failure);
            assert!(!title.is_empty() && !body.is_empty());
        }
    }
}
