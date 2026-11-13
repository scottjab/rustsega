//! The shell's reusable components: buttons, drawer, toasts, form controls.
//!
//! Everything here is a real `<button>`, `<input>` or `<select>`, so keyboard
//! focus and the `:focus-visible` ring work without extra scripting. Visual
//! state lives in `style.css`; these components only set classes and ARIA
//! attributes.

pub mod button;
pub mod drawer;
pub mod hint;
pub mod icons;
pub mod select;
pub mod slider;
pub mod toast;
pub mod toggle;
