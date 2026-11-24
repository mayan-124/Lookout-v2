//! The Office-text helper, a Lookout plugin: it receives the bytes of a
//! .docx, .xlsx, .pptx or .pdf file over the plugin protocol and returns its text.
//! It opens no files and keeps nothing.

pub mod extract;
pub mod handler;
pub mod pdf;
