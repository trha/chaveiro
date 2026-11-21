//! The layout every screen shares: the form stack, the title, and the gap
//! between form groups.

use windows_core::Result;
use winui_immediate::{Orientation, Ui};

/// One screen's controls: a vertical stack of what `body` declares,
/// `space-2` apart, under the window's title bar.
pub fn form(ui: &mut Ui, id: &'static str, body: impl FnOnce(&mut Ui) -> Result<()>) -> Result<()> {
    ui.stack(id, Orientation::Vertical, 8.0, body)?
        .padding([24.0, 16.0, 24.0, 24.0])?;
    Ok(())
}

/// A screen's title, set apart from the controls under it like a form
/// group.
pub fn title(ui: &mut Ui, text: &str) -> Result<()> {
    ui.label("title", format_args!("{text}"))?
        .style("TitleTextBlockStyle")?
        .margin(GROUP_GAP_BELOW)?;
    Ok(())
}

/// Added to a form's 8px (`space-2`) spacing, these make the 16px
/// (`space-4`) gap between form groups.
pub const GROUP_GAP_BELOW: [f64; 4] = [0.0, 0.0, 0.0, 8.0];
pub const GROUP_GAP_ABOVE: [f64; 4] = [0.0, 8.0, 0.0, 0.0];
