// Copyright 2022 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under The General Public License (GPL), version 3.
// Unless required by applicable law or agreed to in writing, the SAFE Network Software distributed
// under the GPL Licence is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied. Please review the Licences for the specific language governing
// permissions and limitations relating to use of the SAFE Network Software.

//! Module providing keys, keypairs, and signatures.
//!
//! The easiest way to get a `PublicKey` is to create a random `Keypair` first through one of the
//! `new` functions. A `PublicKey` can't be generated by itself; it must always be derived from a
//! secret key.

pub mod ed25519;
pub(super) mod keypair;
pub(crate) mod public_key;
pub(super) mod secret_key;
pub(super) mod signature;

#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils {
    use crate::{
        messaging::system::{SectionSig, SectionSigned},
        network_knowledge::{SectionAuthUtils, SectionKeyShare},
    };
    use bls::{blstrs::Scalar, poly::Poly, SecretKey, SecretKeySet, Signature};
    use eyre::eyre;
    use serde::Serialize;
    use std::collections::{BTreeMap, BTreeSet};

    /// bls key related test utilities
    pub struct TestKeys {}

    impl TestKeys {
        /// Create `bls::Signature` for the given payload using the provided `bls::SecretKey`
        pub fn sign<T: Serialize>(secret_key: &SecretKey, payload: &T) -> Signature {
            let bytes = bincode::serialize(payload).expect("Failed to serialize payload");
            Self::sign_bytes(secret_key, &bytes)
        }

        /// Create `bls::Signature` for the given bytes using the provided `bls::SecretKey`
        pub fn sign_bytes(secret_key: &SecretKey, bytes: &[u8]) -> Signature {
            secret_key.sign(bytes)
        }

        /// Create `SectionSig` for the given bytes using the provided `bls::SecretKey`
        pub fn get_section_sig_bytes(secret_key: &SecretKey, bytes: &[u8]) -> SectionSig {
            SectionSig {
                public_key: secret_key.public_key(),
                signature: Self::sign_bytes(secret_key, bytes),
            }
        }

        /// Create `SectionSig` for the given payload using the provided `bls::SecretKey`
        pub fn get_section_sig<T: Serialize>(secret_key: &SecretKey, payload: &T) -> SectionSig {
            let bytes = bincode::serialize(payload).expect("Failed to serialize payload");
            Self::get_section_sig_bytes(secret_key, &bytes)
        }

        /// Create signature for the given payload using the provided `bls::SecretKey` and
        /// wrap them using `SectionSigned`
        pub fn get_section_signed<T: Serialize>(
            secret_key: &SecretKey,
            payload: T,
        ) -> SectionSigned<T> {
            let sig = Self::get_section_sig(secret_key, &payload);
            SectionSigned::new(payload, sig)
        }

        /// Generate a `SectionKeyShare` from the `bls::SecretKeySet` and given index
        pub fn get_section_key_share(sk_set: &SecretKeySet, index: usize) -> SectionKeyShare {
            SectionKeyShare {
                public_key_set: sk_set.public_keys(),
                index,
                secret_key_share: sk_set.secret_key_share(index),
            }
        }

        /// Create `bls::SecretKeySet` from the provided set of `SecretKeyShare` if
        /// we provide n shares, where n > threshold + 1.
        pub fn get_sk_set_from_shares(
            section_key_shares: &[SectionKeyShare],
        ) -> eyre::Result<SecretKeySet> {
            // need to first get the pub_key_set to calulcate the threshold
            let pub_key_set = section_key_shares
                .iter()
                .map(|share| share.public_key_set.clone())
                .collect::<BTreeSet<_>>();

            if pub_key_set.len() != 1 {
                return Err(eyre!("Found multiple pub_key_sets for given set of shares"));
            }
            let pub_key_set = pub_key_set
                .into_iter()
                .next()
                .ok_or_else(|| eyre!("1 element is present"))?;

            let secrets: BTreeMap<_, _> = section_key_shares
                .iter()
                .map(|share| {
                    let bytes = share.secret_key_share.to_bytes();
                    // cannot be mapped to Err
                    let fr = Scalar::from_bytes_be(&bytes).unwrap();
                    // share index + 1 gives us the polynomial coefficient index
                    (share.index + 1, fr)
                })
                .collect();

            // we need threshold + 1 unique shares
            if secrets.len() <= pub_key_set.threshold() {
                return Err(eyre!(
                    "We need {} unique SectionKeyShare, we got {}",
                    pub_key_set.threshold() + 1,
                    secrets.len()
                ));
            }

            // we need exactly threshold+1 shares to get the Poly, else error
            let secrets: BTreeMap<_, _> = secrets
                .into_iter()
                .take(pub_key_set.threshold() + 1)
                .collect();

            // throws duplicated index if we have more shares than threshold + 1
            let sk_set = SecretKeySet::from(Poly::interpolate(secrets)?);

            Ok(sk_set)
        }
    }

    #[test]
    fn obtain_sk_set_from_shares() -> eyre::Result<()> {
        let sks = SecretKeySet::random(3, &mut bls::rand::thread_rng());

        // > threshold shares are needed to get the sk_set
        // index can be any number
        let sk_share_set = Vec::from([
            TestKeys::get_section_key_share(&sks, 3),
            TestKeys::get_section_key_share(&sks, 4),
            TestKeys::get_section_key_share(&sks, 5),
            TestKeys::get_section_key_share(&sks, 6),
        ]);
        assert_eq!(
            TestKeys::get_sk_set_from_shares(&sk_share_set)?.to_bytes(),
            sks.to_bytes()
        );

        // <= threshold will not produce an sk_set
        let sk_share_set = Vec::from([
            TestKeys::get_section_key_share(&sks, 3),
            TestKeys::get_section_key_share(&sks, 4),
            TestKeys::get_section_key_share(&sks, 5),
        ]);
        assert!(TestKeys::get_sk_set_from_shares(&sk_share_set).is_err());

        // <= threshold will not produce an sk_set; even if #of shares > threshold
        let sk_share_set = Vec::from([
            TestKeys::get_section_key_share(&sks, 3),
            TestKeys::get_section_key_share(&sks, 4),
            TestKeys::get_section_key_share(&sks, 4),
            TestKeys::get_section_key_share(&sks, 5),
        ]);
        assert!(TestKeys::get_sk_set_from_shares(&sk_share_set).is_err());

        // >> threshold shares is still valid
        let sk_share_set = Vec::from([
            TestKeys::get_section_key_share(&sks, 3),
            TestKeys::get_section_key_share(&sks, 4),
            TestKeys::get_section_key_share(&sks, 5),
            TestKeys::get_section_key_share(&sks, 6),
            TestKeys::get_section_key_share(&sks, 7),
        ]);
        assert_eq!(
            TestKeys::get_sk_set_from_shares(&sk_share_set)?.to_bytes(),
            sks.to_bytes()
        );

        // >> threshold with reapeated shares is also valid
        let sk_share_set = Vec::from([
            TestKeys::get_section_key_share(&sks, 3),
            TestKeys::get_section_key_share(&sks, 3),
            TestKeys::get_section_key_share(&sks, 4),
            TestKeys::get_section_key_share(&sks, 5),
            TestKeys::get_section_key_share(&sks, 6),
        ]);
        assert_eq!(
            TestKeys::get_sk_set_from_shares(&sk_share_set)?.to_bytes(),
            sks.to_bytes()
        );
        Ok(())
    }
}
