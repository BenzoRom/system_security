// Copyright 2021, The Android Open Source Project
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Implements get_attestation_key_info which loads remote provisioned or user
//! generated attestation keys.

use crate::database::{BlobMetaData, KeyEntryLoadBits, KeyType};
use crate::database::{KeyIdGuard, KeystoreDB};
use crate::error::{Error, ErrorCode};
use crate::permission::KeyPerm;
use crate::remote_provisioning::RemProvState;
use crate::utils::check_key_permission;
use android_hardware_security_keymint::aidl::android::hardware::security::keymint::{
    AttestationKey::AttestationKey, Certificate::Certificate, KeyParameter::KeyParameter, Tag::Tag,
};
use android_system_keystore2::aidl::android::system::keystore2::{
    Domain::Domain, KeyDescriptor::KeyDescriptor,
};
use anyhow::{Context, Result};
use keystore2_crypto::parse_subject_from_certificate;

/// KeyMint takes two different kinds of attestation keys. Remote provisioned keys
/// and those that have been generated by the user. Unfortunately, they need to be
/// handled quite differently, thus the different representations.
pub enum AttestationKeyInfo {
    RemoteProvisioned {
        key_id_guard: KeyIdGuard,
        attestation_key: AttestationKey,
        attestation_certs: Certificate,
    },
    UserGenerated {
        key_id_guard: KeyIdGuard,
        blob: Vec<u8>,
        blob_metadata: BlobMetaData,
        issuer_subject: Vec<u8>,
    },
}

/// This function loads and, optionally, assigns the caller's remote provisioned
/// attestation key if a challenge is present. Alternatively, if `attest_key_descriptor` is given,
/// it loads the user generated attestation key from the database.
pub fn get_attest_key_info(
    key: &KeyDescriptor,
    caller_uid: u32,
    attest_key_descriptor: Option<&KeyDescriptor>,
    params: &[KeyParameter],
    rem_prov_state: &RemProvState,
    db: &mut KeystoreDB,
) -> Result<Option<AttestationKeyInfo>> {
    let challenge_present = params.iter().any(|kp| kp.tag == Tag::ATTESTATION_CHALLENGE);
    match attest_key_descriptor {
        None if challenge_present => rem_prov_state
            .get_remotely_provisioned_attestation_key_and_certs(key, caller_uid, params, db)
            .context(concat!(
                "In get_attest_key_and_cert_chain: ",
                "Trying to get remotely provisioned attestation key."
            ))
            .map(|result| {
                result.map(|(key_id_guard, attestation_key, attestation_certs)| {
                    AttestationKeyInfo::RemoteProvisioned {
                        key_id_guard,
                        attestation_key,
                        attestation_certs,
                    }
                })
            }),
        None => Ok(None),
        Some(attest_key) => get_user_generated_attestation_key(attest_key, caller_uid, db)
            .context("In get_attest_key_and_cert_chain: Trying to load attest key")
            .map(Some),
    }
}

fn get_user_generated_attestation_key(
    key: &KeyDescriptor,
    caller_uid: u32,
    db: &mut KeystoreDB,
) -> Result<AttestationKeyInfo> {
    let (key_id_guard, blob, cert, blob_metadata) =
        load_attest_key_blob_and_cert(key, caller_uid, db)
            .context("In get_user_generated_attestation_key: Failed to load blob and cert")?;

    let issuer_subject: Vec<u8> = parse_subject_from_certificate(&cert).context(
        "In get_user_generated_attestation_key: Failed to parse subject from certificate.",
    )?;

    Ok(AttestationKeyInfo::UserGenerated { key_id_guard, blob, issuer_subject, blob_metadata })
}

fn load_attest_key_blob_and_cert(
    key: &KeyDescriptor,
    caller_uid: u32,
    db: &mut KeystoreDB,
) -> Result<(KeyIdGuard, Vec<u8>, Vec<u8>, BlobMetaData)> {
    match key.domain {
        Domain::BLOB => Err(Error::Km(ErrorCode::INVALID_ARGUMENT)).context(
            "In load_attest_key_blob_and_cert: Domain::BLOB attestation keys not supported",
        ),
        _ => {
            let (key_id_guard, mut key_entry) = db
                .load_key_entry(
                    key,
                    KeyType::Client,
                    KeyEntryLoadBits::BOTH,
                    caller_uid,
                    |k, av| check_key_permission(KeyPerm::use_(), k, &av),
                )
                .context("In load_attest_key_blob_and_cert: Failed to load key.")?;

            let (blob, blob_metadata) =
                key_entry.take_key_blob_info().ok_or_else(Error::sys).context(concat!(
                    "In load_attest_key_blob_and_cert: Successfully loaded key entry,",
                    " but KM blob was missing."
                ))?;
            let cert = key_entry.take_cert().ok_or_else(Error::sys).context(concat!(
                "In load_attest_key_blob_and_cert: Successfully loaded key entry,",
                " but cert was missing."
            ))?;
            Ok((key_id_guard, blob, cert, blob_metadata))
        }
    }
}
