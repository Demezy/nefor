mod client;
mod request;

pub use client::{WebClient, WebError};
pub use request::{
    AllowedCaller, ClickOperation, FindOperation, OpenOperation, ProviderErrorEnvelope,
    ScreenshotOperation, SearchEndpointResponse, SearchInput, SearchQuery, SearchRequest,
    SearchResponse, SearchSettings, WebCommand,
};
