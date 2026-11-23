use super::when;
use jiff::{Timestamp, tz::TimeZone};

#[test]
fn a_time_shows_in_this_computers_zone() {
    let iso = "2026-10-14T07:12:00.000Z";
    let local = iso
        .parse::<Timestamp>()
        .unwrap()
        .to_zoned_datetime(TimeZone::system());
    let text = when(iso);
    // Day, short month, year, then the local hour and minute.
    assert!(text.starts_with(&format!("{} ", local.day())), "{text}");
    assert!(text.contains(" Oct 2026, "), "{text}");
    assert!(
        text.ends_with(&format!("{:02}:{:02}", local.hour(), local.minute())),
        "{text}"
    );
}

#[test]
fn text_that_is_no_time_shows_as_given() {
    assert_eq!(when("yesterday"), "yesterday");
}
