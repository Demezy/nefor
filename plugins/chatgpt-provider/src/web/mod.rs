mod client;
mod request;

pub use client::{WebClient, WebError};
pub use request::{
    AllowedCaller, ClickOperation, FindOperation, OpenOperation, ScreenshotOperation, SearchInput,
    SearchQuery, SearchRequest, SearchResponse, SearchSettings, WebCommand,
};
