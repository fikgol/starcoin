// Copyright (c) The Starcoin Core Contributors
// SPDX-License-Identifier: Apache-2.0

use crate::messages::PeerMessage;
use anyhow::*;
use async_trait::async_trait;
use network_rpc_core::RawRpcClient;
use std::borrow::Cow;

pub mod messages;
mod peer_message_handler;
mod peer_provider;

pub use network_p2p_types::Multiaddr;
pub use network_p2p_types::MultiaddrWithPeerId;
pub use peer_message_handler::PeerMessageHandler;
pub use peer_provider::{PeerProvider, PeerSelector};
pub use starcoin_types::peer_info::{PeerId, PeerInfo};

#[async_trait]
pub trait NetworkService:
    Send + Sync + Clone + Sized + std::marker::Unpin + RawRpcClient + PeerProvider
{
    async fn send_peer_message(
        &self,
        protocol_name: Cow<'static, str>,
        peer_id: PeerId,
        msg: PeerMessage,
    ) -> Result<()>;
}
