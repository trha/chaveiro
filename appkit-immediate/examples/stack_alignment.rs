//! Spike: does `NSStackView` with alignment `NotAnAttribute` add no
//! cross-axis constraints, so each child can carry its own? Prints the
//! frames of three labels pinned start, center and stretch in a 300pt wide
//! vertical stack, and whether layout is ambiguous.

// The program is macOS only. On other targets it is empty, so it checks but
// does not link.
#![cfg_attr(not(target_os = "macos"), no_main)]
#![cfg(target_os = "macos")]

use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSLayoutAttribute, NSStackView, NSTextField, NSUserInterfaceLayoutOrientation,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

fn main() {
    let mtm = MainThreadMarker::new().expect("main thread");
    let stack = NSStackView::new(mtm);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    stack.setAlignment(NSLayoutAttribute::NotAnAttribute);
    stack.setFrame(NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(300.0, 200.0),
    ));
    let labels: Vec<_> = ["start", "center", "stretch"]
        .iter()
        .map(|text| {
            let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
            label.setTranslatesAutoresizingMaskIntoConstraints(false);
            stack.addArrangedSubview(&label);
            label
        })
        .collect();
    labels[0]
        .leadingAnchor()
        .constraintEqualToAnchor(&stack.leadingAnchor())
        .setActive(true);
    labels[1]
        .centerXAnchor()
        .constraintEqualToAnchor(&stack.centerXAnchor())
        .setActive(true);
    labels[2]
        .leadingAnchor()
        .constraintEqualToAnchor(&stack.leadingAnchor())
        .setActive(true);
    labels[2]
        .trailingAnchor()
        .constraintEqualToAnchor(&stack.trailingAnchor())
        .setActive(true);
    stack.layoutSubtreeIfNeeded();
    println!("stack constraints {}", stack.constraints().count());
    for label in &labels {
        let frame = label.frame();
        println!(
            "{} x {} width {} ambiguous {}",
            label.stringValue(),
            frame.origin.x,
            frame.size.width,
            label.hasAmbiguousLayout()
        );
    }
}
