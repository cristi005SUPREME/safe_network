// Copyright 2022 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under The General Public License (GPL), version 3.
// Unless required by applicable law or agreed to in writing, the SAFE Network Software distributed
// under the GPL Licence is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied. Please review the Licences for the specific language governing
// permissions and limitations relating to use of the SAFE Network Software.

mod load_monitoring;

use self::load_monitoring::{LoadMonitoring, INITIAL_MSGS_PER_S};

use std::sync::Arc;
use tokio::{sync::RwLock, time::Instant};

const SANITY_MAX_PER_S_AND_PEER: f64 = INITIAL_MSGS_PER_S;
const SANITY_MIN_PER_S_AND_PEER: f64 = 1.0; // 1 every s

type OutgoingReport = (Instant, f64);

#[derive(Clone)]
pub(crate) struct BackPressure {
    monitoring: LoadMonitoring,
    last_report: Arc<RwLock<Option<OutgoingReport>>>,
}

impl BackPressure {
    pub(crate) fn new() -> Self {
        Self {
            monitoring: LoadMonitoring::new(),
            last_report: Arc::new(RwLock::new(None)),
        }
    }

    pub(crate) fn count_msg(&self) {
        self.monitoring.count_msg();
    }

    /// Sent to nodes calling us, if the value has changed significantly.
    pub(crate) async fn tolerated_msgs_per_s(&self, sessions_count: usize) -> Option<f64> {
        let now = Instant::now();
        let tolerated_msgs_per_s = self.try_get_new_value(sessions_count, now).await;
        tolerated_msgs_per_s
    }

    async fn try_get_new_value(&self, sessions_count: usize, now: Instant) -> Option<f64> {
        let msgs_per_s = 10.0 * self.monitoring.msgs_per_s().await;
        let num_callers = sessions_count as f64;

        // avoid divide by 0 errors
        let msgs_per_s_and_peer = msgs_per_s / f64::max(1.0, num_callers);

        // make sure not more than sanity max
        let msgs_per_s_and_peer = f64::min(SANITY_MAX_PER_S_AND_PEER, msgs_per_s_and_peer);

        // make sure not less than sanity min
        let msgs_per_s_and_peer = f64::max(SANITY_MIN_PER_S_AND_PEER, msgs_per_s_and_peer);

        debug!("Number of callers {:?}", num_callers);
        debug!("Msgs per s and peer {:?}", msgs_per_s_and_peer);

        let prev = *self.last_report.read().await;

        // bound update rates by require some minimum level of change
        let significant_change = |change_ratio| {
            // if current val is 5 % worse, or 10 % better, then update our peer with it
            0.95 >= change_ratio || change_ratio >= 1.1 || change_ratio == 0.0
        };

        let (record_changes, worthwhile_reporting) = if let Some((_, previous)) = prev {
            let change_ratio = msgs_per_s_and_peer / previous;
            if significant_change(change_ratio) {
                // we want to store the value, and update the node
                (true, true)
            } else {
                debug!(
                    "No significant change of backpressure value (previous: {}, ratio: {})",
                    previous, change_ratio
                );
                // we neither want to store the value, nor update the sender
                (false, false)
            }
        } else {
            let change_ratio = msgs_per_s_and_peer / SANITY_MAX_PER_S_AND_PEER;
            if significant_change(change_ratio) {
                // we want to store the value, and update the node
                (true, true)
            } else {
                debug!(
                    "No significant change of sender default backpressure value (ratio: {})",
                    change_ratio
                );
                // we want to store the value, but not update the sender
                (true, false)
            }
        };

        if record_changes {
            debug!("Recording changes");
            *self.last_report.write().await = Some((now, msgs_per_s_and_peer));
        }

        if worthwhile_reporting {
            Some(msgs_per_s_and_peer)
        } else {
            None
        }
    }
}