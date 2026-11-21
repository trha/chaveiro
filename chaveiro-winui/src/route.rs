//! Which screen the window shows, and how it moves from one to the next.

use vault_core::core::Screen;
use winui_immediate::Slide;

/// A screen of the window. Login and SecondFactor form the login flow, where
/// moving on slides forward and going back slides back. Locked and Vault
/// stand apart, so a change to or from them is a cut.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum Route {
    Login,
    SecondFactor,
    Locked,
    Vault,
}

impl Route {
    /// The route for the core's screen. Before the core exists, and while
    /// it starts, the window shows the login form.
    pub fn of(screen: Option<&Screen<'_>>) -> Route {
        match screen {
            None | Some(Screen::Starting | Screen::Login { .. }) => Route::Login,
            Some(Screen::SecondFactor { .. }) => Route::SecondFactor,
            Some(Screen::Locked { .. }) => Route::Locked,
            Some(Screen::Vault(_)) => Route::Vault,
        }
    }

    /// The position in the login flow, or [`None`] outside it.
    fn step(self) -> Option<u8> {
        match self {
            Route::Login => Some(0),
            Route::SecondFactor => Some(1),
            Route::Locked | Route::Vault => None,
        }
    }

    /// Whether the title bar offers a way back to an earlier step.
    pub fn has_back(self) -> bool {
        self.step().is_some_and(|step| step > 0)
    }

    /// How the window moves from `self` to `to`.
    pub fn slide_to(self, to: Route) -> Slide {
        match (self.step(), to.step()) {
            (Some(from), Some(to)) if to > from => Slide::Forward,
            (Some(from), Some(to)) if to < from => Slide::Back,
            _ => Slide::Instant,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Route; 4] = [
        Route::Login,
        Route::SecondFactor,
        Route::Locked,
        Route::Vault,
    ];

    #[test]
    fn the_login_flow_slides_forward_and_back() {
        assert_eq!(Route::Login.slide_to(Route::SecondFactor), Slide::Forward);
        assert_eq!(Route::SecondFactor.slide_to(Route::Login), Slide::Back);
    }

    #[test]
    fn every_change_to_or_from_locked_or_vault_is_a_cut() {
        for from in ALL {
            for to in [Route::Locked, Route::Vault] {
                assert_eq!(from.slide_to(to), Slide::Instant, "{from:?} to {to:?}");
                assert_eq!(to.slide_to(from), Slide::Instant, "{to:?} to {from:?}");
            }
        }
    }

    #[test]
    fn only_the_second_factor_has_back() {
        let with_back: Vec<_> = ALL.into_iter().filter(|route| route.has_back()).collect();
        assert_eq!(with_back, [Route::SecondFactor]);
    }
}
