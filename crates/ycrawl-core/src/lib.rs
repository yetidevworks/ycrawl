//! Stateless URL to markdown extraction.
//!
//! Nothing here touches the filesystem: a page is fetched, converted, and handed
//! back. Design decisions in this crate are grounded in `ycrawl-bench`, which
//! measures fetch success against bot-walled sites and output quality against
//! known-good pages.

pub mod browser;
pub mod clean;
pub mod extract;
pub mod fetch;
pub mod types;
pub mod verdict;

pub use browser::Browser;
pub use extract::{extract, extract_fetched, ExtractMode, ExtractOptions};
pub use fetch::{
    client, fetch, fetch_with_limit, Fetched, FetchedBody, Profile, DEFAULT_MAX_BYTES,
};
pub use types::{Attempt, ExtractPath, Meta, Page, Tier};
pub use verdict::{classify, is_interstitial, Escalation, Verdict, Wall};
