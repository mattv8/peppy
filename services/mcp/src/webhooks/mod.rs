//! Pure building blocks for outbound webhook delivery.

pub mod egress;
pub mod engine;
pub mod filter;
pub mod name;
pub mod payload;
pub mod signing;
pub mod store;

// W2b appends `store` and `engine` modules here.

pub use egress::{UrlError, is_blocked_ip, resolve_checked, validate_url};
pub use filter::{FilterEntryRef, FilterMode, matches};
pub use name::derive_name;
pub use payload::{AttachmentMeta, delivery_id, message_payload, random_delivery_id, test_payload};
pub use signing::{SigningError, generate_secret, sign};
