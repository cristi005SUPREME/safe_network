// Copyright 2022 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under The General Public License (GPL), version 3.
// Unless required by applicable law or agreed to in writing, the SAFE Network Software distributed
// under the GPL Licence is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied. Please review the Licences for the specific language governing
// permissions and limitations relating to use of the SAFE Network Software.

use crate::node::{core::Proposal, XorName};

use sn_consensus::Generation;
use sn_interface::{
    messaging::{
        system::{DkgFailureSigSet, KeyedSig, NodeState, SectionAuth, SystemMsg},
        DstLocation, WireMsg,
    },
    network_knowledge::{SectionAuthorityProvider, SectionKeyShare},
    types::{Peer, ReplicatedDataAddress},
};

use bytes::Bytes;
use custom_debug::Debug;
use std::{
    collections::BTreeSet,
    fmt,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime},
};

/// A struct for the job of controlling the flow
/// of a [`Cmd`] in the system.
///
/// An id is assigned to it, a priority by which it is
/// ordered in the queue among other pending cmd jobs,
/// and the time the job was instantiated.
///
/// todo: take parent id
#[derive(Debug, Clone)]
pub struct CmdJob {
    id: u64, // Consider use of subcmd id e.g. parent "963111461", child "963111461.0"
    cmd: Cmd,
    priority: i32,
    created_at: SystemTime,
}

impl CmdJob {
    pub(crate) fn new(id: u64, cmd: Cmd, created_at: SystemTime) -> Self {
        let priority = cmd.priority();
        Self {
            id,
            cmd,
            priority,
            created_at,
        }
    }

    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    pub(crate) fn cmd(&self) -> &Cmd {
        &self.cmd
    }

    pub(crate) fn priority(&self) -> i32 {
        self.priority
    }

    pub(crate) fn created_at(&self) -> SystemTime {
        self.created_at
    }
}

/// Internal cmds for a node.
///
/// Cmds are used to connect different modules, allowing
/// for a better modularization of the code base.
/// Modelling a call like this also allows for throttling
/// and prioritization, which is not something e.g. tokio tasks allow.
/// In other words, it enables enhanced flow control.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub(crate) enum Cmd {
    /// Cleanup node's PeerLinks, removing any unsused, unconnected peers
    CleanupPeerLinks,
    /// Handle `message` from `sender`.
    /// Holding the WireMsg that has been received from the network,
    HandleMsg {
        sender: Peer,
        wire_msg: WireMsg,
        #[debug(skip)]
        // original bytes to avoid reserializing for entropy checks
        original_bytes: Option<Bytes>,
    },
    /// Handle a timeout previously scheduled with `ScheduleDkgTimeout`.
    HandleDkgTimeout(u64),
    /// Handle peer that's been detected as lost.
    HandlePeerLost(Peer),
    /// Handle agreement on a proposal.
    HandleAgreement { proposal: Proposal, sig: KeyedSig },
    /// Handle a new Node joining agreement.
    HandleNewNodeOnline(SectionAuth<NodeState>),
    /// Handle a Node leaving agreement.
    HandleNodeLeft(SectionAuth<NodeState>),
    /// Handle agree on elders. This blocks node message processing until complete.
    HandleNewEldersAgreement { proposal: Proposal, sig: KeyedSig },
    /// Handle the outcome of a DKG session where we are one of the participants (that is, one of
    /// the proposed new elders).
    HandleDkgOutcome {
        section_auth: SectionAuthorityProvider,
        outcome: SectionKeyShare,
        generation: Generation,
    },
    /// Handle a DKG failure that was observed by a majority of the DKG participants.
    HandleDkgFailure(DkgFailureSigSet),
    /// Send a message to the given `recipients`.
    SendMsg {
        recipients: Vec<Peer>,
        wire_msg: WireMsg,
    },
    /// Send the batch of data messages in a throttled/controlled fashion to the given `recipients`.
    /// chunks addresses are provided, so that we only retrieve the data right before we send it,
    /// hopefully reducing memory impact or data replication
    EnqueueDataForReplication {
        // throttle_duration: Duration,
        recipient: Peer,
        /// Batches of ReplicatedDataAddress to be sent together
        data_batch: Vec<ReplicatedDataAddress>,
    },
    /// Performs serialisation and signing for sending of NodeMsg.
    /// This cmd only send this to other nodes
    SignOutgoingSystemMsg { msg: SystemMsg, dst: DstLocation },
    /// Send a message to `delivery_group_size` peers out of the given `recipients`.
    SendMsgDeliveryGroup {
        recipients: Vec<Peer>,
        delivery_group_size: usize,
        wire_msg: WireMsg,
    },
    /// Schedule a timeout after the given duration. When the timeout expires, a `HandleDkgTimeout`
    /// cmd is raised. The token is used to identify the timeout.
    ScheduleDkgTimeout { duration: Duration, token: u64 },
    /// Proposes peers as offline
    ProposeOffline(BTreeSet<XorName>),
    /// Send a signal to all Elders to
    /// test the connectivity to a specific node
    StartConnectivityTest(XorName),
    /// Test Connectivity
    TestConnectivity(XorName),
}

impl Cmd {
    /// The priority of the cmd
    pub(crate) fn priority(&self) -> i32 {
        use Cmd::*;
        match self {
            HandleAgreement { .. } => 10,
            HandleNewEldersAgreement { .. } => 10,
            HandleDkgOutcome { .. } => 10,
            HandleDkgFailure(_) => 10,
            HandlePeerLost(_) => 10,
            HandleNodeLeft(_) => 10,
            ProposeOffline(_) => 10,

            HandleDkgTimeout(_) => 9,
            HandleNewNodeOnline(_) => 9,
            EnqueueDataForReplication { .. } => 9,

            ScheduleDkgTimeout { .. } => 8,
            StartConnectivityTest(_) => 8,
            TestConnectivity(_) => 8,

            // See [`MsgType`] for the priority constants and the range of possible values.
            HandleMsg { wire_msg, .. } => wire_msg.priority(),
            SendMsg { wire_msg, .. } => wire_msg.priority(),
            SignOutgoingSystemMsg { msg, .. } => msg.priority(),
            SendMsgDeliveryGroup { wire_msg, .. } => wire_msg.priority(),

            CleanupPeerLinks => -10,
        }
    }
}

impl fmt::Display for Cmd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Cmd::CleanupPeerLinks => {
                write!(f, "CleanupPeerLinks")
            }
            Cmd::HandleDkgTimeout(_) => write!(f, "HandleDkgTimeout"),
            Cmd::ScheduleDkgTimeout { .. } => write!(f, "ScheduleDkgTimeout"),
            #[cfg(not(feature = "test-utils"))]
            Cmd::HandleMsg { wire_msg, .. } => {
                write!(f, "HandleMsg {:?}", wire_msg.msg_id())
            }
            #[cfg(feature = "test-utils")]
            Cmd::HandleMsg { wire_msg, .. } => {
                write!(
                    f,
                    "HandleMsg {:?} {:?}",
                    wire_msg.msg_id(),
                    wire_msg.payload_debug
                )
            }
            Cmd::HandlePeerLost(peer) => write!(f, "HandlePeerLost({:?})", peer.name()),
            Cmd::HandleAgreement { .. } => write!(f, "HandleAgreement"),
            Cmd::HandleNewEldersAgreement { .. } => write!(f, "HandleNewEldersAgreement"),
            Cmd::HandleNewNodeOnline(_) => write!(f, "HandleNewNodeOnline"),
            Cmd::HandleNodeLeft(_) => write!(f, "HandleNodeLeft"),
            Cmd::HandleDkgOutcome { .. } => write!(f, "HandleDkgOutcome"),
            Cmd::HandleDkgFailure(_) => write!(f, "HandleDkgFailure"),
            #[cfg(not(feature = "test-utils"))]
            Cmd::SendMsg { wire_msg, .. } => {
                write!(f, "SendMsg {:?}", wire_msg.msg_id())
            }
            #[cfg(feature = "test-utils")]
            Cmd::SendMsg { wire_msg, .. } => {
                write!(
                    f,
                    "SendMsg {:?} {:?}",
                    wire_msg.msg_id(),
                    wire_msg.payload_debug
                )
            }
            Cmd::SignOutgoingSystemMsg { .. } => write!(f, "SignOutgoingSystemMsg"),
            Cmd::EnqueueDataForReplication { .. } => write!(f, "ThrottledSendBatchMsgs"),
            #[cfg(not(feature = "test-utils"))]
            Cmd::SendMsgDeliveryGroup { wire_msg, .. } => {
                write!(f, "SendMsgDeliveryGroup {:?}", wire_msg.msg_id())
            }
            #[cfg(feature = "test-utils")]
            Cmd::SendMsgDeliveryGroup { wire_msg, .. } => {
                write!(
                    f,
                    "SendMsg {:?} {:?}",
                    wire_msg.msg_id(),
                    wire_msg.payload_debug
                )
            }
            Cmd::ProposeOffline(_) => write!(f, "ProposeOffline"),
            Cmd::StartConnectivityTest(_) => write!(f, "StartConnectivityTest"),
            Cmd::TestConnectivity(_) => write!(f, "TestConnectivity"),
        }
    }
}

/// Generate unique timer token.
pub(crate) fn next_timer_token() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}