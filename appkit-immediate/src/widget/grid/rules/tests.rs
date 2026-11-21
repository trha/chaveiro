use super::*;
use proptest::prelude::*;

proptest! {
    #[test]
    fn every_child_gets_one_top(rows in lengths(), extra in 0usize..3) {
        let children = rows.len().saturating_sub(extra);
        let rules = rules(&rows, children);
        for row in 0..children {
            let tops = rules.iter().filter(|rule| **rule == Rule::Top { row }).count();
            prop_assert_eq!(tops, 1);
        }
        let in_range = rules.iter().all(|rule| match rule {
            Rule::Top { row }
            | Rule::Bottom { row, .. }
            | Rule::Height { row, .. }
            | Rule::Share { row, .. }
            | Rule::Stretch { row } => *row < children,
        });
        prop_assert!(in_range, "a rule names a row past the last child");
    }

    #[test]
    fn the_last_child_alone_meets_the_bottom(rows in lengths()) {
        let rules = rules(&rows, rows.len());
        let bottoms: Vec<_> = rules.iter().filter_map(|rule| match rule {
            Rule::Bottom { row, exact } => Some((*row, *exact)),
            _ => None,
        }).collect();
        if rows.is_empty() {
            prop_assert!(bottoms.is_empty());
        } else {
            let any_star = rows.iter().any(|row| matches!(row, Length::Star(_)));
            prop_assert_eq!(bottoms, vec![(rows.len() - 1, any_star)]);
        }
    }

    #[test]
    fn exactly_the_star_rows_stretch(rows in lengths()) {
        let rules = rules(&rows, rows.len());
        for (row, length) in rows.iter().enumerate() {
            let stretches = rules.iter().filter(|rule| **rule == Rule::Stretch { row }).count();
            prop_assert_eq!(stretches, usize::from(matches!(length, Length::Star(_))));
        }
    }

    #[test]
    fn auto_rows_get_no_height(rows in lengths()) {
        let rules = rules(&rows, rows.len());
        for (row, _) in rows.iter().enumerate().filter(|(_, length)| **length == Length::Auto) {
            prop_assert!(height_of(&rules, &rows, row, 1.0).is_none());
        }
    }

    #[test]
    fn pixel_rows_get_their_height(rows in lengths(), unit in 1.0f64..500.0) {
        let rules = rules(&rows, rows.len());
        for (row, length) in rows.iter().enumerate() {
            if let Length::Pixels(points) = length {
                prop_assert_eq!(height_of(&rules, &rows, row, unit), Some(*points));
            }
        }
    }

    #[test]
    fn star_rows_split_space_by_weight(rows in lengths(), unit in 1.0f64..500.0) {
        let rules = rules(&rows, rows.len());
        // Read the heights back for one unit height, then compare every pair
        // of star rows against the ratio of their weights.
        let stars: Vec<(f64, f64)> = rows.iter().enumerate().filter_map(|(row, length)| match length {
            Length::Star(weight) => Some((*weight, height_of(&rules, &rows, row, unit).expect("star rows have a height"))),
            _ => None,
        }).collect();
        for (weight_a, height_a) in &stars {
            for (weight_b, height_b) in &stars {
                // height_a / height_b == weight_a / weight_b, cross-multiplied
                // so zero weights need no division.
                let left = height_a * weight_b;
                let right = height_b * weight_a;
                prop_assert!((left - right).abs() <= 1e-6 * left.abs().max(right.abs()).max(1.0));
            }
        }
    }
}

/// The height `rules` give `row` when the unit row is `unit` points tall, or
/// [`None`] for a row sized by its content. A star row with no sizing rule
/// is the unit itself.
fn height_of(rules: &[Rule], rows: &[Length], row: usize, unit: f64) -> Option<f64> {
    let sized = rules.iter().find_map(|rule| match *rule {
        Rule::Height {
            row: target,
            points,
        } if target == row => Some(points),
        Rule::Share {
            row: target, ratio, ..
        } if target == row => Some(ratio * unit),
        _ => None,
    });
    sized.or(matches!(rows[row], Length::Star(_)).then_some(unit))
}

fn lengths() -> impl Strategy<Value = Vec<Length>> {
    prop::collection::vec(
        prop_oneof![
            Just(Length::Auto),
            (0.0f64..400.0).prop_map(Length::Pixels),
            prop_oneof![Just(0.0), 0.1f64..8.0].prop_map(Length::Star),
        ],
        0..8,
    )
}
