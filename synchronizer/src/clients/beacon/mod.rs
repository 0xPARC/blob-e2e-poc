// MIT License Copyright (c) 2022 Blobscan <https://blobscan.com>
//
// Permission is hereby granted, free of charge,
// to any person obtaining a copy of this software and associated documentation
// files (the "Software"), to deal in the Software without restriction, including
// without limitation the rights to use, copy, modify, merge, publish, distribute,
// sublicense, and/or sell copies of the Software, and to permit persons to whom the
// Software is furnished to do so, subject to the following conditions:
//
// The above
// copyright notice and this permission notice (including the next paragraph) shall
// be included in all copies or substantial portions of the Software.
//
// THE SOFTWARE
// IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING
// BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR
// PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS
// BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF
// CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE
// SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

pub mod types;

use std::fmt::Debug;

use anyhow::Context as AnyhowContext;
use async_trait::async_trait;
use backoff::ExponentialBackoff;

use reqwest::{Client, Url};
use reqwest_eventsource::EventSource;

use types::BlockHeader;

use crate::{
    clients::{
        beacon::types::{BlockHeaderResponse, Spec, SpecResponse},
        common::ClientResult,
    },
    json_get,
};

use self::types::{Blob, BlobsResponse, Block, BlockId, BlockResponse, Topic};

#[derive(Debug, Clone)]
pub struct BeaconClient {
    base_url: Url,
    client: Client,
    exp_backoff: Option<ExponentialBackoff>,
}

pub struct Config {
    pub base_url: String,
    pub exp_backoff: Option<ExponentialBackoff>,
}

#[async_trait]
pub trait CommonBeaconClient: Send + Sync + Debug {
    async fn get_block(&self, block_id: BlockId) -> ClientResult<Option<Block>>;
    async fn get_block_header(&self, block_id: BlockId) -> ClientResult<Option<BlockHeader>>;
    async fn get_blobs(&self, block_id: BlockId) -> ClientResult<Option<Vec<Blob>>>;
    async fn get_spec(&self) -> ClientResult<Option<Spec>>;
    fn subscribe_to_events(&self, topics: &[Topic]) -> ClientResult<EventSource>;
}

impl BeaconClient {
    pub fn try_with_client(client: Client, config: Config) -> ClientResult<Self> {
        let base_url = Url::parse(&format!("{}/eth/", config.base_url))
            .with_context(|| "Failed to parse base URL")?;
        let exp_backoff = config.exp_backoff;

        Ok(Self {
            base_url,
            client,
            exp_backoff,
        })
    }
}

#[async_trait]
impl CommonBeaconClient for BeaconClient {
    async fn get_block(&self, block_id: BlockId) -> ClientResult<Option<Block>> {
        let path = format!("v2/beacon/blocks/{}", { block_id.to_detailed_string() });
        let url = self.base_url.join(path.as_str())?;

        json_get!(&self.client, url, BlockResponse, self.exp_backoff.clone())
            .map(|res| res.map(|r| r.into()))
    }

    async fn get_block_header(&self, block_id: BlockId) -> ClientResult<Option<BlockHeader>> {
        let path = format!("v1/beacon/headers/{}", { block_id.to_detailed_string() });
        let url = self.base_url.join(path.as_str())?;

        json_get!(
            &self.client,
            url,
            BlockHeaderResponse,
            self.exp_backoff.clone()
        )
        .map(|res| res.map(|r| r.into()))
    }

    async fn get_blobs(&self, block_id: BlockId) -> ClientResult<Option<Vec<Blob>>> {
        let path = format!("v1/beacon/blob_sidecars/{}", {
            block_id.to_detailed_string()
        });
        let url = self.base_url.join(path.as_str())?;

        json_get!(&self.client, url, BlobsResponse, self.exp_backoff.clone()).map(|res| match res {
            Some(r) => Some(r.data),
            None => None,
        })
    }

    async fn get_spec(&self) -> ClientResult<Option<Spec>> {
        let url = self.base_url.join("v1/config/spec")?;

        json_get!(&self.client, url, SpecResponse, self.exp_backoff.clone()).map(|res| match res {
            Some(r) => Some(r.data),
            None => None,
        })
    }

    fn subscribe_to_events(&self, topics: &[Topic]) -> ClientResult<EventSource> {
        let topics = topics
            .iter()
            .map(|topic| topic.into())
            .collect::<Vec<String>>()
            .join(",");
        let path = format!("v1/events?topics={topics}");
        let url = self.base_url.join(&path)?;

        Ok(EventSource::get(url))
    }
}
