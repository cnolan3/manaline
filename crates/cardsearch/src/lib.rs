//! Card search (§4.5): a query language deliberately close to Scryfall's,
//! over an index built from the Scryfall cache (every Oracle card) joined
//! with the engine's card set (which of them are implemented).
//!
//! ```text
//! t:creature c:r mv<=2 o:"draw a card" is:implemented
//! kw:flying -t:creature or t:angel
//! ```

pub mod index;
pub mod query;

pub use index::{Entry, Index};
pub use query::{parse, Cmp, Query, QueryError, Term};
