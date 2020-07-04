// Copyright 2020 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under The General Public License (GPL), version 3.
// Unless required by applicable law or agreed to in writing, the SAFE Network Software distributed
// under the GPL Licence is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied. Please review the Licences for the specific language governing
// permissions and limitations relating to use of the SAFE Network Software.

mod blob_register;
mod elder_stores;
mod map_storage;
mod reading;
mod sequence_storage;
mod writing;

use crate::{action::Action, node::Init, rpc::Rpc as Message, utils, Config, Result};
use blob_register::BlobRegister;
use elder_stores::ElderStores;
use map_storage::MapStorage;
use reading::Reading;
use routing::{Node, SrcLocation};
use sequence_storage::SequenceStorage;
use writing::Writing;

use log::{error, trace};
use safe_nd::{
    BlobRead, IDataAddress, MessageId, NodePublicId, NodeRequest, PublicId, Read, Request,
    Response, XorName,
};
use threshold_crypto::{PublicKey, Signature, SignatureShare};

use std::{
    cell::{Cell, RefCell},
    collections::BTreeSet,
    fmt::{self, Display, Formatter},
    rc::Rc,
};

pub(crate) struct Metadata {
    id: NodePublicId,
    elder_stores: ElderStores,
    routing_node: Rc<RefCell<Node>>,
}

impl Metadata {
    pub fn new(
        id: NodePublicId,
        config: &Config,
        total_used_space: &Rc<Cell<u64>>,
        init_mode: Init,
        routing_node: Rc<RefCell<Node>>,
    ) -> Result<Self> {
        let blob_register = BlobRegister::new(id.clone(), config, init_mode, routing_node.clone())?;
        let map_storage = MapStorage::new(id.clone(), config, total_used_space, init_mode)?;
        let sequence_storage =
            SequenceStorage::new(id.clone(), config, total_used_space, init_mode)?;
        let elder_stores = ElderStores::new(blob_register, map_storage, sequence_storage);
        Ok(Self {
            id,
            elder_stores,
            routing_node,
        })
    }

    pub fn receive_msg(
        &mut self,
        src: SrcLocation,
        msg: Message,
        accumulated_signature: Option<Signature>,
    ) -> Option<Action> {
        match msg {
            Message::Request {
                request,
                requester,
                message_id,
                ..
            } => self.handle_request(src, requester, request, message_id, accumulated_signature),
            Message::Response {
                response,
                requester,
                message_id,
                proof,
                ..
            } => self.handle_response(src, response, requester, message_id, proof),
            Message::Duplicate {
                address,
                holders,
                message_id,
                ..
            } => self.initiate_duplication(address, holders, message_id, accumulated_signature),
            Message::DuplicationComplete {
                response,
                message_id,
                proof: Some((idata_address, signature)),
            } => self.finalise_duplication(src, response, message_id, idata_address, signature),
            _ => None,
        }
    }

    // This should be called whenever a node leaves the section. It fetches the list of data that was
    // previously held by the node and requests the other holders to store an additional copy.
    // The list of holders is also updated by removing the node that left.
    pub fn trigger_chunk_duplication(&mut self, node: XorName) -> Option<Vec<Action>> {
        self.elder_stores.blob_register_mut().duplicate_chunks(node)
    }

    pub fn handle_request(
        &mut self,
        src: SrcLocation,
        requester: PublicId,
        request: Request,
        message_id: MessageId,
        accumulated_signature: Option<Signature>,
    ) -> Option<Action> {
        trace!(
            "{}: Received ({:?} {:?}) from src {:?} (client {:?})",
            self,
            request,
            message_id,
            src,
            requester
        );
        use NodeRequest::*;
        use Request::*;
        match request.clone() {
            Node(Read(read)) => {
                let reading = Reading::new(
                    read,
                    src,
                    requester,
                    request,
                    message_id,
                    accumulated_signature,
                    self.public_key(),
                );
                reading.get_result(&self.elder_stores)
            }
            Node(Write(write)) => {
                let mut writing = Writing::new(
                    write,
                    src,
                    requester,
                    request,
                    message_id,
                    accumulated_signature,
                    self.public_key(),
                );
                writing.get_result(&mut self.elder_stores)
            }
            _ => None,
        }
    }

    pub fn handle_response(
        &mut self,
        src: SrcLocation,
        response: Response,
        requester: PublicId,
        message_id: MessageId,
        proof: Option<(Request, Signature)>,
    ) -> Option<Action> {
        use Response::*;
        trace!(
            "{}: Received ({:?} {:?}) from {}",
            self,
            response,
            message_id,
            utils::get_source_name(src),
        );
        if let Some((request, signature)) = proof {
            if !matches!(requester, PublicId::Node(_))
                && self
                    .validate_section_signature(&request, &signature)
                    .is_none()
            {
                error!("Invalid section signature");
                return None;
            }
            match response {
                Write(result) => self.elder_stores.blob_register_mut().handle_write_result(
                    utils::get_source_name(src),
                    requester,
                    result,
                    message_id,
                    request,
                ),
                GetIData(result) => self.elder_stores.blob_register().handle_get_result(
                    result,
                    message_id,
                    requester,
                    (request, signature),
                ),
                //
                // ===== Invalid =====
                //
                ref _other => {
                    error!(
                        "{}: Should not receive {:?} as a data handler.",
                        self, response
                    );
                    None
                }
            }
        } else {
            error!("Missing section signature");
            None
        }
    }

    fn initiate_duplication(
        &mut self,
        address: IDataAddress,
        holders: BTreeSet<XorName>,
        message_id: MessageId,
        accumulated_signature: Option<Signature>,
    ) -> Option<Action> {
        trace!(
            "Sending GetIData request for address: ({:?}) to {:?}",
            address,
            holders,
        );
        let our_id = self.id.clone();
        Some(Action::SendToPeers {
            targets: holders,
            rpc: Message::Request {
                request: Request::Node(NodeRequest::Read(Read::Blob(BlobRead::Get(address)))),
                requester: PublicId::Node(our_id),
                message_id,
                signature: Some((0, SignatureShare(accumulated_signature?))),
            },
        })
    }

    fn finalise_duplication(
        &mut self,
        sender: SrcLocation,
        response: Response,
        message_id: MessageId,
        idata_address: IDataAddress,
        signature: Signature,
    ) -> Option<Action> {
        use Response::*;
        if self
            .routing_node
            .borrow()
            .public_key_set()
            .ok()?
            .public_key()
            .verify(&signature, &utils::serialise(&idata_address))
        {
            match response {
                Write(result) => self.elder_stores.blob_register_mut().update_holders(
                    idata_address,
                    utils::get_source_name(sender),
                    result,
                    message_id,
                ),
                // Duplication doesn't care about other type of responses
                ref _other => {
                    error!(
                        "{}: Should not receive {:?} as a data handler.",
                        self, response
                    );
                    None
                }
            }
        } else {
            error!("Ignoring duplication response. Invalid Signature.");
            None
        }
    }

    fn public_key(&self) -> Option<PublicKey> {
        Some(
            self.routing_node
                .borrow()
                .public_key_set()
                .ok()?
                .public_key(),
        )
    }

    fn validate_section_signature(&self, request: &Request, signature: &Signature) -> Option<()> {
        if self
            .public_key()?
            .verify(signature, &utils::serialise(request))
        {
            Some(())
        } else {
            None
        }
    }
}

impl Display for Metadata {
    fn fmt(&self, formatter: &mut Formatter) -> fmt::Result {
        write!(formatter, "{}", self.id.name())
    }
}
