//! Which screen the window shows, and how it moves from one to the next.

use vault_core::core::{Challenge, Screen};
use winui_immediate::Slide;

/// A screen of the window. Login, Methods and SecondFactor form the login
/// flow, where moving on slides forward and going back slides back. Locked
/// and Vault stand apart, so a change to or from them is a cut.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum Route {
    Login,
    /// The list of second-factor methods.
    Methods,
    SecondFactor,
    Locked,
    Vault,
}

impl Route {
    /// The route for the core's screen. Before the core exists, and while
    /// it starts, the window shows the login form. On the second factor,
    /// `choosing` says whether the method list shows.
    pub fn of(screen: Option<&Screen<'_>>, choosing: impl FnOnce(&Challenge) -> bool) -> Route {
        match screen {
            None | Some(Screen::Starting | Screen::Login { .. }) => Route::Login,
            Some(Screen::SecondFactor { challenge, .. }) => match choosing(challenge) {
                true => Route::Methods,
                false => Route::SecondFactor,
            },
            Some(Screen::Locked { .. }) => Route::Locked,
            Some(Screen::Vault(_)) => Route::Vault,
        }
    }

    /// The position in the login flow, or [`None`] outside it.
    fn step(self) -> Option<u8> {
        match self {
            Route::Login => Some(0),
            Route::Methods => Some(1),
            Route::SecondFactor => Some(2),
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

    const ALL: [Route; 5] = [
        Route::Login,
        Route::Methods,
        Route::SecondFactor,
        Route::Locked,
        Route::Vault,
    ];

    #[test]
    fn the_login_flow_slides_forward_and_back() {
        let flow = [Route::Login, Route::Methods, Route::SecondFactor];
        for (index, from) in flow.into_iter().enumerate() {
            for to in &flow[index + 1..] {
                assert_eq!(from.slide_to(*to), Slide::Forward, "{from:?} to {to:?}");
                assert_eq!(to.slide_to(from), Slide::Back, "{to:?} to {from:?}");
            }
        }
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
    fn only_the_second_factor_screens_have_back() {
        let with_back: Vec<_> = ALL.into_iter().filter(|route| route.has_back()).collect();
        assert_eq!(with_back, [Route::Methods, Route::SecondFactor]);
    }

    #[test]
    fn the_method_list_shows_only_when_choosing() {
        let challenge = Challenge::NewDevice;
        let screen = |error| Screen::SecondFactor {
            email: "",
            challenge: &challenge,
            error,
            busy: false,
            sending: false,
        };
        assert_eq!(Route::of(Some(&screen(None)), |_| true), Route::Methods);
        assert_eq!(Route::of(Some(&screen(None)), |_| false), Route::SecondFactor);
    }
}
